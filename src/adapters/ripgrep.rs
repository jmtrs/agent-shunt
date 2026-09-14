use std::{
    collections::HashSet,
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::{application::ports::CodeSearch, domain::SearchHit};

pub struct RipgrepSearch;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Generated or dependency-owned paths that never hold useful source evidence.
const EXCLUDED_GLOBS: &[&str] = &[
    "!node_modules/**",
    "!target/**",
    "!.git/**",
    "!dist/**",
    "!build/**",
    "!coverage/**",
    "!.next/**",
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

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "where", "what", "which", "from", "this", "that", "los", "las",
    "una", "uno", "del", "con", "donde", "dónde", "como", "cómo", "que", "qué", "por", "para",
    "está", "esta", "son", "hay",
];

impl CodeSearch for RipgrepSearch {
    fn terms(&self, question: &str) -> Vec<String> {
        query_terms(question)
    }

    fn search(&self, root: &Path, question: &str, max_hits: usize) -> Result<Vec<SearchHit>> {
        let terms = query_terms(question);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = filename_hits(root, &terms, 10_000)?;
        let raw_hit_limit = max_hits.saturating_mul(50).clamp(max_hits, 5_000);
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
        for glob in EXCLUDED_GLOBS {
            command.args(["--glob", glob]);
        }
        command.args(["--", &pattern, "."]);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let mut child = command.spawn().context("failed to execute ripgrep")?;
        let stdout = child.stdout.take().context("ripgrep stdout unavailable")?;
        let (receiver, reader) = line_reader(stdout);
        let deadline = Instant::now() + SEARCH_TIMEOUT;
        let mut terminated = false;
        loop {
            let Some(line) = receive_line(&receiver, deadline)? else {
                if Instant::now() >= deadline {
                    terminated = true;
                    let _ = child.kill();
                }
                break;
            };
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
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
            let Some(line_number) = event
                .pointer("/data/line_number")
                .and_then(|value| value.as_u64())
            else {
                continue;
            };
            let text = event
                .pointer("/data/lines/text")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let matched_terms = terms.iter().enumerate().fold(0u16, |mask, (index, term)| {
                mask | (u16::from(text.contains(&term.to_lowercase())) << index)
            });
            let score = matched_terms.count_ones().max(1) as usize;
            hits.push(SearchHit {
                path: path.strip_prefix("./").unwrap_or(path).to_owned(),
                line: line_number as usize,
                score,
                matched_terms,
            });
            if hits.len() >= raw_hit_limit {
                terminated = true;
                let _ = child.kill();
                break;
            }
        }
        drop(receiver);
        let status = child.wait()?;
        let _ = reader.join();
        if !status.success() && status.code() != Some(1) && !terminated {
            bail!("ripgrep failed with status {status}");
        }
        let mut coverage = std::collections::HashMap::<String, u16>::new();
        for hit in &hits {
            *coverage.entry(hit.path.clone()).or_default() |= hit.matched_terms;
        }
        for hit in &mut hits {
            hit.score = hit.score * 2
                + coverage
                    .get(hit.path.as_str())
                    .copied()
                    .unwrap_or_default()
                    .count_ones() as usize;
        }
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.line.cmp(&right.line))
        });
        let mut represented = HashSet::new();
        let mut diverse = Vec::with_capacity(hits.len());
        let mut deferred = Vec::new();
        for hit in hits {
            if represented.insert(hit.path.clone()) {
                diverse.push(hit);
            } else {
                deferred.push(hit);
            }
        }
        diverse.extend(deferred);
        diverse.truncate(max_hits);
        Ok(diverse)
    }

    fn available(&self) -> bool {
        Command::new("rg")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }
}

fn filename_hits(root: &Path, terms: &[String], scan_limit: usize) -> Result<Vec<SearchHit>> {
    let mut command = Command::new("rg");
    command.current_dir(root).args(["--files"]);
    for glob in EXCLUDED_GLOBS {
        command.args(["--glob", glob]);
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = command
        .spawn()
        .context("failed to list files with ripgrep")?;
    let stdout = child.stdout.take().context("ripgrep stdout unavailable")?;
    let (receiver, reader) = line_reader(stdout);
    let mut hits = Vec::new();
    let mut scanned = 0;
    let mut terminated = false;
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    loop {
        let Some(path) = receive_line(&receiver, deadline)? else {
            if Instant::now() >= deadline {
                terminated = true;
                let _ = child.kill();
            }
            break;
        };
        if scanned >= scan_limit {
            terminated = true;
            let _ = child.kill();
            break;
        }
        scanned += 1;
        let normalized = path.to_lowercase();
        let matched_terms = terms.iter().enumerate().fold(0u16, |mask, (index, term)| {
            mask | (u16::from(normalized.contains(&term.to_lowercase())) << index)
        });
        let score = matched_terms.count_ones() as usize;
        if score > 0 {
            hits.push(SearchHit {
                path,
                line: 1,
                score: score * 2,
                matched_terms,
            });
        }
    }
    drop(receiver);
    let status = child.wait()?;
    let _ = reader.join();
    if !status.success() && !terminated {
        bail!("ripgrep file listing failed with status {status}");
    }
    Ok(hits)
}

fn line_reader(
    stdout: impl std::io::Read + Send + 'static,
) -> (
    mpsc::Receiver<std::io::Result<String>>,
    thread::JoinHandle<()>,
) {
    let (sender, receiver) = mpsc::sync_channel(64);
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    (receiver, reader)
}

fn receive_line(
    receiver: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
) -> Result<Option<String>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(line) => Ok(Some(line?)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
    }
}

fn query_terms(question: &str) -> Vec<String> {
    let splitter = Regex::new(r"[^\p{L}\p{N}_:.\-/]+").expect("static regex");
    let stop = STOP_WORDS.iter().copied().collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    splitter
        .split(question)
        .map(|term| term.trim_matches(['.', ':', '/', '-']))
        .filter(|term| term.chars().count() >= 3)
        .filter(|term| !stop.contains(term.to_lowercase().as_str()))
        .filter(|term| seen.insert(term.to_lowercase()))
        .take(12)
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::application::ports::CodeSearch;

    use super::RipgrepSearch;

    #[test]
    fn removes_common_query_words() {
        assert_eq!(
            RipgrepSearch.terms("Dónde está authentication validation?"),
            ["authentication", "validation"]
        );
    }

    #[test]
    fn mixed_case_terms_still_match_lowercase_content() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("code.rs"), "let token = auth_value;").unwrap();
        let hits = RipgrepSearch.search(root.path(), "Auth token", 10).unwrap();
        assert!(hits.iter().any(|hit| hit.path == "code.rs"));
    }

    #[test]
    fn many_file_search_stops_at_hit_limit() {
        let root = tempdir().unwrap();
        for index in 0..300 {
            fs::write(
                root.path().join(format!("file-{index}.txt")),
                "bounded-search-marker\n",
            )
            .unwrap();
        }
        let hits = RipgrepSearch
            .search(root.path(), "bounded-search-marker", 7)
            .unwrap();
        assert_eq!(hits.len(), 7);
    }
}
