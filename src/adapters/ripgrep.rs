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

/// Generated or dependency-owned paths, and secret-bearing files, that never
/// hold useful source evidence. Directory patterns are `**/`-anchored so they
/// match at any depth (a monorepo's `packages/app/node_modules`, not only a
/// top-level `node_modules`); bare-filename patterns already match at any depth
/// in ripgrep. These are applied *after* caller globs (see `apply_globs`) so a
/// caller include such as `packages/**` cannot re-admit them via ripgrep's
/// last-match-wins glob resolution.
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

/// Registers caller globs first, then [`EXCLUDED_GLOBS`], so the built-in
/// exclusions always win ripgrep's last-match-wins resolution. Registering them
/// in the other order lets a caller include (`packages/**`) silently re-admit
/// `node_modules`, `.git`, and `.env`, flooding results with dependency code and
/// leaking secrets into analysis.
pub(crate) fn apply_globs(command: &mut Command, globs: &[String]) {
    for glob in globs {
        command.args(["--glob", glob]);
    }
    for glob in EXCLUDED_GLOBS {
        command.args(["--glob", glob]);
    }
}

/// Filename extensions that never hold readable source. A filename match on one
/// of these (e.g. `create-table.png`) must never enter the ranked set.
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "pdf", "zip", "gz", "tar", "bz2",
    "xz", "7z", "rar", "tgz", "jar", "war", "class", "exe", "dll", "so", "dylib", "bin", "dat",
    "wasm", "woff", "woff2", "ttf", "otf", "eot", "mp3", "mp4", "mov", "avi", "mkv", "wav", "flac",
    "psd", "lockb", "node", "pyc", "obj",
];

/// Scales the (logarithmic) inverse document frequency into integer term
/// weights. Rarer terms outweigh generic ones, but the log keeps the spread
/// gentle so a file matching one rare term stays competitive with a prose file
/// that happens to mention several common ones.
const IDF_SCALE: f64 = 4.0;

/// The file-coverage bonus credits a file's single strongest (rarest) matched
/// term in full, and the rest only at this fraction. Without the discount a
/// tangential file that mentions many *generic* query terms (`how`, `are`,
/// `resolved`) out-covers the real subject file that holds the one *rare*,
/// discriminating term (`precedence`) — the measured cause of an off-target
/// file flooding a localized question.
const COVERAGE_TAIL_DIVISOR: usize = 3;

/// A file whose *name* matches a query term is a strong locator ("metrics" ->
/// metrics.rs), so a filename match is worth several content occurrences of the
/// same term, weighted by the term's rarity (IDF). Generic tokens weigh little,
/// so `create-table.png` cannot dominate the way it used to.
const FILENAME_BOOST: usize = 8;

/// A matching parent directory is useful module/package evidence, but it is a
/// much weaker locator than the basename. It only augments files that already
/// matched query content, so a directory called `matcher` cannot inject every
/// descendant into the ranked set by itself.
const PATH_COMPONENT_BOOST: usize = 2;

/// Documentation and prose files match broad natural-language vocabulary, so a
/// README or CHANGELOG easily outranks the real source for a code question.
/// This worker is a source-code analyst, so a prose file's score is scaled to
/// this fraction — enough to sink below matching code, but never to zero, so a
/// doc-only match still surfaces when nothing else answers the question.
const PROSE_SCORE_NUM: usize = 1;
const PROSE_SCORE_DEN: usize = 3;

/// Extensions whose content is prose, not source. Down-weighted, not excluded:
/// a doc may still be the only evidence for a question about the docs.
const PROSE_EXTENSIONS: &[&str] = &["md", "mdc", "markdown", "rst", "txt", "adoc", "org"];

