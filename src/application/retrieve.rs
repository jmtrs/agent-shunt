use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, bail};

use crate::{
    application::ports::{
        ChangeSource, CodeSearch, ContextWorker, CredentialResolver, DocumentLoader,
    },
    domain::{Document, FileChange, Limits, LineRange, RetrieveResult, RetrievedChunk, ScanResult},
};

#[derive(Debug, Clone)]
pub struct RetrieveInput {
    pub question: String,
    pub cwd: PathBuf,
    pub limits: Limits,
    pub budget_tokens: usize,
    pub context_lines: usize,
    pub max_hits: usize,
    pub globs: Vec<String>,
    /// When set, restricts retrieval to locally changed files so a review
    /// question covers exactly what the caller touched, not the whole tree.
    pub scope: Option<ChangeScope>,
}

/// Restricts retrieval to locally changed files (tracked changes against a
/// base ref plus untracked files).
#[derive(Debug, Clone)]
pub struct ChangeScope {
    pub base: String,
}

/// The changed lines' text appended to the question for term extraction is
/// capped: only enough to name the touched identifiers, never the full diff.
const MAX_DIFF_TERM_TEXT: usize = 100_000;

/// Hits on changed lines are the subject of a scoped review, so they outrank
/// other hits from the same files by this factor.
const SCOPE_HUNK_BOOST: usize = 3;

/// Chunks scoring below this percentage of the top hit are dropped before
/// selection. The ranked tail of a broad question is weakly related — dozens
/// of files can match faintly — and would otherwise fill the whole token
/// budget with noise that never answers the question.
const MIN_SCORE_PERCENT: usize = 15;

