use std::collections::BTreeMap;

use crate::domain::{Document, FileChange, LineRange, RetrievedChunk};

use super::{ENVELOPE_TOKENS_PER_CHUNK, SAME_PATH_SIM};

pub(super) fn decode_terms(mask: u16, terms: &[String]) -> Vec<String> {
    (0..terms.len())
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| terms[index].clone())
        .collect()
}

pub(super) fn documents_from_chunks(
    chunks: &[RetrievedChunk],
    originals: &[Document],
) -> Vec<Document> {
    let mut grouped: BTreeMap<&str, Vec<&RetrievedChunk>> = BTreeMap::new();
    for chunk in chunks {
        grouped.entry(&chunk.path).or_default().push(chunk);
    }
    grouped
        .into_iter()
        .filter_map(|(path, chunks)| {
            let original = originals.iter().find(|document| document.path == path)?;
            let allowed_ranges = chunks
                .iter()
                .map(|chunk| LineRange {
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                })
                .collect::<Vec<_>>();
            Some(Document {
                path: path.to_owned(),
                bytes: chunks.iter().map(|chunk| chunk.content.len()).sum(),
                line_count: original.line_count,
                lines: original.lines.clone(),
                numbered_content: chunks
                    .iter()
                    .map(|chunk| chunk.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                allowed_ranges,
            })
        })
        .collect()
}

/// Returns the top semantic file only when it is meaningfully separated from
/// the next distinct file. The ratio is relative to the leader's own cosine
/// similarity, avoiding an absolute similarity threshold tied to one model.
pub(super) fn delivered_tokens(content: &str, path: &str) -> usize {
    estimate_tokens(content) + estimate_tokens(path) + ENVELOPE_TOKENS_PER_CHUNK
}

/// Approximate BPE token count of delivered text. Word runs tokenize at roughly
/// four characters each (min one token), symbol runs at two (dense punctuation
/// only partly merges), and whitespace is free but breaks runs. This tracks a
/// real tokenizer far better than a flat `len / 4`, which undercounts the
/// symbol-dense punctuation of source code and so silently lets the token
/// budget overshoot the delivered footprint. It errs conservative (never below
/// a real count), so the budget is a ceiling the output cannot breach.
pub(super) fn estimate_tokens(text: &str) -> usize {
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

/// Lowercased identifier-ish tokens of a chunk's content, for redundancy
/// comparison. The line-number prefixes and short tokens are dropped so two
/// chunks are judged similar by the identifiers they share, not their line
/// numbering or punctuation.
pub(super) fn content_tokens(content: &str) -> std::collections::HashSet<String> {
    content
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| token.len() >= 3 && !token.chars().all(|character| character.is_numeric()))
        .map(str::to_lowercase)
        .collect()
}

/// Jaccard overlap of two token sets, in `[0, 1]`.
pub(super) fn jaccard(
    left: &std::collections::HashSet<String>,
    right: &std::collections::HashSet<String>,
) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Redundancy between two candidate chunks: content overlap, raised to a floor
/// for two chunks of the same file so per-file depth is treated as partly
/// redundant (the recall spread).
pub(super) fn similarity(
    left: usize,
    right: usize,
    candidates: &[RetrievedChunk],
    token_sets: &[std::collections::HashSet<String>],
) -> f64 {
    let overlap = jaccard(&token_sets[left], &token_sets[right]);
    if candidates[left].path == candidates[right].path {
        overlap.max(SAME_PATH_SIM)
    } else {
        overlap
    }
}

/// Whether `cwd`, or an ancestor, contains a `.git` entry — a cheap filesystem
/// check that avoids spawning git for the changed-file boost outside a work tree.
pub(super) fn in_git_worktree(cwd: &std::path::Path) -> bool {
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return true;
        }
        dir = current.parent();
    }
    false
}

/// A hit counts as inside the change when the file is wholly new (`whole_file`,
/// untracked) or the hit line falls inside a changed range. A tracked file
/// whose diff is pure deletions has empty hunks but is *not* whole-file, so
/// none of its hits get the boost.
pub(super) fn hit_in_changed_lines(hit: &crate::domain::SearchHit, change: &FileChange) -> bool {
    change.whole_file
        || change.hunks.iter().any(|range| {
            range.contains(LineRange {
                start_line: hit.line,
                end_line: hit.line,
            })
        })
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };

    use anyhow::Result;

    use std::collections::BTreeMap;
