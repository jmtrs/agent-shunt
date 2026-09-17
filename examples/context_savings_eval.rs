use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use agent_shunt::{
    adapters::{filesystem::SecureFilesystem, git::GitChangeSource, ripgrep::RipgrepSearch},
    application::{
        ports::DocumentLoader,
        retrieve::{
            MAX_BLOCK_LINES, MIN_SCORE_PERCENT, MMR_LAMBDA, RetrieveInput, execute_with_resolver,
        },
    },
    domain::{Limits, RetrievedChunk},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[cfg(feature = "ast")]
use agent_shunt::adapters::tree_sitter_chunker::AstResolver;
#[cfg(not(feature = "ast"))]
use agent_shunt::application::resolver::HeuristicResolver;

const ENVELOPE_TOKENS_PER_ITEM: usize = 40;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    version: u32,
    name: String,
    #[serde(default)]
    repository: Option<RepositorySpec>,
    budget_tokens: usize,
    context_lines: usize,
    max_hits: usize,
    #[serde(default)]
    globs: Vec<String>,
    cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    corpus: String,
    cases: usize,
    hit_at: BTreeMap<usize, f64>,
    mrr: f64,
    total_delivered_tokens: usize,
    total_whole_file_tokens: usize,
    context_reduction_pct: f64,
    compression_ratio: f64,
    avg_delivered_tokens: f64,
    avg_whole_file_tokens: f64,
    median_case_reduction_pct: f64,
    p95_delivered_tokens: usize,
    p95_whole_file_tokens: usize,
    results: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseResult {
    id: String,
    first_relevant_rank: Option<usize>,
    delivered_tokens: usize,
    whole_file_tokens: usize,
    context_reduction_pct: f64,
    compression_ratio: f64,
    chunks: usize,
    selected_files: usize,
}

fn main() -> Result<ExitCode> {
    let mut args = env::args().skip(1);
    let corpus_path = PathBuf::from(args.next().unwrap_or_else(|| "eval/corpus.json".to_owned()));
    let mut root = env::current_dir().context("cannot resolve current directory")?;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = PathBuf::from(args.next().context("--root requires a path")?);
            }
            "--json" => json = true,
            _ => bail!("unknown option {arg}"),
        }
    }

    let raw = fs::read_to_string(&corpus_path)
        .with_context(|| format!("cannot read corpus {}", corpus_path.display()))?;
    let corpus: Corpus = serde_json::from_str(&raw)
        .with_context(|| format!("invalid corpus {}", corpus_path.display()))?;
    ensure!(
        corpus.version == 1,
        "unsupported corpus version {}",
        corpus.version
    );
    ensure!(!corpus.cases.is_empty(), "corpus contains no cases");

    let root = fs::canonicalize(&root)
        .with_context(|| format!("cannot resolve repository root {}", root.display()))?;
    if let Some(repository) = &corpus.repository {
        verify_repository(&root, repository)?;
    }

    let report = run(&root, &corpus)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report, corpus.repository.as_ref());
    }
    Ok(ExitCode::SUCCESS)
}