/// Executes bounded retrieval under a strict evidence token budget: any
/// retrieved chunk that does not fit the budget is skipped entirely, never
/// truncated, so selected evidence is always complete source context.
pub fn execute(
    search: &dyn CodeSearch,
    change_source: &dyn ChangeSource,
    loader: &dyn DocumentLoader,
    input: &RetrieveInput,
) -> Result<(RetrieveResult, Vec<Document>)> {
    super::scan::validate_question(&input.question, &input.limits)?;
    if input.budget_tokens == 0 {
        bail!("budget-tokens must be positive");
    }
    if input.budget_tokens > input.limits.max_request_bytes.div_ceil(4) {
        bail!("budget-tokens exceeds the configured request limit");
    }
    if input.max_hits == 0 || input.max_hits > 5_000 {
        bail!("max-hits must be between 1 and 5000");
    }
    if input.context_lines > 500 {
        bail!("context-lines must not exceed 500");
    }
    if !search.available() {
        bail!("ripgrep (rg) is required for automatic retrieval");
    }
    let (scope, search_question) = match &input.scope {
        Some(scope) => {
            let changes = change_source.changes(&input.cwd, &scope.base)?;
            if changes.is_empty() {
                bail!(
                    "no local changes against {} — nothing to scope the search",
                    scope.base
                );
            }
            // Changed-line text goes first: term extraction keeps a bounded
            // number of terms, and in a scoped review the touched identifiers
            // are what should rank hits, with the question adding intent.
            let mut question = String::new();
            for change in &changes {
                question.push_str(&change.changed_lines);
                question.push('\n');
            }
            question.push_str(&input.question);
            (
                Some(changes),
                question.chars().take(MAX_DIFF_TERM_TEXT).collect(),
            )
        }
        None => (None, input.question.clone()),
    };
    let mut globs = input.globs.clone();
    if let Some(changes) = &scope {
        // Exact-path include globs keep the search itself inside the scope
        // instead of diluting the hit limit on out-of-scope files. By design
        // the scope wins over caller exclude globs: a review must see every
        // changed file even if a caller exclude would hide it (the built-in
        // EXCLUDED_GLOBS still apply, so secrets stay out).
        globs.extend(changes.iter().map(|change| change.path.clone()));
    }
    let terms = search.terms(&search_question);
    if terms.is_empty() {
        bail!("question contains no searchable terms");
    }
    let mut hits = search.search(&input.cwd, &search_question, input.max_hits, &globs)?;
    if let Some(changes) = &scope {
        hits.retain(|hit| changes.iter().any(|change| change.path == hit.path));
        for hit in &mut hits {
            if let Some(change) = changes.iter().find(|change| change.path == hit.path)
                && hit_in_changed_lines(hit, change)
            {
                hit.score = hit.score.saturating_mul(SCOPE_HUNK_BOOST);
            }
        }
    }
    let mut grouped: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for hit in hits {
        grouped
            .entry(hit.path)
            .or_default()
            .push((hit.line, hit.score));
    }

    let mut ranked_files = grouped
        .iter()
        .map(|(path, hits)| {
            (
                path.clone(),
                hits.iter().map(|(_, score)| *score).max().unwrap_or(0),
            )
        })
        .collect::<Vec<_>>();
    ranked_files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked_files.truncate(input.limits.max_files);
    let paths = ranked_files
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    let loaded = if paths.is_empty() {
        crate::domain::LoadedDocuments {
            documents: Vec::new(),
            total_bytes: 0,
        }
    } else {
        loader.load(&input.cwd, &paths, &input.limits)?
    };

    let by_path = loaded
        .documents
        .iter()
        .map(|document| (document.path.clone(), document))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    for (path, file_score) in ranked_files {
        let Some(document) = by_path.get(&path) else {
            continue;
        };
        let mut ranges = grouped[&path]
            .iter()
            .map(|(line, score)| {
                (
                    LineRange {
                        start_line: line.saturating_sub(input.context_lines).max(1),
                        end_line: (*line + input.context_lines).min(document.line_count),
                    },
                    *score,
                )
            })
            .collect::<Vec<_>>();
        ranges.sort_by_key(|(range, _)| range.start_line);
        let mut merged: Vec<(LineRange, usize)> = Vec::new();
        for (range, score) in ranges {
            if let Some((previous, previous_score)) = merged.last_mut()
                && range.start_line <= previous.end_line + 1
            {
                let extended = LineRange {
                    start_line: previous.start_line,
                    end_line: previous.end_line.max(range.end_line),
                };
                // Keep merged chunks within the evidence budget: chained hits
                // in one file must not grow into a chunk that can never fit,
                // starving the top-ranked file of the whole budget.
                if estimate_tokens(&document.numbered_range(extended)) <= input.budget_tokens {
                    *previous = extended;
                    *previous_score = (*previous_score).max(score);
                    continue;
                }
            }
            merged.push((range, score));
        }
        for (range, score) in merged {
            let content = document.numbered_range(range);
            let estimated_tokens = estimate_tokens(&content);
            candidates.push(RetrievedChunk {
                path: path.clone(),
                start_line: range.start_line,
                end_line: range.end_line,
                score: score * 10 + file_score,
                estimated_tokens,
                content,
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });

    let available_count = candidates.len();
    // Relevance floor: drop the ranked tail whose score is a small fraction of
    // the top hit before anything is selected, so weakly-related files never
    // consume the token budget. `available_count` is captured above, so any
    // floored chunk still marks the result truncated (evidence was omitted).
    let max_score = candidates
        .first()
        .map(|candidate| candidate.score)
        .unwrap_or(0);
    if max_score > 0 {
        candidates.retain(|candidate| {
            candidate.score.saturating_mul(100) >= max_score.saturating_mul(MIN_SCORE_PERCENT)
        });
    }

    // Diversity: each file gets one slot before any file repeats, so several
    // sources are represented instead of the budget draining into one file.
    // Extra chunks follow in score order, so a file with more strong hits still
    // gets depth once every file has been seen. This one-per-path-first pass is
    // the recall guarantee — a faintly-ranked but expected file keeps a slot
    // rather than being starved by repeated hits from a term-heavy neighbour.
    let mut diverse = Vec::with_capacity(candidates.len());
    let mut deferred = Vec::new();
    let mut represented = std::collections::HashSet::new();
    for candidate in candidates {
        if represented.insert(candidate.path.clone()) {
            diverse.push(candidate);
        } else {
            deferred.push(candidate);
        }
    }
    diverse.extend(deferred);
    let mut selected = Vec::new();
    let mut total_tokens = 0;
    for candidate in diverse {
        if total_tokens + candidate.estimated_tokens > input.budget_tokens {
            continue;
        }
        total_tokens += candidate.estimated_tokens;
        selected.push(candidate);
        if total_tokens >= input.budget_tokens {
            break;
        }
    }

    let selected_documents = documents_from_chunks(&selected, &loaded.documents);
    Ok((
        RetrieveResult {
            version: 1,
            query_terms: terms,
            truncated: selected.len() < available_count,
            chunks: selected,
            estimated_tokens: total_tokens,
        },
        selected_documents,
    ))
}

// Wiring requires the full adapter set plus input and model chain; collapsing
// them into a struct would obscure the call site for no design gain.
#[allow(clippy::too_many_arguments)]
pub fn execute_analyzed(
    search: &dyn CodeSearch,
    change_source: &dyn ChangeSource,
    loader: &dyn DocumentLoader,
    credentials: &dyn CredentialResolver,
    worker: &dyn ContextWorker,
    input: &RetrieveInput,
    model: &str,
    fallback_models: &[String],
) -> Result<(ScanResult, usize, bool)> {
    let (_, documents) = execute(search, change_source, loader, input)?;
    if documents.is_empty() {
        bail!("automatic retrieval found no relevant source chunks within the token budget");
    }
    let (result, fallback) = super::scan::analyze_documents(
        credentials,
        worker,
        &input.question,
        &documents,
        &input.limits,
        model,
        fallback_models,
    )?;
    let bytes = documents.iter().map(|document| document.bytes).sum();
    Ok((result, bytes, fallback))
}

fn documents_from_chunks(chunks: &[RetrievedChunk], originals: &[Document]) -> Vec<Document> {
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

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// A hit counts as inside the change when the file is wholly new (`whole_file`,
/// untracked) or the hit line falls inside a changed range. A tracked file
/// whose diff is pure deletions has empty hunks but is *not* whole-file, so
/// none of its hits get the boost.
fn hit_in_changed_lines(hit: &crate::domain::SearchHit, change: &FileChange) -> bool {
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

    use crate::{
        application::ports::{ChangeSource, CodeSearch, DocumentLoader},
        domain::{Document, FileChange, Limits, LineRange, LoadedDocuments, SearchHit},
    };

    use super::{ChangeScope, RetrieveInput, execute};

    struct Search {
        globs: Mutex<Vec<Vec<String>>>,
    }
    impl Search {
        fn new() -> Self {
            Self {
                globs: Mutex::new(Vec::new()),
            }
        }
    }
    impl CodeSearch for Search {
        fn terms(&self, _: &str) -> Vec<String> {
            vec!["needle".to_owned()]
        }
        fn search(&self, _: &Path, _: &str, _: usize, globs: &[String]) -> Result<Vec<SearchHit>> {
            self.globs.lock().unwrap().push(globs.to_vec());
            Ok(vec![
                SearchHit {
                    path: "src/a.rs".to_owned(),
                    line: 2,
                    score: 2,
                    matched_terms: 1,
                },
                SearchHit {
                    path: "src/a.rs".to_owned(),
                    line: 3,
                    score: 1,
                    matched_terms: 1,
                },
                SearchHit {
                    path: "src/out.rs".to_owned(),
                    line: 1,
                    score: 5,
                    matched_terms: 1,
                },
            ])
        }
        fn available(&self) -> bool {
            true
        }
    }

    struct Changes(Vec<FileChange>);
    impl ChangeSource for Changes {
        fn changes(&self, _: &Path, base: &str) -> Result<Vec<FileChange>> {
            assert_eq!(base, "HEAD");
            Ok(self.0.clone())
        }
    }

    struct Loader;
    impl DocumentLoader for Loader {
        fn load(&self, _: &Path, _: &[PathBuf], _: &Limits) -> Result<LoadedDocuments> {
            let lines = ["zero", "needle", "also needle", "end"]
                .map(str::to_owned)
                .to_vec();
            Ok(LoadedDocuments {
                total_bytes: 25,
                documents: vec![Document {
                    path: "src/a.rs".to_owned(),
                    bytes: 25,
                    line_count: lines.len(),
                    numbered_content: String::new(),
                    lines,
                    allowed_ranges: vec![LineRange {
                        start_line: 1,
                        end_line: 4,
                    }],
                }],
            })
        }
    }

    fn input() -> RetrieveInput {
        RetrieveInput {
            question: "needle".to_owned(),
            cwd: PathBuf::from("."),
            limits: Limits::default(),
            budget_tokens: 100,
            context_lines: 1,
            max_hits: 10,
            globs: Vec::new(),
            scope: None,
        }
    }

    #[test]
    fn merges_overlapping_hits_and_preserves_ranges() {
        let (result, documents) =
            execute(&Search::new(), &Changes(Vec::new()), &Loader, &input()).unwrap();
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunks[0].start_line, 1);
        assert_eq!(result.chunks[0].end_line, 4);
        assert_eq!(documents[0].allowed_ranges[0].end_line, 4);
    }

    #[test]
    fn budget_is_positive_and_never_exceeded() {
        let base = RetrieveInput {
            budget_tokens: 0,
            ..input()
        };
        assert!(execute(&Search::new(), &Changes(Vec::new()), &Loader, &base).is_err());

        let tiny = RetrieveInput {
            budget_tokens: 1,
            ..input()
        };
        let (result, documents) =
            execute(&Search::new(), &Changes(Vec::new()), &Loader, &tiny).unwrap();
        assert!(result.chunks.is_empty());
        assert!(documents.is_empty());
        assert_eq!(result.estimated_tokens, 0);
        assert!(result.truncated);

        let too_many_hits = RetrieveInput {
            budget_tokens: 100,
            max_hits: 5_001,
            ..input()
        };
        assert!(
            execute(
                &Search::new(),
                &Changes(Vec::new()),
                &Loader,
                &too_many_hits
            )
            .is_err()
        );
    }

    fn scoped_change() -> FileChange {
        FileChange {
            path: "src/a.rs".to_owned(),
            hunks: vec![LineRange {
                start_line: 2,
                end_line: 2,
            }],
            whole_file: false,
            changed_lines: "needle touched".to_owned(),
        }
    }

    #[test]
    fn scope_filters_out_of_scope_files_and_boosts_changed_lines() {
        let mut scoped = input();
        scoped.scope = Some(ChangeScope {
            base: "HEAD".to_owned(),
        });
        let changes = Changes(vec![scoped_change()]);
        let (result, documents) = execute(&Search::new(), &changes, &Loader, &scoped).unwrap();
        // The higher-scoring out-of-scope file is gone; only src/a.rs remains.
        assert!(documents.iter().all(|document| document.path == "src/a.rs"));
        assert!(result.chunks.iter().all(|chunk| chunk.path == "src/a.rs"));
        // Both in-file hits merge into one chunk spanning lines 1-4.
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.query_terms, vec!["needle".to_owned()]);
    }

    #[test]
    fn scope_passes_changed_files_as_globs_and_boosts_hunk_hits() {
        // Compare chunk scores with the hunk on the hit line versus off it:
        // the in-hunk hit is boosted by SCOPE_HUNK_BOOST.
        let mut scoped = input();
        scoped.scope = Some(ChangeScope {
            base: "HEAD".to_owned(),
        });
        let search = Search::new();
        let (boosted, _) =
            execute(&search, &Changes(vec![scoped_change()]), &Loader, &scoped).unwrap();
        let off_hunk = Changes(vec![FileChange {
            path: "src/a.rs".to_owned(),
            hunks: vec![LineRange {
                start_line: 4,
                end_line: 4,
            }],
            whole_file: false,
            changed_lines: String::new(),
        }]);
        let (plain, _) = execute(&search, &off_hunk, &Loader, &scoped).unwrap();
        assert!(boosted.chunks[0].score > plain.chunks[0].score);
        // The search itself was narrowed to the changed file via a glob.
        let globs = search.globs.lock().unwrap();
        assert_eq!(globs[0], vec!["src/a.rs".to_owned()]);
    }

    #[test]
    fn scope_with_no_changes_fails() {
        let mut scoped = input();
        scoped.scope = Some(ChangeScope {
            base: "HEAD".to_owned(),
        });
        let error = execute(&Search::new(), &Changes(Vec::new()), &Loader, &scoped).unwrap_err();
        assert!(error.to_string().contains("no local changes"));
    }

    /// A faintly-matching file must not survive next to a strong hit: the
    /// relevance floor drops it before selection, yet the result stays marked
    /// truncated because evidence was omitted.
    #[test]
    fn relevance_floor_drops_weak_tail_and_marks_truncated() {
        struct TwoFileSearch;
        impl CodeSearch for TwoFileSearch {
            fn terms(&self, _: &str) -> Vec<String> {
                vec!["needle".to_owned()]
            }
            fn search(
                &self,
                _: &Path,
                _: &str,
                _: usize,
                _: &[String],
            ) -> Result<Vec<SearchHit>> {
                Ok(vec![
                    SearchHit {
                        path: "src/strong.rs".to_owned(),
                        line: 2,
                        score: 100,
                        matched_terms: 1,
                    },
                    SearchHit {
                        path: "src/weak.rs".to_owned(),
                        line: 2,
                        score: 5,
                        matched_terms: 1,
                    },
                ])
            }
            fn available(&self) -> bool {
                true
            }
        }

        struct TwoFileLoader;
        impl DocumentLoader for TwoFileLoader {
            fn load(&self, _: &Path, _: &[PathBuf], _: &Limits) -> Result<LoadedDocuments> {
                let lines = ["a", "needle", "c", "d"].map(str::to_owned).to_vec();
                let doc = |path: &str| Document {
                    path: path.to_owned(),
                    bytes: 20,
                    line_count: lines.len(),
                    numbered_content: String::new(),
                    lines: lines.clone(),
                    allowed_ranges: vec![LineRange {
                        start_line: 1,
                        end_line: 4,
                    }],
                };
                Ok(LoadedDocuments {
                    total_bytes: 40,
                    documents: vec![doc("src/strong.rs"), doc("src/weak.rs")],
                })
            }
        }

        let (result, documents) =
            execute(&TwoFileSearch, &Changes(Vec::new()), &TwoFileLoader, &input()).unwrap();
        assert!(result.chunks.iter().all(|chunk| chunk.path == "src/strong.rs"));
        assert!(documents.iter().all(|document| document.path == "src/strong.rs"));
        // The weak file was ranked but floored out, so the result is truncated.
        assert!(result.truncated);
    }
}
