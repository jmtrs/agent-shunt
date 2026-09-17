use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use agent_shunt::{
    adapters::{filesystem::SecureFilesystem, git::GitChangeSource, ripgrep::RipgrepSearch},
    application::{
        ports::{CodeSearch, DocumentLoader},
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
const RG_TOP_K: [usize; 3] = [1, 3, 5];

// Keep the plain-rg baseline inside the same safe source universe as production
// retrieval. Corpus-specific globs are applied first and these exclusions last.
// The baseline intentionally does not use agent-shunt's filename/path boosts,
// IDF weighting, AST chunking, MMR, PRF, or relevance pruning.
const EXCLUDED_GLOBS: &[&str] = &[
    "!**/node_modules/**",
    "!**/target/**",
    "!**/.git/**",
    "!**/.svn/**",
    "!**/.hg/**",
    "!**/dist/**",
    "!**/build/**",
    "!**/coverage/**",
    "!**/.next/**",
    "!**/.env",
    "!**/.env.*",
    "!**/.idea/**",
    "!**/.vscode/**",
    "!**/.DS_Store",
    "!**/__pycache__/**",
    "!**/.venv/**",
    "!**/.pytest_cache/**",
    "!**/.mypy_cache/**",
    "!**/.ruff_cache/**",
    "!**/.gradle/**",
    "!**/.terraform/**",
    "!Cargo.lock",
    "!package-lock.json",
    "!yarn.lock",
    "!pnpm-lock.yaml",
    "!bun.lockb",
    "!go.sum",
    "!poetry.lock",
    "!composer.lock",
    "!Gemfile.lock",
    "!flake.lock",
];

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

#[derive(Debug, Default)]
struct RgFileScore {
    matched_terms: BTreeSet<usize>,
    matching_lines: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    corpus: String,
    cases: usize,
    agent_chunk_hit_at: BTreeMap<usize, f64>,
    agent_chunk_mrr: f64,
    agent_file_hit_at: BTreeMap<usize, f64>,
    agent_file_mrr: f64,
    rg_file_hit_at: BTreeMap<usize, f64>,
    rg_file_mrr: f64,
    total_agent_tokens: usize,
    total_rg_top1_tokens: usize,
    total_rg_top3_tokens: usize,
    total_rg_top5_tokens: usize,
    agent_vs_rg_top1_reduction_pct: f64,
    agent_vs_rg_top3_reduction_pct: f64,
    agent_vs_rg_top5_reduction_pct: f64,
    agent_vs_rg_top1_compression: f64,
    agent_vs_rg_top3_compression: f64,
    agent_vs_rg_top5_compression: f64,
    avg_agent_tokens: f64,
    avg_rg_top1_tokens: f64,
    avg_rg_top3_tokens: f64,
    avg_rg_top5_tokens: f64,
    median_agent_vs_rg_top5_reduction_pct: f64,
    p95_agent_tokens: usize,
    p95_rg_top5_tokens: usize,
    avg_rg_matched_files: f64,
    rg_no_match_cases: usize,
    results: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseResult {
    id: String,
    agent_chunk_first_relevant_rank: Option<usize>,
    agent_file_first_relevant_rank: Option<usize>,
    rg_file_first_relevant_rank: Option<usize>,
    agent_tokens: usize,
    rg_top1_tokens: usize,
    rg_top3_tokens: usize,
    rg_top5_tokens: usize,
    agent_vs_rg_top5_reduction_pct: f64,
    rg_matched_files: usize,
    rg_top5_files: Vec<String>,
}

fn main() -> Result<ExitCode> {
    let mut args = env::args().skip(1);
    let corpus_path = PathBuf::from(args.next().unwrap_or_else(|| "eval/corpus.json".to_owned()));
    let mut root = env::current_dir().context("cannot resolve current directory")?;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(args.next().context("--root requires a path")?),
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

    let search = RipgrepSearch;
    let loader = SecureFilesystem;
    let mut results = Vec::with_capacity(corpus.cases.len());

    for case in &corpus.cases {
        let (agent, _) = execute_with_resolver(
            &search,
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
        .with_context(|| format!("agent-shunt lexical retrieval failed case {}", case.id))?;

        let benchmark_delivered = agent
            .chunks
            .iter()
            .map(|chunk| delivered_tokens(&chunk.content, &chunk.path))
            .sum::<usize>();
        ensure!(
            benchmark_delivered == agent.estimated_tokens,
            "token estimator drift in case {}: benchmark={} production={}",
            case.id,
            benchmark_delivered,
            agent.estimated_tokens
        );

        let agent_files = distinct_chunk_paths(&agent.chunks);
        let terms = search.terms(&case.question);
        let rg_files = raw_rg_ranked_files(root, &terms, &corpus.globs)
            .with_context(|| format!("plain ripgrep baseline failed case {}", case.id))?;
        let rg_top5 = rg_files.iter().take(5).cloned().collect::<Vec<_>>();
        let rg_tokens = whole_file_tokens(root, &loader, &rg_top5)?;

        let rg_top1_tokens = cumulative_at(&rg_tokens, 1);
        let rg_top3_tokens = cumulative_at(&rg_tokens, 3);
        let rg_top5_tokens = cumulative_at(&rg_tokens, 5);

        results.push(CaseResult {
            id: case.id.clone(),
            agent_chunk_first_relevant_rank: first_relevant_chunk_rank(
                &agent.chunks,
                &case.expected,
            ),
            agent_file_first_relevant_rank: first_relevant_file_rank(&agent_files, &case.expected),
            rg_file_first_relevant_rank: first_relevant_file_rank(&rg_files, &case.expected),
            agent_tokens: agent.estimated_tokens,
            rg_top1_tokens,
            rg_top3_tokens,
            rg_top5_tokens,
            agent_vs_rg_top5_reduction_pct: reduction_pct(agent.estimated_tokens, rg_top5_tokens),
            rg_matched_files: rg_files.len(),
            rg_top5_files: rg_top5,
        });
    }

    let agent_chunk_hit_at = hit_at(&results, |r| r.agent_chunk_first_relevant_rank);
    let agent_file_hit_at = hit_at(&results, |r| r.agent_file_first_relevant_rank);
    let rg_file_hit_at = hit_at(&results, |r| r.rg_file_first_relevant_rank);
    let agent_chunk_mrr = mrr(&results, |r| r.agent_chunk_first_relevant_rank);
    let agent_file_mrr = mrr(&results, |r| r.agent_file_first_relevant_rank);
    let rg_file_mrr = mrr(&results, |r| r.rg_file_first_relevant_rank);

    let total_agent_tokens = results.iter().map(|r| r.agent_tokens).sum();
    let total_rg_top1_tokens = results.iter().map(|r| r.rg_top1_tokens).sum();
    let total_rg_top3_tokens = results.iter().map(|r| r.rg_top3_tokens).sum();
    let total_rg_top5_tokens = results.iter().map(|r| r.rg_top5_tokens).sum();
    let count = results.len() as f64;

    let mut reductions = results
        .iter()
        .map(|r| r.agent_vs_rg_top5_reduction_pct)
        .collect::<Vec<_>>();
    reductions.sort_by(f64::total_cmp);

    let mut agent_values = results.iter().map(|r| r.agent_tokens).collect::<Vec<_>>();
    let mut rg_top5_values = results.iter().map(|r| r.rg_top5_tokens).collect::<Vec<_>>();
    agent_values.sort_unstable();
    rg_top5_values.sort_unstable();

    Ok(Report {
        corpus: corpus.name.clone(),
        cases: results.len(),
        agent_chunk_hit_at,
        agent_chunk_mrr,
        agent_file_hit_at,
        agent_file_mrr,
        rg_file_hit_at,
        rg_file_mrr,
        total_agent_tokens,
        total_rg_top1_tokens,
        total_rg_top3_tokens,
        total_rg_top5_tokens,
        agent_vs_rg_top1_reduction_pct: reduction_pct(total_agent_tokens, total_rg_top1_tokens),
        agent_vs_rg_top3_reduction_pct: reduction_pct(total_agent_tokens, total_rg_top3_tokens),
        agent_vs_rg_top5_reduction_pct: reduction_pct(total_agent_tokens, total_rg_top5_tokens),
        agent_vs_rg_top1_compression: compression_ratio(total_agent_tokens, total_rg_top1_tokens),
        agent_vs_rg_top3_compression: compression_ratio(total_agent_tokens, total_rg_top3_tokens),
        agent_vs_rg_top5_compression: compression_ratio(total_agent_tokens, total_rg_top5_tokens),
        avg_agent_tokens: total_agent_tokens as f64 / count,
        avg_rg_top1_tokens: total_rg_top1_tokens as f64 / count,
        avg_rg_top3_tokens: total_rg_top3_tokens as f64 / count,
        avg_rg_top5_tokens: total_rg_top5_tokens as f64 / count,
        median_agent_vs_rg_top5_reduction_pct: median(&reductions),
        p95_agent_tokens: percentile95(&agent_values),
        p95_rg_top5_tokens: percentile95(&rg_top5_values),
        avg_rg_matched_files: results
            .iter()
            .map(|r| r.rg_matched_files as f64)
            .sum::<f64>()
            / count,
        rg_no_match_cases: results.iter().filter(|r| r.rg_matched_files == 0).count(),
        results,
    })
}

fn raw_rg_ranked_files(root: &Path, terms: &[String], globs: &[String]) -> Result<Vec<String>> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let pattern = terms
        .iter()
        .map(|term| regex::escape(term))
        .collect::<Vec<_>>()
        .join("|");
    let mut command = Command::new("rg");
    command.current_dir(root).args([
        "--json",
        "--line-number",
        "--ignore-case",
        "--no-messages",
        "--max-count",
        "20",
        "--max-filesize",
        "5M",
    ]);
    for glob in globs {
        command.args(["--glob", glob]);
    }
    for glob in EXCLUDED_GLOBS {
        command.args(["--glob", glob]);
    }
    command.args(["--", &pattern, "."]);

    let output = command
        .output()
        .context("failed to execute ripgrep baseline")?;
    ensure!(
        output.status.success() || output.status.code() == Some(1),
        "ripgrep baseline failed with status {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let lowered_terms = terms
        .iter()
        .map(|term| term.to_lowercase())
        .collect::<Vec<_>>();
    let mut scores = HashMap::<String, RgFileScore>::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("type").and_then(|value| value.as_str()) != Some("match") {
            continue;
        }
        let Some(path) = event
            .pointer("/data/path/text")
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        let text = event
            .pointer("/data/lines/text")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let score = scores
            .entry(path.strip_prefix("./").unwrap_or(path).to_owned())
            .or_default();
        score.matching_lines += 1;
        for (index, term) in lowered_terms.iter().enumerate() {
            if text.contains(term) {
                score.matched_terms.insert(index);
            }
        }
    }

    let mut ranked = scores.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_path, left), (right_path, right)| {
        right
            .matched_terms
            .len()
            .cmp(&left.matched_terms.len())
            .then_with(|| right.matching_lines.cmp(&left.matching_lines))
            .then_with(|| left_path.cmp(right_path))
    });
    Ok(ranked.into_iter().map(|(path, _)| path).collect())
}

fn whole_file_tokens(
    root: &Path,
    loader: &SecureFilesystem,
    paths: &[String],
) -> Result<Vec<usize>> {
    paths
        .iter()
        .map(|path| {
            let loaded = loader.load(root, &[PathBuf::from(path)], &Limits::default())?;
            let tokens = loaded
                .documents
                .first()
                .map(|document| delivered_tokens(&document.numbered_content, &document.path))
                .unwrap_or(0);
            Ok(tokens)
        })
        .collect()
}

fn distinct_chunk_paths(chunks: &[RetrievedChunk]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for chunk in chunks {
        if seen.insert(chunk.path.clone()) {
            paths.push(chunk.path.clone());
        }
    }
    paths
}

fn first_relevant_chunk_rank(
    chunks: &[RetrievedChunk],
    expected: &[ExpectedEvidence],
) -> Option<usize> {
    chunks
        .iter()
        .position(|chunk| {
            expected
                .iter()
                .any(|target| matches_chunk_target(chunk, target))
        })
        .map(|index| index + 1)
}

fn first_relevant_file_rank(paths: &[String], expected: &[ExpectedEvidence]) -> Option<usize> {
    paths
        .iter()
        .position(|path| expected.iter().any(|target| path == &target.path))
        .map(|index| index + 1)
}

fn matches_chunk_target(chunk: &RetrievedChunk, target: &ExpectedEvidence) -> bool {
    if chunk.path != target.path {
        return false;
    }
    match (target.start_line, target.end_line) {
        (Some(start), Some(end)) => chunk.start_line <= end && start <= chunk.end_line,
        _ => true,
    }
}

fn hit_at<F>(results: &[CaseResult], rank: F) -> BTreeMap<usize, f64>
where
    F: Fn(&CaseResult) -> Option<usize>,
{
    RG_TOP_K
        .into_iter()
        .map(|k| {
            let hits = results
                .iter()
                .filter(|result| rank(result).is_some_and(|r| r <= k))
                .count();
            (k, hits as f64 / results.len() as f64)
        })
        .collect()
}

fn mrr<F>(results: &[CaseResult], rank: F) -> f64
where
    F: Fn(&CaseResult) -> Option<usize>,
{
    results
        .iter()
        .map(|result| rank(result).map(|r| 1.0 / r as f64).unwrap_or(0.0))
        .sum::<f64>()
        / results.len() as f64
}

fn cumulative_at(values: &[usize], k: usize) -> usize {
    values.iter().take(k).sum()
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
    if sorted.len().is_multiple_of(2) {
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
        "agent chunk quality: hit@1={:.3} hit@3={:.3} hit@5={:.3} MRR={:.3}",
        report.agent_chunk_hit_at[&1],
        report.agent_chunk_hit_at[&3],
        report.agent_chunk_hit_at[&5],
        report.agent_chunk_mrr
    );
    println!(
        "agent file quality:  hit@1={:.3} hit@3={:.3} hit@5={:.3} MRR={:.3}",
        report.agent_file_hit_at[&1],
        report.agent_file_hit_at[&3],
        report.agent_file_hit_at[&5],
        report.agent_file_mrr
    );
    println!(
        "rg file quality:     hit@1={:.3} hit@3={:.3} hit@5={:.3} MRR={:.3}",
        report.rg_file_hit_at[&1],
        report.rg_file_hit_at[&3],
        report.rg_file_hit_at[&5],
        report.rg_file_mrr
    );
    println!(
        "context totals: agent={} rg-top1={} rg-top3={} rg-top5={}",
        report.total_agent_tokens,
        report.total_rg_top1_tokens,
        report.total_rg_top3_tokens,
        report.total_rg_top5_tokens
    );
    println!(
        "agent vs rg: top1={:.1}%/{:.2}x top3={:.1}%/{:.2}x top5={:.1}%/{:.2}x",
        report.agent_vs_rg_top1_reduction_pct,
        report.agent_vs_rg_top1_compression,
        report.agent_vs_rg_top3_reduction_pct,
        report.agent_vs_rg_top3_compression,
        report.agent_vs_rg_top5_reduction_pct,
        report.agent_vs_rg_top5_compression
    );
    println!(
        "per-case: avg agent={:.1} avg rg top5={:.1} median top5 reduction={:.1}% p95 agent={} p95 rg top5={} avg rg matches={:.1} no-match={}",
        report.avg_agent_tokens,
        report.avg_rg_top5_tokens,
        report.median_agent_vs_rg_top5_reduction_pct,
        report.p95_agent_tokens,
        report.p95_rg_top5_tokens,
        report.avg_rg_matched_files,
        report.rg_no_match_cases
    );
}
