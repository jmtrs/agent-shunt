use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{Duration, Instant},
};

use agent_shunt::{
    adapters::{filesystem::SecureFilesystem, git::GitChangeSource, ripgrep::RipgrepSearch},
    application::retrieve::{
        MAX_BLOCK_LINES, MIN_SCORE_PERCENT, MMR_LAMBDA, RetrieveInput, execute_with_resolver,
    },
    domain::{Limits, RetrievedChunk},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[cfg(feature = "ast")]
use agent_shunt::adapters::tree_sitter_chunker::AstResolver;
#[cfg(not(feature = "ast"))]
use agent_shunt::application::resolver::HeuristicResolver;

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
    ks: Vec<usize>,
    strategies: Vec<Strategy>,
    cases: Vec<EvalCase>,
    #[serde(default)]
    gates: BTreeMap<String, Gate>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepositorySpec {
    url: String,
    commit: String,
}

#[derive(Debug)]
struct Cli {
    corpus_path: PathBuf,
    root: PathBuf,
    json_output: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Strategy {
    Lexical,
    Prf,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Prf => "prf",
        }
    }

    fn prf(self) -> bool {
        matches!(self, Self::Prf)
    }
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Gate {
    #[serde(default)]
    recall_at: BTreeMap<usize, f64>,
    min_mrr: Option<f64>,
    max_avg_tokens: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StrategyReport {
    strategy: String,
    cases: usize,
    recall_at: BTreeMap<usize, f64>,
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
    let cli = parse_args()?;
    let raw = fs::read_to_string(&cli.corpus_path)
        .with_context(|| format!("cannot read corpus {}", cli.corpus_path.display()))?;
    let corpus: Corpus = serde_json::from_str(&raw)
        .with_context(|| format!("invalid corpus {}", cli.corpus_path.display()))?;
    validate_corpus(&corpus)?;

    let root = fs::canonicalize(&cli.root)
        .with_context(|| format!("cannot resolve repository root {}", cli.root.display()))?;
    if !root.is_dir() {
        bail!("repository root is not a directory: {}", root.display());
    }
    if let Some(repository) = &corpus.repository {
        verify_repository(&root, repository)?;
    }

    let mut reports = Vec::new();
    let mut failures = Vec::new();

    for strategy in &corpus.strategies {
        let report = run_strategy(&root, &corpus, *strategy)?;
        if let Some(gate) = corpus.gates.get(strategy.name()) {
            failures.extend(check_gate(&report, gate));
        }
        reports.push(report);
    }

    if cli.json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": corpus.version,
                "corpus": corpus.name,
                "repository": corpus.repository,
                "reports": reports,
                "gateFailures": failures,
            }))?
        );
    } else {
        print_human(&corpus, &reports, &failures);
    }

    Ok(if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn parse_args() -> Result<Cli> {
    let mut corpus_path = None;
    let mut root = None;
    let mut json_output = false;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--root" => {
                if root.is_some() {
                    bail!("--root may only be provided once");
                }
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

    Ok(Cli {
        corpus_path: corpus_path.unwrap_or_else(|| PathBuf::from("eval/corpus.json")),
        root: root.unwrap_or(env::current_dir().context("cannot resolve current directory")?),
        json_output,
    })
}

fn validate_corpus(corpus: &Corpus) -> Result<()> {
    if corpus.version != 1 {
        bail!("unsupported corpus version {}", corpus.version);
    }
    if corpus.cases.is_empty() {
        bail!("corpus contains no cases");
    }
    if corpus.strategies.is_empty() {
        bail!("corpus contains no strategies");
    }
    if corpus.ks.is_empty() || corpus.ks.contains(&0) {
        bail!("ks must contain positive ranks");
    }
    if corpus.budget_tokens == 0 || corpus.max_hits == 0 {
        bail!("budgetTokens and maxHits must be positive");
    }
    if let Some(repository) = &corpus.repository {
        if repository.url.trim().is_empty() {
            bail!("repository url must not be empty");
        }
        if repository.commit.len() != 40
            || !repository.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("repository commit must be a full 40-character Git SHA");
        }
    }
    for case in &corpus.cases {
        if case.id.trim().is_empty() || case.question.trim().is_empty() {
            bail!("case id and question must not be empty");
        }
        if case.expected.is_empty() {
            bail!("case {} contains no expected evidence", case.id);
        }
        for expected in &case.expected {
            if expected.path.trim().is_empty() {
                bail!("case {} contains an empty expected path", case.id);
            }
            match (expected.start_line, expected.end_line) {
                (None, None) => {}
                (Some(start), Some(end)) if start > 0 && end >= start => {}
                _ => bail!(
                    "case {} expected ranges need both startLine/endLine and a valid order",
                    case.id
                ),
            }
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
        bail!(
            "repository checkout is dirty; external benchmarks require an exact clean commit"
        );
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    String::from_utf8(output.stdout)
        .context("git returned non-UTF-8 output")
        .map(|value| value.trim().to_owned())
}

fn run_strategy(root: &Path, corpus: &Corpus, strategy: Strategy) -> Result<StrategyReport> {
    #[cfg(feature = "ast")]
    let resolver = AstResolver::default();
    #[cfg(not(feature = "ast"))]
    let resolver = HeuristicResolver;

    let mut results = Vec::with_capacity(corpus.cases.len());
    for case in &corpus.cases {
        let started = Instant::now();
        let (result, _) = execute_with_resolver(
            &RipgrepSearch,
            &GitChangeSource,
            &SecureFilesystem,
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
                prf: strategy.prf(),
                review: false,
            },
        )
        .with_context(|| format!("{} failed case {}", strategy.name(), case.id))?;
        let elapsed = started.elapsed();
        results.push(CaseResult {
            id: case.id.clone(),
            first_relevant_rank: first_relevant_rank(&result.chunks, &case.expected),
            delivered_tokens: result.estimated_tokens,
            chunks: result.chunks.len(),
            latency_ms: duration_ms(elapsed),
        });
    }

    let recall_at = corpus
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

    Ok(StrategyReport {
        strategy: strategy.name().to_owned(),
        cases: results.len(),
        recall_at,
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

fn check_gate(report: &StrategyReport, gate: &Gate) -> Vec<String> {
    let mut failures = Vec::new();
    for (k, minimum) in &gate.recall_at {
        let actual = report.recall_at.get(k).copied().unwrap_or(0.0);
        if actual + f64::EPSILON < *minimum {
            failures.push(format!(
                "{} recall@{} {:.3} < {:.3}",
                report.strategy, k, actual, minimum
            ));
        }
    }
    if let Some(minimum) = gate.min_mrr
        && report.mrr + f64::EPSILON < minimum
    {
        failures.push(format!(
            "{} MRR {:.3} < {:.3}",
            report.strategy, report.mrr, minimum
        ));
    }
    if let Some(maximum) = gate.max_avg_tokens
        && report.avg_tokens > maximum
    {
        failures.push(format!(
            "{} avg tokens {:.1} > {:.1}",
            report.strategy, report.avg_tokens, maximum
        ));
    }
    failures
}

fn print_human(corpus: &Corpus, reports: &[StrategyReport], failures: &[String]) {
    println!("corpus: {} ({} cases)", corpus.name, corpus.cases.len());
    if let Some(repository) = &corpus.repository {
        println!("repository: {} @ {}", repository.url, &repository.commit[..12]);
    }
    print!("strategy");
    for k in &corpus.ks {
        print!("\trecall@{k}");
    }
    println!("\tMRR\tavg tokens\tavg ms\tp95 ms");
    for report in reports {
        print!("{}", report.strategy);
        for k in &corpus.ks {
            print!("\t{:.3}", report.recall_at.get(k).copied().unwrap_or(0.0));
        }
        println!(
            "\t{:.3}\t{:.1}\t{:.1}\t{:.1}",
            report.mrr, report.avg_tokens, report.avg_latency_ms, report.p95_latency_ms
        );
    }
    if failures.is_empty() {
        println!("gates: PASS");
    } else {
        println!("gates: FAIL");
        for failure in failures {
            println!("  - {failure}");
        }
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