fn run(root: &Path, corpus: &Corpus) -> Result<Report> {
    #[cfg(feature = "ast")]
    let resolver = AstResolver::default();
    #[cfg(not(feature = "ast"))]
    let resolver = HeuristicResolver;

    let loader = SecureFilesystem;
    let mut results = Vec::with_capacity(corpus.cases.len());

    for case in &corpus.cases {
        let (result, _) = execute_with_resolver(
            &RipgrepSearch,
            &GitChangeSource,
            &loader,
            &resolver,
            None,
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
        .with_context(|| format!("lexical retrieval failed case {}", case.id))?;

        // Keep the benchmark estimator mechanically aligned with production.
        // If production budgeting changes, this assertion forces the benchmark
        // to be updated instead of silently publishing incomparable numbers.
        let benchmark_delivered = result
            .chunks
            .iter()
            .map(|chunk| delivered_tokens(&chunk.content, &chunk.path))
            .sum::<usize>();
        ensure!(
            benchmark_delivered == result.estimated_tokens,
            "token estimator drift in case {}: benchmark={} production={}",
            case.id,
            benchmark_delivered,
            result.estimated_tokens
        );

        let selected_paths = result
            .chunks
            .iter()
            .map(|chunk| PathBuf::from(&chunk.path))
            .collect::<BTreeSet<_>>();
        let loaded = loader.load(
            root,
            &selected_paths.iter().cloned().collect::<Vec<_>>(),
            &Limits::default(),
        )?;
        let whole_file_tokens = loaded
            .documents
            .iter()
            .map(|document| delivered_tokens(&document.numbered_content, &document.path))
            .sum::<usize>();
        let delivered = result.estimated_tokens;
        let reduction = reduction_pct(delivered, whole_file_tokens);
        let ratio = compression_ratio(delivered, whole_file_tokens);

        results.push(CaseResult {
            id: case.id.clone(),
            first_relevant_rank: first_relevant_rank(&result.chunks, &case.expected),
            delivered_tokens: delivered,
            whole_file_tokens,
            context_reduction_pct: reduction,
            compression_ratio: ratio,
            chunks: result.chunks.len(),
            selected_files: selected_paths.len(),
        });
    }

    let hit_at = [1usize, 3, 5]
        .into_iter()
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

    let total_delivered_tokens = results.iter().map(|r| r.delivered_tokens).sum();
    let total_whole_file_tokens = results.iter().map(|r| r.whole_file_tokens).sum();
    let avg_delivered_tokens = total_delivered_tokens as f64 / results.len() as f64;
    let avg_whole_file_tokens = total_whole_file_tokens as f64 / results.len() as f64;

    let mut reductions = results
        .iter()
        .map(|r| r.context_reduction_pct)
        .collect::<Vec<_>>();
    reductions.sort_by(f64::total_cmp);
    let median_case_reduction_pct = median(&reductions);

    let mut delivered_values = results
        .iter()
        .map(|r| r.delivered_tokens)
        .collect::<Vec<_>>();
    let mut whole_values = results
        .iter()
        .map(|r| r.whole_file_tokens)
        .collect::<Vec<_>>();
    delivered_values.sort_unstable();
    whole_values.sort_unstable();

    Ok(Report {
        corpus: corpus.name.clone(),
        cases: results.len(),
        hit_at,
        mrr,
        total_delivered_tokens,
        total_whole_file_tokens,
        context_reduction_pct: reduction_pct(total_delivered_tokens, total_whole_file_tokens),
        compression_ratio: compression_ratio(total_delivered_tokens, total_whole_file_tokens),
        avg_delivered_tokens,
        avg_whole_file_tokens,
        median_case_reduction_pct,
        p95_delivered_tokens: percentile95(&delivered_values),
        p95_whole_file_tokens: percentile95(&whole_values),
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

fn reduction_pct(delivered: usize, baseline: usize) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        100.0 * (1.0 - delivered as f64 / baseline as f64)
    }
}

fn compression_ratio(delivered: usize, baseline: usize) -> f64 {
    if delivered == 0 {
        0.0
    } else {
        baseline as f64 / delivered as f64
    }
}

fn median(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn percentile95(sorted: &[usize]) -> usize {
    let index = ((sorted.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

// Exact copy of the production estimator used by retrieval budgeting. The
// equality assertion above makes any future production drift fail loudly.
fn delivered_tokens(content: &str, path: &str) -> usize {
    estimate_tokens(content) + estimate_tokens(path) + ENVELOPE_TOKENS_PER_ITEM
}

fn estimate_tokens(text: &str) -> usize {
    let mut tokens = 0usize;
    let mut run = 0usize;
    let mut run_is_word = false;
    for character in text.chars() {
        let is_word = character.is_alphanumeric() || character == '_';
        let is_space = character.is_whitespace();
        if is_space || is_word != run_is_word {
            tokens += run_tokens(run, run_is_word);
            run = 0;
            run_is_word = is_word;
        }
        if !is_space {
            run += 1;
        }
    }
    tokens + run_tokens(run, run_is_word)
}

fn run_tokens(run: usize, is_word: bool) -> usize {
    if run == 0 {
        0
    } else if is_word {
        run.div_ceil(4).max(1)
    } else {
        run.div_ceil(2).max(1)
    }
}

fn verify_repository(root: &Path, expected: &RepositorySpec) -> Result<()> {
    let head = git_output(root, &["rev-parse", "HEAD"])?;
    ensure!(
        head.eq_ignore_ascii_case(&expected.commit),
        "repository commit mismatch: expected {}, got {}",
        expected.commit,
        head
    );
    let status = git_output(root, &["status", "--porcelain", "--untracked-files=all"])?;
    ensure!(status.is_empty(), "repository checkout is dirty");
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {}", root.display()))?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)
        .context("git returned non-UTF-8 output")?
        .trim()
        .to_owned())
}

fn print_human(report: &Report, repository: Option<&RepositorySpec>) {
    println!("corpus: {} ({} cases)", report.corpus, report.cases);
    if let Some(repository) = repository {
        println!(
            "repository: {} @ {}",
            repository.url,
            &repository.commit[..12]
        );
    }
    println!(
        "quality: hit@1={:.3} hit@3={:.3} hit@5={:.3} MRR={:.3}",
        report.hit_at[&1], report.hit_at[&3], report.hit_at[&5], report.mrr
    );
    println!(
        "context: delivered={} whole-selected-files={} reduction={:.1}% compression={:.2}x",
        report.total_delivered_tokens,
        report.total_whole_file_tokens,
        report.context_reduction_pct,
        report.compression_ratio
    );
    println!(
        "per-case: avg delivered={:.1} avg whole={:.1} median reduction={:.1}% p95 delivered={} p95 whole={}",
        report.avg_delivered_tokens,
        report.avg_whole_file_tokens,
        report.median_case_reduction_pct,
        report.p95_delivered_tokens,
        report.p95_whole_file_tokens
    );
}
