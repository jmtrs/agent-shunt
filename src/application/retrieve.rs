use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, bail};

use crate::{
    application::ports::{CodeSearch, ContextWorker, CredentialResolver, DocumentLoader},
    domain::{Document, Limits, LineRange, RetrieveResult, RetrievedChunk, ScanResult},
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
}

/// Executes bounded retrieval under a strict evidence token budget: any
/// retrieved chunk that does not fit the budget is skipped entirely, never
/// truncated, so selected evidence is always complete source context.
pub fn execute(
    search: &dyn CodeSearch,
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
    let terms = search.terms(&input.question);
    if terms.is_empty() {
        bail!("question contains no searchable terms");
    }
    let hits = search.search(&input.cwd, &input.question, input.max_hits, &input.globs)?;
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

pub fn execute_analyzed(
    search: &dyn CodeSearch,
    loader: &dyn DocumentLoader,
    credentials: &dyn CredentialResolver,
    worker: &dyn ContextWorker,
    input: &RetrieveInput,
    model: &str,
    fallback_models: &[String],
) -> Result<(ScanResult, usize, bool)> {
    let (_, documents) = execute(search, loader, input)?;
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use anyhow::Result;

    use crate::{
        application::ports::{CodeSearch, DocumentLoader},
        domain::{Document, Limits, LineRange, LoadedDocuments, SearchHit},
    };

    use super::{RetrieveInput, execute};

    struct Search;
    impl CodeSearch for Search {
        fn terms(&self, _: &str) -> Vec<String> {
            vec!["needle".to_owned()]
        }
        fn search(&self, _: &Path, _: &str, _: usize, _: &[String]) -> Result<Vec<SearchHit>> {
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
            ])
        }
        fn available(&self) -> bool {
            true
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

    #[test]
    fn merges_overlapping_hits_and_preserves_ranges() {
        let (result, documents) = execute(
            &Search,
            &Loader,
            &RetrieveInput {
                question: "needle".to_owned(),
                cwd: PathBuf::from("."),
                limits: Limits::default(),
                budget_tokens: 100,
                context_lines: 1,
                max_hits: 10,
                globs: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunks[0].start_line, 1);
        assert_eq!(result.chunks[0].end_line, 4);
        assert_eq!(documents[0].allowed_ranges[0].end_line, 4);
    }

    #[test]
    fn budget_is_positive_and_never_exceeded() {
        let base = RetrieveInput {
            question: "needle".to_owned(),
            cwd: PathBuf::from("."),
            limits: Limits::default(),
            budget_tokens: 0,
            context_lines: 1,
            max_hits: 10,
            globs: Vec::new(),
        };
        assert!(execute(&Search, &Loader, &base).is_err());

        let mut tiny = base.clone();
        tiny.budget_tokens = 1;
        let (result, documents) = execute(&Search, &Loader, &tiny).unwrap();
        assert!(result.chunks.is_empty());
        assert!(documents.is_empty());
        assert_eq!(result.estimated_tokens, 0);
        assert!(result.truncated);

        let mut too_many_hits = base;
        too_many_hits.budget_tokens = 100;
        too_many_hits.max_hits = 5_001;
        assert!(execute(&Search, &Loader, &too_many_hits).is_err());
    }
}
