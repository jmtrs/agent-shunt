use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{Duration, Instant},
};

use agent_shunt::{
    adapters::{
        embedding_index::EmbeddingIndex,
        filesystem::SecureFilesystem,
        git::GitChangeSource,
        local_embedding::LocalEmbedder,
        ripgrep::RipgrepSearch,
    },
    application::{
        ports::DenseIndex,
        retrieve::{
            MAX_BLOCK_LINES, MIN_SCORE_PERCENT, MMR_LAMBDA, RetrieveInput, execute_with_resolver,
        },
    },
    domain::{Limits, RetrievedChunk},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[cfg(feature = "ast")]
use agent_shunt::adapters::tree_sitter_chunker::AstResolver;
#[cfg(not(feature = "ast"))]
use agent_shunt::application::resolver::HeuristicResolver;

const MODEL: &str = "bge-small-en-v1.5";
const MODEL_TAG: &str = "local:bge-small-en-v1.5";
const INDEX_TOP_K: usize = 24;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    version: u32,
    name: String,
    repository: RepositorySpec,
    budget_tokens: usize,
    context_lines: usize,
    max_hits: usize,
    #[serde(default)]
    globs: Vec<String>,
    ks: Vec<usize>,
    cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepositorySpec {
    url: String,
    commit: String,
}

#[derive(Debug, Deserialize)]
struct EvalCase {
    id: String,
    question: String,
    expected: Vec<ExpectedEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpectedEvidence {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    strategy: String,
    cases: usize,
    hit_at: BTreeMap<usize, f64>,
    mrr: f64,
    avg_tokens: f64,
    avg_latency_ms: f64,
    p95_latency_ms: f64,
    results: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseResult {
    id: String,
    first_relevant_rank: Option<usize>,
    delivered_tokens: usize,
    chunks: usize,
    latency_ms: f64,
}

fn main() -> Result<ExitCode> {
    let (corpus_path, root, json_output) = parse_args()?;
    let raw = fs::read_to_string(&corpus_path)
        .with_context(|| format!("cannot read corpus {}", corpus_path.display()))?;
    let corpus: Corpus = serde_json::from_str(&raw)
        .with_context(|| format!("invalid corpus {}", corpus_path.display()))?;
    validate_corpus(&corpus)?;

    let root = fs::canonicalize(&root)
        .with_context(|| format!("cannot resolve repository root {}", root.display()))?;
    verify_repository(&root, &corpus.repository)?;

    #[cfg(feature = "ast")]
    let resolver = AstResolver::default();
    #[cfg(not(feature = "ast"))]
    let resolver = HeuristicResolver;

    let embedder = LocalEmbedder::new(MODEL)?;
    let cache_dir = env::temp_dir()
        .join("agent-shunt-semantic-eval")
        .join(&corpus.repository.commit);
    let index = EmbeddingIndex::new(
        &embedder,
        &resolver,
        cache_dir,
        MODEL_TAG,
        INDEX_TOP_K,
        MAX_BLOCK_LINES,
    );

    let lexical = run_strategy(&root, &corpus, &resolver, None, "lexical")?;
    let semantic = run_strategy(
        &root,
        &corpus,
        &resolver,
        Some(&index as &dyn DenseIndex),
        "semantic",
    )?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": corpus.version,
                "corpus": corpus.name,
                "repository": corpus.repository,
                "embeddingModel": MODEL,
                "indexTopK": INDEX_TOP_K,
                "reports": [lexical, semantic],
            }))?
        );
    } else {
        println!("corpus: {} ({} cases)", corpus.name, corpus.cases.len());
        println!(
            "repository: {} @ {}",
            corpus.repository.url,
            &corpus.repository.commit[..12]
        );
        println!("semantic model: {MODEL}; dense top-k: {INDEX_TOP_K}");
        print_report(&corpus, &lexical);
        print_report(&corpus, &semantic);
    }

    Ok(ExitCode::SUCCESS)
}

fn parse_args() -> Result<(PathBuf, PathBuf, bool)> {
    let mut corpus_path = None;
    let mut root = None;
    let mut json_output = false;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--root" => {
                let value = args.next().context("--root requires a path")?;
                root = Some(PathBuf::from(value));
            }
            _ if arg.starts_with('-') => bail!("unknown option {arg}"),
            _ => {
                if corpus_path.is_some() {
                    bail!("only one corpus path may be provided");
                }
                corpus_path = Some(PathBuf::from(arg));
            }
        }
    }

    Ok((
        corpus_path.unwrap_or_else(|| PathBuf::from("eval/corpus.json")),
        root.unwrap_or(env::current_dir().context("cannot resolve current directory")?),
        json_output,
    ))
}