/// Extensionless (or any-extension) prose filenames, matched on the stem so
/// `README`, `README.md`, and `CHANGELOG.rst` are all recognised.
const PROSE_STEMS: &[&str] = &[
    "readme",
    "changelog",
    "changes",
    "history",
    "license",
    "licence",
    "authors",
    "contributors",
    "contributing",
    "notice",
    "copying",
    "codeowners",
];

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "where", "what", "which", "from", "this", "that",
    // Interrogatives and auxiliaries carry no locating signal but survive the
    // length filter, so a natural-language question ("how does X work") would
    // otherwise spend term slots and rank noise on them.
    "how", "does", "are", "was", "were", "has", "have", "had", "why", "who", "will", "can",
    "should", "would", "into", "about", "los", "las", "una", "uno", "del", "con", "donde", "dónde",
    "como", "cómo", "que", "qué", "por", "para", "está", "esta", "son", "hay",
];

impl CodeSearch for RipgrepSearch {
    fn terms(&self, question: &str) -> Vec<String> {
        query_terms(question)
    }

    fn search(
        &self,
        root: &Path,
        question: &str,
        max_hits: usize,
        globs: &[String],
    ) -> Result<Vec<SearchHit>> {
        let terms = query_terms(question);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // Search and score on stems so a plural query term still recovers the
        // singular in code: `paths` -> `path`, `questions` -> `question`.
        // Each term carries its cross-convention spellings so a camelCase
        // identifier also matches its snake_case or kebab-case rendering.
        let forms = terms
            .iter()
            .map(|term| term_forms(term))
            .collect::<Vec<_>>();
        let matchers = term_matchers(&forms);
        let intrinsic = intrinsic_weights(&terms);
        let mut filename_raw = filename_hits(root, &forms, globs, 10_000)?;
        let mut hits: Vec<SearchHit> = Vec::new();
        // Cap on raw hits scored before ripgrep is killed. ripgrep emits in
        // filesystem-traversal order, not by relevance, so a cap hit early
        // silently drops everything traversed later — a recall bias on large
        // trees. A high ceiling (still bounded, still guarded by SEARCH_TIMEOUT
        // and `--max-count 20` per file) keeps that bias off the common case;
        // an explicit `--glob`/`--diff` scope narrows traversal so it cannot
        // truncate the requested files at all.
        let raw_hit_limit = max_hits.saturating_mul(50).clamp(max_hits, 50_000);
        let pattern = forms
            .iter()
            .flatten()
            .map(|form| regex::escape(form))
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
        apply_globs(&mut command, globs);
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
                .unwrap_or_default();
            // Score on whole-word matches only: "table" must not credit a line
            // that merely contains "constable" or "tableau".
            let matched_terms = matchers
                .iter()
                .enumerate()
                .fold(0u16, |mask, (index, matcher)| {
                    mask | (u16::from(matcher.is_match(text)) << index)
                });
            if matched_terms == 0 {
                continue;
            }
            hits.push(SearchHit {
                path: path.strip_prefix("./").unwrap_or(path).to_owned(),
                line: line_number as usize,
                score: 0,
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
        // Inverse document frequency from content matches: a term found in many
        // files is generic and cheap; a term found in few files is discriminating.
        let total_files = coverage.len().max(1);
        let mut document_frequency = vec![0usize; terms.len()];
        for mask in coverage.values() {
            for index in set_bits(*mask) {
                document_frequency[index] += 1;
            }
        }
        let term_weight = (0..terms.len())
            .map(|index| {
                let df = document_frequency[index].max(1);
                let idf = (1.0 + total_files as f64 / df as f64).log2();
                ((intrinsic[index] as f64) * IDF_SCALE * idf)
                    .round()
                    .max(1.0) as usize
            })
            .collect::<Vec<_>>();
        // A filename match locates the subject file, so fold its IDF-weighted
        // boost into every content hit of that path (lifting the whole file) and
        // keep it as the score for files matched only by name.
        let mut filename_boost = std::collections::HashMap::<String, usize>::new();
        for hit in &filename_raw {
            let boost = filename_boost_of(hit.matched_terms, &term_weight);
            filename_boost
                .entry(hit.path.clone())
                .and_modify(|value| *value = (*value).max(boost))
                .or_insert(boost);
        }
        // Parent directories carry weaker package/module evidence. Only files
        // already present in `coverage` can receive it; a path component match
        // alone never creates a retrieval hit.
        let path_component_boost = coverage
            .keys()
            .filter_map(|path| {
                let mask = directory_match_mask(path, &forms);
                (mask != 0).then(|| {
                    (
                        path.clone(),
                        weight_of(mask, &term_weight) * PATH_COMPONENT_BOOST,
                    )
                })
            })
            .collect::<std::collections::HashMap<_, _>>();
        for hit in &mut hits {
            let line_weight = weight_of(hit.matched_terms, &term_weight);
            let file_weight = coverage
                .get(hit.path.as_str())
                .copied()
                .map(|mask| coverage_weight(mask, &term_weight))
                .unwrap_or_default();
            let name_boost = filename_boost.get(hit.path.as_str()).copied().unwrap_or(0);
            let path_boost = path_component_boost
                .get(hit.path.as_str())
                .copied()
                .unwrap_or(0);
            // Reward the matching line, a smaller bonus for how much of the whole
            // query the file covers, a strong basename locator, and a weak
            // parent-directory package/module signal.
            hit.score = line_weight * 2 + file_weight + name_boost + path_boost;
        }
        // Keep a filename hit only for a file with no content match. Where the
        // file also matches in content, its boost is already folded into those
        // hits; emitting the line-1 filename hit too would surface a useless
        // top-of-file chunk (imports/boilerplate) that beats the real evidence.
        filename_raw.retain(|hit| !coverage.contains_key(hit.path.as_str()));
        for hit in &mut filename_raw {
            hit.score = filename_boost_of(hit.matched_terms, &term_weight);
        }
        hits.append(&mut filename_raw);
        // Sink prose below matching source: a README's broad-vocabulary hit
        // must not lead a code question. Applied to content and filename hits
        // alike, after all scores are final and before ranking.
        for hit in &mut hits {
            if hit.score > 0 && is_prose_path(&hit.path) {
                hit.score = (hit.score * PROSE_SCORE_NUM / PROSE_SCORE_DEN).max(1);
            }
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

fn filename_hits(
    root: &Path,
    forms: &[Vec<String>],
    globs: &[String],
    scan_limit: usize,
) -> Result<Vec<SearchHit>> {
    let mut command = Command::new("rg");
    command.current_dir(root).args(["--files"]);
    apply_globs(&mut command, globs);
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
        if has_binary_extension(&path) {
            continue;
        }
        // The strong locator signal is intentionally basename-only. Parent
        // directories are scored separately and more weakly, only for files
        // that already have content matches.
        let normalized = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&path)
            .to_lowercase();
        let matched_terms = forms.iter().enumerate().fold(0u16, |mask, (index, forms)| {
            mask | (u16::from(forms.iter().any(|form| normalized.contains(form.as_str()))) << index)
        });
        if matched_terms != 0 {
            // Scored later against inverse-document-frequency term weights so a
            // filename hit cannot outrank a real content match.
            hits.push(SearchHit {
                path,
                line: 1,
                score: 0,
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

fn directory_match_mask(path: &str, forms: &[Vec<String>]) -> u16 {
    let components = Path::new(path)
        .parent()
        .into_iter()
        .flat_map(|parent| parent.iter())
        .filter_map(|component| component.to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    forms.iter().enumerate().fold(0u16, |mask, (index, forms)| {
        let matched = components
            .iter()
            .any(|component| forms.iter().any(|form| component.contains(form.as_str())));
        mask | (u16::from(matched) << index)
    })
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
        // Keep three-plus-char terms, and two-char terms that are clearly
        // identifiers rather than filler: an all-caps initialism (`IO`, `DB`,
        // `UI`) or one carrying a digit (`S3`, `v2`). Generic two-letter words
        // (`is`, `of`, `to`) stay dropped.
        .filter(|term| {
            let count = term.chars().count();
            count >= 3
                || (count == 2
                    && (term.chars().all(|character| character.is_ascii_uppercase())
                        || term.chars().any(|character| character.is_ascii_digit())))
        })
        .filter(|term| !stop.contains(term.to_lowercase().as_str()))
        .filter(|term| seen.insert(term.to_lowercase()))
        .take(12)
        .map(ToOwned::to_owned)
        .collect()
}

/// Compiles one case-insensitive matcher per query term. The term (in any of
/// its cross-convention spellings, see [`term_forms`]) must begin at a token
/// boundary — the string start or a non-alphanumeric character, which includes
/// `_` so `validate` still matches `validate_question`. The tail is left open
/// so morphology still counts: `path` matches `paths`, `exclude` matches
/// `excludes`. Requiring the left boundary rejects the substring noise the
/// caller reported, e.g. `table` inside `constable` or `comfortable`.
fn term_matchers(forms: &[Vec<String>]) -> Vec<Regex> {
    forms
        .iter()
        .map(|forms| {
            let alternation = forms
                .iter()
                .map(|form| regex::escape(form))
                .collect::<Vec<_>>()
                .join("|");
            Regex::new(&format!(r"(?i)(?:^|[^\p{{L}}\p{{N}}])(?:{alternation})"))
                .expect("valid term regex")
        })
        .collect()
}

/// Alternate spellings of one query term across identifier conventions.
/// A compound identifier appears in code as camelCase, snake_case or
/// kebab-case (kebab doubles as URL-path style), so a query in one
/// convention must recover matches written in another: `ExcelGridSelector`
/// also matches `excel_grid_selector` and `excel-grid-selector`. All forms
/// are stemmed and lowercased; matching itself is case-insensitive.
fn term_forms(term: &str) -> Vec<String> {
    let words = split_words(term);
    let mut forms = vec![stem(term)];
    if words.len() > 1 {
        for joined in [words.join("_"), words.join("-"), words.concat()] {
            forms.push(stem(&joined));
        }
    }
    let mut seen = HashSet::new();
    forms
        .into_iter()
        .filter(|form| seen.insert(form.clone()))
        .collect()
}

/// Splits a compound identifier into words on `_`, `-`, `.`, `:`, `/`, and
/// camelCase boundaries — lower-to-upper starts a word, and an uppercase run
/// only continues while the next character is not lowercase, so `HTTPServer`
/// splits as `HTTP`, `Server` and `v2Router` as `v2`, `Router`.
fn split_words(term: &str) -> Vec<String> {
    let chars: Vec<char> = term.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, &character) in chars.iter().enumerate() {
        if matches!(character, '_' | '-' | '.' | ':' | '/') {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if character.is_uppercase() && !current.is_empty() {
            // A word boundary at an uppercase character happens when the
            // previous character is not uppercase (lower/digit → upper) or
            // when the next one is lowercase (end of an acronym run), so
            // `HTTPServer` splits as `HTTP`, `Server`.
            let previous_upper = current
                .chars()
                .last()
                .is_some_and(|last| last.is_uppercase());
            let next_lower = chars.get(index + 1).is_some_and(|next| next.is_lowercase());
            if !previous_upper || next_lower {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Base weight before inverse-document-frequency scaling. Identifier-shaped
/// terms (camelCase, snake_case, namespaced, or long) are far more likely to be
/// what the caller actually meant than short lowercase words.
fn intrinsic_weights(terms: &[String]) -> Vec<usize> {
    terms
        .iter()
        .map(|term| if is_identifier_like(term) { 3 } else { 1 })
        .collect()
}

fn is_identifier_like(term: &str) -> bool {
    term.len() >= 8
        || term.contains('_')
        || term.contains("::")
        || term.contains('.')
        || term.chars().any(|c| c.is_ascii_uppercase())
}

/// Naive suffix stemmer: drops a trailing plural/verb `s`/`es` so a query in one
/// grammatical number still matches code written in the other. Deliberately
/// conservative — it only trims, never rewrites, to avoid surprising matches.
fn stem(term: &str) -> String {
    let lower = term.to_lowercase();
    if lower.len() > 4 && lower.ends_with("es") {
        lower[..lower.len() - 2].to_owned()
    } else if lower.len() > 3
        && lower.ends_with('s')
        // Latinate singulars end in a consonant + `s` that is not a plural
        // marker: `status`, `focus`, `bonus`, `axis`, `basis`, `analysis`.
        // Stripping it would coin `statu`, whose open-tailed matcher then
        // catches `statute`/`statutory`. Leave `-ss`/`-us`/`-is` intact.
        && !lower.ends_with("ss")
        && !lower.ends_with("us")
        && !lower.ends_with("is")
    {
        lower[..lower.len() - 1].to_owned()
    } else {
        lower
    }
}

fn set_bits(mask: u16) -> impl Iterator<Item = usize> {
    (0..u16::BITS as usize).filter(move |index| mask & (1 << index) != 0)
}

fn weight_of(mask: u16, term_weight: &[usize]) -> usize {
    set_bits(mask)
        .filter_map(|index| term_weight.get(index).copied())
        .sum()
}

/// File-coverage bonus with diminishing returns: the file's single strongest
/// (rarest) matched term at full weight, plus the remaining matched terms at
/// [`COVERAGE_TAIL_DIVISOR`]. Unlike a flat sum, breadth of generic terms
/// cannot stand in for the rare discriminating term a file is missing.
fn coverage_weight(mask: u16, term_weight: &[usize]) -> usize {
    let mut weights = set_bits(mask)
        .filter_map(|index| term_weight.get(index).copied())
        .collect::<Vec<_>>();
    weights.sort_unstable_by(|left, right| right.cmp(left));
    match weights.split_first() {
        Some((first, rest)) => first + rest.iter().sum::<usize>() / COVERAGE_TAIL_DIVISOR,
        None => 0,
    }
}

/// Filename-match boost: the matched terms' IDF weight scaled by [`FILENAME_BOOST`].
fn filename_boost_of(mask: u16, term_weight: &[usize]) -> usize {
    weight_of(mask, term_weight) * FILENAME_BOOST
}

pub(crate) fn has_binary_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .is_some_and(|extension| BINARY_EXTENSIONS.contains(&extension.as_str()))
}

/// True for documentation/prose files, by extension (`.md`, `.rst`, ...) or by
/// a well-known stem (`README`, `CHANGELOG`, ...) regardless of extension.
fn is_prose_path(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if let Some(extension) = Path::new(&name).extension().and_then(|ext| ext.to_str())
        && PROSE_EXTENSIONS.contains(&extension)
    {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(&name);
    PROSE_STEMS.contains(&stem)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::application::ports::CodeSearch;

    use super::RipgrepSearch;

    #[test]
    fn coverage_weight_favors_the_rare_term_over_breadth_of_generic_ones() {
        use super::coverage_weight;
        // term_weight: index 0 is a rare/discriminating term (weight 20),
        // indices 1..=3 are generic (weight 3 each).
        let term_weight = [20usize, 3, 3, 3];
        // File R covers only the rare term; file G covers the three generic ones.
        let rare_only = coverage_weight(0b0001, &term_weight);
        let generic_breadth = coverage_weight(0b1110, &term_weight);
        // Under a flat sum, breadth (9) would beat the rare term (20 already wins
        // here, but the point is the discounted tail keeps it decisive): the file
        // holding the rare term must outrank the one merely covering many generic
        // terms.
        assert!(rare_only > generic_breadth);
        // The discount applies to the tail: rare + three generic = 20 + (3+3+3)/3.
        assert_eq!(coverage_weight(0b1111, &term_weight), 20 + (3 + 3 + 3) / 3);
    }

    #[test]
    fn removes_common_query_words() {
        assert_eq!(
            RipgrepSearch.terms("Dónde está authentication validation?"),
            ["authentication", "validation"]
        );
    }

    #[test]
    fn keeps_short_identifier_terms_but_drops_generic_two_letter_words() {
        use super::query_terms;
        // `IO` (all-caps), `S3` and `v2` (carry a digit) are identifiers worth
        // searching for; `db`/`is`/`of` are lowercase filler and stay dropped.
        assert_eq!(query_terms("IO db S3 v2 is of"), ["IO", "S3", "v2"]);
    }

    #[test]
    fn stem_leaves_latinate_singulars_intact() {
        use super::stem;
        assert_eq!(stem("paths"), "path");
        assert_eq!(stem("classes"), "class");
        // Not plurals: stripping the trailing `s` would coin a false stem.
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("focus"), "focus");
        assert_eq!(stem("basis"), "basis");
    }

    #[test]
    fn splits_compound_identifiers_into_words() {
        use super::split_words;
        let words = |term: &str| split_words(term).join(" ");
        assert_eq!(words("ExcelGridSelector"), "Excel Grid Selector");
        assert_eq!(words("excel_grid_selector"), "excel grid selector");
        assert_eq!(words("excel-grid-selector"), "excel grid selector");
        assert_eq!(
            words("createTableListTableTransaction"),
            "create Table List Table Transaction"
        );
        assert_eq!(words("HTTPServer"), "HTTP Server");
        assert_eq!(words("v2Router"), "v2 Router");
        assert_eq!(words("table"), "table");
    }

    #[test]
    fn compound_terms_match_across_naming_conventions() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("snake.rs"),
            "fn excel_grid_selector() {}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("camel.rs"),
            "const createTableListTableTransaction = 1;\n",
        )
        .unwrap();
        fs::write(
            root.path().join("route.ts"),
            "fetch('/create-table-list-table-transaction');\n",
        )
        .unwrap();
        let hits = |question: &str| {
            RipgrepSearch
                .search(root.path(), question, 20, &[])
                .unwrap()
                .into_iter()
                .map(|hit| hit.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(hits("ExcelGridSelector"), vec!["snake.rs".to_owned()]);
        assert_eq!(
            hits("create_table_list_table_transaction"),
            vec!["camel.rs".to_owned(), "route.ts".to_owned()]
        );
    }

    #[test]
    fn mixed_case_terms_still_match_lowercase_content() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("code.rs"), "let token = auth_value;").unwrap();
        let hits = RipgrepSearch
            .search(root.path(), "Auth token", 10, &[])
            .unwrap();
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
            .search(root.path(), "bounded-search-marker", 7, &[])
            .unwrap();
        assert_eq!(hits.len(), 7);
    }

    #[test]
    fn token_scoring_ignores_infix_substring_noise() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("noise.rs"), "let constable = comfortable;").unwrap();
        fs::write(root.path().join("hit.rs"), "let table_name = 1;").unwrap();
        let hits = RipgrepSearch.search(root.path(), "table", 10, &[]).unwrap();
        // Token-prefix match: `table` credits `table_name` but not the `table`
        // buried inside `constable` / `comfortable`.
        assert!(hits.iter().any(|hit| hit.path == "hit.rs"));
        assert!(!hits.iter().any(|hit| hit.path == "noise.rs"));
    }

    #[test]
    fn filename_hits_ignore_parent_directory_names() {
        use super::{filename_hits, term_forms};

        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("matcher")).unwrap();
        fs::write(root.path().join("matcher/noise.rs"), "").unwrap();
        fs::write(root.path().join("actual-matcher.rs"), "").unwrap();

        let hits = filename_hits(root.path(), &[term_forms("matcher")], &[], 20).unwrap();
        assert!(hits.iter().any(|hit| hit.path == "actual-matcher.rs"));
        assert!(hits.iter().all(|hit| hit.path != "matcher/noise.rs"));
    }

    #[test]
    fn parent_directory_signal_only_boosts_existing_content_hits() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("a")).unwrap();
        fs::create_dir(root.path().join("z-matcher")).unwrap();
        fs::write(root.path().join("a/other.rs"), "fn selected_glob() {}\n").unwrap();
        fs::write(
            root.path().join("z-matcher/related.rs"),
            "fn selected_glob() {}\n",
        )
        .unwrap();
        fs::write(root.path().join("z-matcher/noise.rs"), "").unwrap();

        let hits = RipgrepSearch
            .search(root.path(), "matcher selected glob", 20, &[])
            .unwrap();
        assert_eq!(
            hits.first().map(|hit| hit.path.as_str()),
            Some("z-matcher/related.rs")
        );
        assert!(hits.iter().all(|hit| hit.path != "z-matcher/noise.rs"));
    }

    #[test]
    fn rare_identifier_outranks_generic_term() {
        let root = tempdir().unwrap();
        for index in 0..8 {
            fs::write(
                root.path().join(format!("common-{index}.rs")),
                "fn handle() { success(); }\n",
            )
            .unwrap();
        }
        fs::write(
            root.path().join("target.rs"),
            "fn createTableListTableTransaction() {}\n",
        )
        .unwrap();
        let hits = RipgrepSearch
            .search(
                root.path(),
                "success createTableListTableTransaction",
                20,
                &[],
            )
            .unwrap();
        assert_eq!(hits.first().map(|hit| hit.path.as_str()), Some("target.rs"));
    }

    #[test]
    fn binary_extension_files_are_never_ranked() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("create-table.png"), [0u8, 1, 2, 3]).unwrap();
        fs::write(
            root.path().join("create_table.rs"),
            "fn create_table() {}\n",
        )
        .unwrap();
        let hits = RipgrepSearch
            .search(root.path(), "create table", 10, &[])
            .unwrap();
        assert!(hits.iter().all(|hit| hit.path != "create-table.png"));
    }

    #[test]
    fn caller_include_glob_cannot_readmit_excluded_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("packages/app/src")).unwrap();
        fs::create_dir_all(root.path().join("packages/app/node_modules/dep")).unwrap();
        fs::write(
            root.path().join("packages/app/src/config.js"),
            "const SECRET_TOKEN = read();\n",
        )
        .unwrap();
        // A nested dependency file and a secrets file that a naive include glob
        // (`packages/**`) would otherwise re-admit past the built-in excludes.
        fs::write(
            root.path().join("packages/app/node_modules/dep/index.js"),
            "const SECRET_TOKEN = 1;\n",
        )
        .unwrap();
        fs::write(
            root.path().join("packages/app/.env"),
            "SECRET_TOKEN=super-secret\n",
        )
        .unwrap();
        let hits = RipgrepSearch
            .search(root.path(), "SECRET_TOKEN", 20, &["packages/**".to_owned()])
            .unwrap();
        assert!(hits.iter().any(|hit| hit.path.contains("src/config.js")));
        assert!(hits.iter().all(|hit| !hit.path.contains("node_modules")));
        assert!(hits.iter().all(|hit| !hit.path.contains(".env")));
    }

    #[test]
    fn prose_files_rank_below_matching_code() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/handler.rs"), "fn marker() {}\n").unwrap();
        // A README matches the same term but must not lead a code question; the
        // path also sorts before src/ on ties, so only the penalty can reorder.
        fs::write(root.path().join("README.md"), "The marker section.\n").unwrap();
        let hits = RipgrepSearch
            .search(root.path(), "marker", 10, &[])
            .unwrap();
        assert_eq!(
            hits.first().map(|hit| hit.path.as_str()),
            Some("src/handler.rs")
        );
        // Down-weighted, not excluded: the doc still appears.
        assert!(hits.iter().any(|hit| hit.path == "README.md"));
    }

    #[test]
    fn user_glob_scopes_the_search() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("docs")).unwrap();
        fs::write(root.path().join("src/code.rs"), "let marker = 1;").unwrap();
        fs::write(root.path().join("docs/notes.md"), "marker\n").unwrap();
        let hits = RipgrepSearch
            .search(root.path(), "marker", 10, &["!docs/**".to_owned()])
            .unwrap();
        assert!(hits.iter().any(|hit| hit.path.contains("code.rs")));
        assert!(hits.iter().all(|hit| !hit.path.contains("notes.md")));
    }
}