fn validate_corpus(corpus: &Corpus) -> Result<()> {
    if corpus.version != 1 {
        bail!("unsupported corpus version {}", corpus.version);
    }
    if corpus.cases.is_empty() {
        bail!("corpus contains no cases");
    }
    if corpus.ks.is_empty() || corpus.ks.contains(&0) {
        bail!("ks must contain positive ranks");
    }
    if corpus.budget_tokens == 0 || corpus.max_hits == 0 {
        bail!("budgetTokens and maxHits must be positive");
    }
    if corpus.repository.commit.len() != 40
        || !corpus
            .repository
            .commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("repository commit must be a full 40-character Git SHA");
    }
    for case in &corpus.cases {
        if case.id.trim().is_empty() || case.question.trim().is_empty() || case.expected.is_empty() {
            bail!("case id, question, and expected evidence must not be empty");
        }
    }
    Ok(())
}

fn verify_repository(root: &Path, expected: &RepositorySpec) -> Result<()> {
    let head = git_output(root, &["rev-parse", "HEAD"])?;
    if !head.eq_ignore_ascii_case(&expected.commit) {
        bail!(
            "repository commit mismatch: expected {}, got {}",
            expected.commit,
            head
        );
    }
    let status = git_output(root, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        bail!("repository checkout is dirty; external benchmarks require an exact clean commit");
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {}", root.display()))?;
    if !output.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    String::from_utf8(output.stdout)
        .context("git returned non-UTF-8 output")
        .map(|value| value.trim().to_owned())
}

fn run_strategy(
    root: &Path,
    corpus: &Corpus,
    resolver: &dyn agent_shunt::application::ports::StructureResolver,
    index: Option<&dyn DenseIndex>,
    name: &str,
) -> Result<Report> {
    let mut results = Vec::with_capacity(corpus.cases.len());

    for case in &corpus.cases {
        let started = Instant::now();
        let (result, _) = execute_with_resolver(
            &RipgrepSearch,
            &GitChangeSource,
            &SecureFilesystem,
            resolver,
            index,
            None,
            None,
            &RetrieveInput {
                question: case.question.clone(),
                cwd: root.to_path_buf(),
                limits: Limits::default(),
                budget_tokens: corpus.budget_tokens,
                context_lines: corpus.context_lines,
                max_hits: corpus.max_hits,
                globs: corpus.globs.clone(),
                scope: None,
                mmr_lambda: MMR_LAMBDA,
                max_block_lines: MAX_BLOCK_LINES,
                min_score_percent: MIN_SCORE_PERCENT,
                why: false,
                prf: false,
                review: false,
            },
        )
        .with_context(|| format!("{name} failed case {}", case.id))?;

        results.push(CaseResult {
            id: case.id.clone(),
            first_relevant_rank: first_relevant_rank(&result.chunks, &case.expected),
            delivered_tokens: result.estimated_tokens,
            chunks: result.chunks.len(),
            latency_ms: duration_ms(started.elapsed()),
        });
    }

    let hit_at = corpus
        .ks
        .iter()
        .copied()
        .map(|k| {
            let hits = results
                .iter()
                .filter(|result| result.first_relevant_rank.is_some_and(|rank| rank <= k))
                .count();
            (k, hits as f64 / results.len() as f64)
        })
        .collect::<BTreeMap<_, _>>();
    let mrr = results
        .iter()
        .map(|result| {
            result
                .first_relevant_rank
                .map(|rank| 1.0 / rank as f64)
                .unwrap_or(0.0)
        })
        .sum::<f64>()
        / results.len() as f64;
    let avg_tokens = results
        .iter()
        .map(|result| result.delivered_tokens as f64)
        .sum::<f64>()
        / results.len() as f64;
    let avg_latency_ms =
        results.iter().map(|result| result.latency_ms).sum::<f64>() / results.len() as f64;
    let mut latencies = results
        .iter()
        .map(|result| result.latency_ms)
        .collect::<Vec<_>>();
    latencies.sort_by(f64::total_cmp);
    let p95_index = ((latencies.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(latencies.len() - 1);

    Ok(Report {
        strategy: name.to_owned(),
        cases: results.len(),
        hit_at,
        mrr,
        avg_tokens,
        avg_latency_ms,
        p95_latency_ms: latencies[p95_index],
        results,
    })
}

fn first_relevant_rank(chunks: &[RetrievedChunk], expected: &[ExpectedEvidence]) -> Option<usize> {
    chunks
        .iter()
        .position(|chunk| expected.iter().any(|target| matches_target(chunk, target)))
        .map(|index| index + 1)
}

fn matches_target(chunk: &RetrievedChunk, target: &ExpectedEvidence) -> bool {
    if chunk.path != target.path {
        return false;
    }
    match (target.start_line, target.end_line) {
        (Some(start), Some(end)) => chunk.start_line <= end && start <= chunk.end_line,
        _ => true,
    }
}

fn print_report(corpus: &Corpus, report: &Report) {
    print!("{}", report.strategy);
    for k in &corpus.ks {
        print!("\thit@{k}={:.3}", report.hit_at.get(k).copied().unwrap_or(0.0));
    }
    println!(
        "\tMRR={:.3}\tavg tokens={:.1}\tavg ms={:.1}\tp95 ms={:.1}",
        report.mrr, report.avg_tokens, report.avg_latency_ms, report.p95_latency_ms
    );
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
