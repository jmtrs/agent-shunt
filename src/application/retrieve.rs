use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

use anyhow::{Result, bail};

use crate::{
    application::ports::{
        ChangeSource, CodeSearch, ContextWorker, CredentialResolver, DenseIndex, DocumentLoader,
        QueryExpander, Reranker, StructureResolver,
    },
    domain::{
        DenseHit, Document, FileChange, Limits, LineRange, RetrieveResult, RetrievedChunk,
        ScanResult,
    },
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
    /// MMR relevance/diversity trade-off; defaults to [`MMR_LAMBDA`].
    pub mmr_lambda: f64,
    /// Largest enclosing block a hit may expand into; defaults to
    /// [`MAX_BLOCK_LINES`].
    pub max_block_lines: usize,
    /// Relevance floor as a percentage of the top hit; defaults to
    /// [`MIN_SCORE_PERCENT`].
    pub min_score_percent: usize,
    /// Annotate each chunk with why it was retrieved (lexical vs dense source,
    /// matched query terms). Off by default so the output contract is unchanged.
    pub why: bool,
    /// Pseudo-relevance feedback: mine distinctive identifiers from the
    /// top-ranked files of a first lexical pass and fold them into the search,
    /// so the origin symbol the caller never named still surfaces. No provider.
    pub prf: bool,
    /// Analyze with the reviewer prompt (find risks) instead of the analyst
    /// prompt (answer the question). Only meaningful on the `--analyze` path.
    pub review: bool,
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

/// Unscoped retrieval multiplies the score of hits in git-untracked (brand-new)
/// files by this factor, so freshly written code is not buried under older files
/// that share its vocabulary. Limited to untracked files: modified tracked files
/// are already findable, and boosting them would make ranking depend on whatever
/// happens to be uncommitted. Below [`SCOPE_HUNK_BOOST`]: a nudge for relevance,
/// not the hard subject-of-review focus that `--diff` applies.
const UNTRACKED_FILE_BOOST: usize = 2;

/// Largest enclosing block a hit may expand into. Beyond this the block is a
/// whole impl/class/file, not a focused unit, so the hit keeps its fixed
/// context window instead. Sized to a generous function body: big enough to
/// collapse a scatter of same-function windows into one chunk, bounded enough
/// that one hit never swallows the budget.
pub const MAX_BLOCK_LINES: usize = 48;

/// Most chunks any single file may contribute to the result. Generous enough
/// that a genuinely file-localized question still gets deep coverage, tight
/// enough that one term-heavy file cannot flood the budget with a scatter of
/// weakly-relevant hits and starve the other evidence.
const MAX_CHUNKS_PER_FILE: usize = 6;

/// MMR relevance/diversity trade-off. At 0.7 relevance leads, but a redundant
/// chunk is still pushed down the order enough to lose its slot to fresh
/// evidence under a tight budget.
pub const MMR_LAMBDA: f64 = 0.7;

/// Floor similarity between two chunks of the same file, regardless of content
/// overlap. Reproduces the one-per-path-first spread: another chunk of an
/// already-chosen file is treated as partly redundant, so a not-yet-seen file
/// of comparable relevance is preferred first.
const SAME_PATH_SIM: f64 = 0.5;

/// Chunks scoring below this percentage of the top hit are dropped before
/// selection. The ranked tail of a broad question is weakly related — dozens
/// of files can match faintly — and would otherwise fill the whole token
/// budget with noise that never answers the question.
pub const MIN_SCORE_PERCENT: usize = 15;

/// Per-chunk cost of the JSON envelope the caller actually receives: field
/// names, quotes, structure, newline escaping, and pretty-print whitespace,
/// beyond the raw content and path. Budgeting on content bytes alone
/// undercounts the delivered footprint by ~20% (measured), so the token budget
/// silently overshoots. Calibrated against real serialized output.
const ENVELOPE_TOKENS_PER_CHUNK: usize = 40;

/// Concentration cliff: a single drop of more than this many percentage points
/// of the top score, between one ranked file and the next, marks the edge
/// between the leaders and a faint shoulder. Term-frequency scores decay
/// smoothly, so adjacent files rarely gap sharply — but relative to the top
/// there is often a clear step (leader at 76% of top, shoulder at 38% and
/// below). Everything from that step onward is padding for a well-localized
/// question and is cut; an even spread (a genuinely broad question) has no such
/// step and is left intact.
const CLIFF_GAP_PERCENT: usize = 30;

/// The concentration cliff never cuts below this many files. Term-heavy prose
/// (a README, a CHANGELOG) can falsely outrank the real source and open a cliff
/// right below itself; keeping a floor of files preserves the expected evidence
/// (which is almost always within the top few) while still trimming the shoulder.
const MIN_CLIFF_FILES: usize = 3;

/// Reciprocal Rank Fusion smoothing constant. The standard value: it damps the
/// weight of any single list's top ranks so neither the lexical nor the dense
/// ranking dominates, and fusing on ranks sidesteps the incompatible score
/// scales of BM25-style weights and cosine similarity.
const RRF_K: f64 = 60.0;

/// Most candidates sent to the LLM re-ranker. Reranking is the costly stage, so
/// it is applied only to the head of the ranking (broad recall first, precise
/// reranking of the top) — the standard two-stage design.
const RERANK_TOP_K: usize = 20;

/// Executes bounded retrieval with the dependency-free heuristic block
/// resolver. The default entry point; [`execute_with_resolver`] injects an
/// AST-backed resolver where one is available.
pub fn execute(
    search: &dyn CodeSearch,
    change_source: &dyn ChangeSource,
    loader: &dyn DocumentLoader,
    input: &RetrieveInput,
) -> Result<(RetrieveResult, Vec<Document>)> {
    execute_with_resolver(
        search,
        change_source,
        loader,
        &super::resolver::HeuristicResolver,
        None,
        None,
        None,
        input,
    )
}

/// Executes bounded retrieval under a strict evidence token budget: any
/// retrieved chunk that does not fit the budget is skipped entirely, never
/// truncated, so selected evidence is always complete source context. The
/// `resolver` snaps each hit to its enclosing block.
#[allow(clippy::too_many_arguments)]
pub fn execute_with_resolver(
    search: &dyn CodeSearch,
    change_source: &dyn ChangeSource,
    loader: &dyn DocumentLoader,
    resolver: &dyn StructureResolver,
    index: Option<&dyn DenseIndex>,
    rerank: Option<&dyn Reranker>,
    expander: Option<&dyn QueryExpander>,
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
    // Optional query expansion: a chat model adds related terms (synonyms,
    // likely identifiers) so the lexical search recovers code phrased in other
    // words. Appended after the question's own terms, which keep priority in the
    // bounded term set. Only the opt-in `--expand` path supplies an expander.
    let search_question = if let Some(expander) = expander {
        let extra = expander.expand(&input.question)?;
        if extra.is_empty() {
            search_question
        } else {
            format!("{search_question} {}", extra.join(" "))
        }
    } else {
        search_question
    };
    // Optional pseudo-relevance feedback: a first lexical pass surfaces the
    // files that best match, and the distinctive identifiers concentrated there
    // are folded back into the search so the origin symbol the question never
    // named (a helper, a field) is recovered on the real pass. Local-only: it
    // reuses ripgrep and a bounded read of the leader files, no provider.
    let search_question = if input.prf {
        let mined = prf_terms(search, loader, input, &search_question, &globs)?;
        if mined.is_empty() {
            search_question
        } else {
            format!("{search_question} {}", mined.join(" "))
        }
    } else {
        search_question
    };
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
    } else {
        // Unscoped: surface brand-new code. A matching file that is git-untracked
        // is likely the subject of the current work, yet its lower term frequency
        // can bury it under older files with the same vocabulary. Boost hits on
        // untracked paths only — modified tracked files are already findable, and
        // boosting them would make ranking swing with whatever is uncommitted.
        // Best-effort: only files that already matched are lifted, and outside a
        // git work tree (or on error) nothing changes. The `.git` check avoids
        // spawning git at all when the directory is not a repository.
        if in_git_worktree(&input.cwd)
            && let Ok(changes) = change_source.changes(&input.cwd, "HEAD")
        {
            let untracked = changes
                .iter()
                .filter(|change| change.whole_file)
                .map(|change| change.path.as_str())
                .collect::<std::collections::HashSet<_>>();
            for hit in &mut hits {
                if untracked.contains(hit.path.as_str()) {
                    hit.score = hit.score.saturating_mul(UNTRACKED_FILE_BOOST);
                }
            }
        }
    }
    let mut grouped: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    // Which query terms each file matched, OR-ed across its hits, kept for the
    // `--why` provenance annotation (bit i of the mask is `terms[i]`).
    let mut coverage: BTreeMap<String, u16> = BTreeMap::new();
    for hit in hits {
        *coverage.entry(hit.path.clone()).or_default() |= hit.matched_terms;
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
    // Concentration cliff: cut the ranked files at the first step where the
    // percent-of-top score drops by more than CLIFF_GAP_PERCENT between adjacent
    // files, so a well-localized question returns the leaders near a targeted
    // read instead of a faint shoulder of padding. An even spread (a genuinely
    // broad question) has no such step and is left intact.
    let top = ranked_files.first().map(|file| file.1).unwrap_or(0);
    if top > 0
        && let Some(cliff) = (1..ranked_files.len()).find(|&index| {
            let drop = ranked_files[index - 1]
                .1
                .saturating_sub(ranked_files[index].1);
            drop.saturating_mul(100) > CLIFF_GAP_PERCENT.saturating_mul(top)
        })
    {
        ranked_files.truncate(cliff.max(MIN_CLIFF_FILES));
    }
    ranked_files.truncate(input.limits.max_files);
    let paths = ranked_files
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    let mut loaded = if paths.is_empty() {
        crate::domain::LoadedDocuments {
            documents: Vec::new(),
            total_bytes: 0,
        }
    } else {
        loader.load(&input.cwd, &paths, &input.limits)?
    };

    // Optional dense recall: the persistent index returns whole-repo chunks the
    // question matches by meaning. Their files are merged into the loaded set
    // (so the analyze path can deliver them), and the hits are fused with the
    // lexical ranking below.
    let dense_hits = if let Some(index) = index {
        let recall = index.recall(&input.question, &input.cwd, &globs, &input.limits)?;
        for document in recall.documents {
            if !loaded
                .documents
                .iter()
                .any(|held| held.path == document.path)
            {
                loaded.total_bytes += document.bytes;
                loaded.documents.push(document);
            }
        }
        recall.hits
    } else {
        Vec::new()
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
                // Snap each hit to its enclosing block so several hits in one
                // function collapse to a single chunk instead of a scatter of
                // overlapping fixed windows; fall back to the fixed window for
                // top-level statements or blocks too large to be worth it.
                let range = resolver
                    .enclosing_block(
                        std::path::Path::new(&document.path),
                        &document.lines,
                        *line,
                        input.max_block_lines,
                    )
                    .unwrap_or(LineRange {
                        start_line: line.saturating_sub(input.context_lines).max(1),
                        end_line: (*line + input.context_lines).min(document.line_count),
                    });
                (document.trim_trivial(range), *score)
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
                if delivered_tokens(&document.numbered_range(extended), &path)
                    <= input.budget_tokens
                {
                    *previous = extended;
                    *previous_score = (*previous_score).max(score);
                    continue;
                }
            }
            merged.push((range, score));
        }
        let (source, matched_terms) = if input.why {
            (
                Some("lexical".to_owned()),
                Some(decode_terms(
                    coverage.get(&path).copied().unwrap_or(0),
                    &terms,
                )),
            )
        } else {
            (None, None)
        };
        for (range, score) in merged {
            let content = document.numbered_range(range);
            let estimated_tokens = delivered_tokens(&content, &path);
            candidates.push(RetrievedChunk {
                path: path.clone(),
                start_line: range.start_line,
                end_line: range.end_line,
                score: score * 10 + file_score,
                estimated_tokens,
                content,
                source: source.clone(),
                matched_terms: matched_terms.clone(),
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });

    // Optional semantic pass: fuse the lexical candidates with the dense index's
    // recall so a chunk that answers the question by meaning — not by sharing its
    // exact terms — can rise into the budget, including from files the lexical
    // search never hit. Only the opt-in `--semantic` path supplies hits.
    if !dense_hits.is_empty() {
        fuse_dense(
            &mut candidates,
            &dense_hits,
            &by_path,
            input.budget_tokens,
            input.why,
        );
    }
    let available_count = candidates.len();
    // Optional precise re-ranking: an LLM scores the head of the ranking for how
    // directly each chunk answers the question, and the top is reordered by that
    // score before the budget is packed. Costly, so it runs only on the top-k
    // and only when `--rerank` supplies a reranker.
    if let Some(rerank) = rerank {
        rerank_candidates(rerank, &input.question, &mut candidates)?;
    }
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
            candidate.score.saturating_mul(100) >= max_score.saturating_mul(input.min_score_percent)
        });
    }

    // Maximal Marginal Relevance ordering: greedily take the chunk that best
    // trades relevance against redundancy with what is already chosen, so a
    // second near-identical chunk (boilerplate duplicated across files, or
    // another hit in an already-represented file) yields its slot to fresh
    // evidence. Same-path pairs carry a floor similarity, which reproduces the
    // previous one-per-path-first recall spread — a faintly-ranked expected file
    // still leads its neighbour's repeats — without a separate pass.
    let token_sets = candidates
        .iter()
        .map(|candidate| content_tokens(&candidate.content))
        .collect::<Vec<_>>();
    let max_score_f = max_score.max(1) as f64;
    let mut remaining = (0..candidates.len()).collect::<Vec<_>>();
    let mut order = Vec::with_capacity(candidates.len());
    while !remaining.is_empty() {
        let mut best_position = 0;
        let mut best_value = f64::NEG_INFINITY;
        for (position, &candidate) in remaining.iter().enumerate() {
            let relevance = candidates[candidate].score as f64 / max_score_f;
            let redundancy = order
                .iter()
                .map(|&chosen| similarity(candidate, chosen, &candidates, &token_sets))
                .fold(0.0_f64, f64::max);
            let value = input.mmr_lambda * relevance - (1.0 - input.mmr_lambda) * redundancy;
            // Strict improvement keeps the earliest (higher-scoring, path-sorted)
            // candidate on ties, so ordering stays deterministic.
            if value > best_value + f64::EPSILON {
                best_value = value;
                best_position = position;
            }
        }
        order.push(remaining.remove(best_position));
    }
    let diverse = order
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect::<Vec<_>>();
    // Per-file depth cap: past its first (diversity-guaranteed) slot, one file
    // may contribute at most this many chunks. A file that merely *uses* a
    // heavily-repeated query term (`credential` scattered across a consumer
    // module) otherwise takes a dozen slots of budget away from the file that
    // actually answers the question, even when the budget is not exhausted.
    // The cap frees that budget; omitted chunks still mark the result truncated.
    let mut selected = Vec::new();
    let mut total_tokens = 0;
    let mut per_file: BTreeMap<String, usize> = BTreeMap::new();
    for candidate in diverse {
        let count = per_file.entry(candidate.path.clone()).or_default();
        if *count >= MAX_CHUNKS_PER_FILE {
            continue;
        }
        if total_tokens + candidate.estimated_tokens > input.budget_tokens {
            continue;
        }
        *count += 1;
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
            baseline_bytes: loaded.total_bytes,
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
    resolver: &dyn StructureResolver,
    index: Option<&dyn DenseIndex>,
    rerank: Option<&dyn Reranker>,
    expander: Option<&dyn QueryExpander>,
    credentials: &dyn CredentialResolver,
    worker: &dyn ContextWorker,
    input: &RetrieveInput,
    model: &str,
    fallback_models: &[String],
) -> Result<(ScanResult, usize, bool)> {
    let (_, documents) = execute_with_resolver(
        search,
        change_source,
        loader,
        resolver,
        index,
        rerank,
        expander,
        input,
    )?;
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
        input.review,
    )?;
    let bytes = documents.iter().map(|document| document.bytes).sum();
    Ok((result, bytes, fallback))
}

/// Files whose distinctive identifiers pseudo-relevance feedback mines from.
const PRF_TOP_DOCS: usize = 3;
/// Mined identifiers folded back into the search — kept small so the second
/// pass stays anchored to the caller's intent rather than the leader files'
/// whole vocabulary.
const PRF_TERMS: usize = 5;

/// Pseudo-relevance feedback terms: run a first lexical pass, take the leader
/// files, and return the distinctive compound identifiers concentrated there
/// that the caller did not already search. These name the origin symbol a
/// natural-language question omits (`matchingColumns` for "why is the wrong
/// column filtered"), which the second pass then recovers. Provider-free.
fn prf_terms(
    search: &dyn CodeSearch,
    loader: &dyn DocumentLoader,
    input: &RetrieveInput,
    question: &str,
    globs: &[String],
) -> anyhow::Result<Vec<String>> {
    let hits = search.search(&input.cwd, question, input.max_hits, globs)?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let mut best: BTreeMap<String, usize> = BTreeMap::new();
    for hit in &hits {
        let entry = best.entry(hit.path.clone()).or_default();
        *entry = (*entry).max(hit.score);
    }
    let mut ranked = best.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked.truncate(PRF_TOP_DOCS);
    let paths = ranked
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let loaded = loader.load(&input.cwd, &paths, &input.limits)?;
    // Never re-mine what the caller already searched.
    let existing = search
        .terms(question)
        .into_iter()
        .map(|term| term.to_lowercase())
        .collect::<HashSet<_>>();
    // For each identifier: how many leader files hold it, and its total count.
    // A symbol present in *every* leader is a language idiom (`into_iter`,
    // `is_empty`), not a domain term — those flood a first pass with generic
    // hits, the measured failure of naive relevance feedback. The distinctive
    // origin symbol instead concentrates in one or two of the leaders.
    let doc_total = loaded.documents.len();
    let mut doc_count: HashMap<String, usize> = HashMap::new();
    let mut total_freq: HashMap<String, usize> = HashMap::new();
    for document in &loaded.documents {
        let mut seen_in_doc = HashSet::new();
        for token in identifier_tokens(&document.numbered_content) {
            *total_freq.entry(token.clone()).or_default() += 1;
            if seen_in_doc.insert(token.clone()) {
                *doc_count.entry(token).or_default() += 1;
            }
        }
    }
    let mut scored = total_freq
        .into_iter()
        .filter(|(token, _)| !existing.contains(&token.to_lowercase()))
        // Drop the ubiquitous idiom (present in every leader) once there is more
        // than one leader to compare against.
        .filter(|(token, _)| doc_total < 2 || doc_count[token] < doc_total)
        .collect::<Vec<_>>();
    // Rank by how central each symbol is to the leaders: total uses first (a
    // symbol the answer code leans on recurs, a one-off name like a test
    // function appears once), then fewer holding files (more discriminating),
    // then longer identifiers, then name for a deterministic set.
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| doc_count[&left.0].cmp(&doc_count[&right.0]))
            .then_with(|| right.0.len().cmp(&left.0.len()))
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(scored
        .into_iter()
        .take(PRF_TERMS)
        .map(|(token, _)| token)
        .collect())
}

/// Compound-identifier tokens in `text`: maximal runs of identifier characters,
/// at least four long, that carry an underscore or a camelCase hump. That shape
/// keeps the domain symbols (`filterMethod`, `applyExtraConfig`) while dropping
/// the bare keywords and prose words a raw token count would otherwise surface.
fn identifier_tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|token| token.len() >= 4 && is_compound_identifier(token))
        .map(ToOwned::to_owned)
        .collect()
}

fn is_compound_identifier(token: &str) -> bool {
    if token.contains('_') {
        return true;
    }
    // A camelCase hump: a lowercase letter or digit immediately followed by an
    // uppercase letter, as in `matchingColumns`.
    token
        .chars()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| (pair[0].is_lowercase() || pair[0].is_ascii_digit()) && pair[1].is_uppercase())
}

/// The query terms a coverage mask names: bit `i` corresponds to `terms[i]`.
fn decode_terms(mask: u16, terms: &[String]) -> Vec<String> {
    (0..terms.len())
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| terms[index].clone())
        .collect()
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

/// Fuses the lexical candidate ranking with the dense index's recall via
/// Reciprocal Rank Fusion. Each dense hit either boosts an overlapping lexical
/// candidate or, when the lexical search never touched that region, joins the
/// pool as a fresh candidate. The fused rank becomes each candidate's new score,
/// so the downstream floor, MMR ordering, and budget packing all operate on the
/// hybrid ranking without further change.
fn fuse_dense(
    candidates: &mut Vec<RetrievedChunk>,
    hits: &[DenseHit],
    by_path: &BTreeMap<String, &Document>,
    budget: usize,
    why: bool,
) {
    let lexical_rank = (0..candidates.len()).collect::<Vec<_>>();
    let mut dense_rank = Vec::new();
    // `hits` arrive sorted by descending similarity, so pushing in order yields
    // the dense ranking directly.
    for hit in hits {
        if let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.path == hit.path && overlaps(candidate, hit))
        {
            if !dense_rank.contains(&index) {
                dense_rank.push(index);
            }
            continue;
        }
        let Some(document) = by_path.get(&hit.path) else {
            continue;
        };
        if hit.range.end_line > document.line_count {
            continue;
        }
        let content = document.numbered_range(hit.range);
        let estimated_tokens = delivered_tokens(&content, &hit.path);
        // A chunk that cannot ever fit the budget is not worth ranking.
        if estimated_tokens > budget {
            continue;
        }
        candidates.push(RetrievedChunk {
            path: hit.path.clone(),
            start_line: hit.range.start_line,
            end_line: hit.range.end_line,
            score: 0,
            estimated_tokens,
            content,
            // Recalled by meaning, not by any shared query term: mark it so a
            // caller sees the chunk earned its place from the semantic index.
            source: why.then(|| "dense".to_owned()),
            matched_terms: why.then(Vec::new),
        });
        dense_rank.push(candidates.len() - 1);
    }
    let fused = reciprocal_rank_fusion(&[lexical_rank, dense_rank], RRF_K);
    let count = candidates.len();
    for (position, (index, _)) in fused.iter().enumerate() {
        // Fused rank -> descending integer score the rest of the pipeline reads.
        // Scores become dense linear ranks, so the downstream `MIN_SCORE_PERCENT`
        // floor trims by rank position here, not by ratio to the top term score —
        // intended: after fusion the lexical magnitudes are no longer comparable.
        candidates[*index].score = count - position;
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
    });
}

/// Whether a lexical candidate's line span overlaps a dense hit's, so the two
/// refer to the same region and should count as one fused candidate.
fn overlaps(candidate: &RetrievedChunk, hit: &DenseHit) -> bool {
    candidate.start_line <= hit.range.end_line && hit.range.start_line <= candidate.end_line
}

/// Reorders the top-k candidates by an LLM relevance score, lifting them above
/// the untouched tail so the budget packs the model-preferred chunks first. The
/// tail keeps its ranking; only the head is re-judged, bounding the LLM cost.
fn rerank_candidates(
    rerank: &dyn Reranker,
    question: &str,
    candidates: &mut [RetrievedChunk],
) -> Result<()> {
    let head = candidates.len().min(RERANK_TOP_K);
    if head < 2 {
        return Ok(());
    }
    let texts = candidates[..head]
        .iter()
        .map(|candidate| candidate.content.clone())
        .collect::<Vec<_>>();
    let scores = rerank.scores(question, &texts)?;
    if scores.len() != head {
        bail!(
            "reranker returned {} scores for {} candidates",
            scores.len(),
            head
        );
    }
    // Order the head by descending relevance, ties broken by the prior rank so
    // the reorder is stable and deterministic.
    let mut order = (0..head).collect::<Vec<_>>();
    order.sort_by(|&left, &right| {
        scores[right]
            .partial_cmp(&scores[left])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.cmp(&right))
    });
    // Score the head above the tail's best so its new order survives the final
    // sort while the tail's relative ranking is preserved.
    let tail_max = candidates
        .get(head)
        .map(|candidate| candidate.score)
        .unwrap_or(0);
    for (position, &index) in order.iter().enumerate() {
        candidates[index].score = tail_max + head - position;
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
    });
    Ok(())
}

/// Reciprocal Rank Fusion: combines several ranked lists (each a sequence of
/// candidate indices, best first) into one, scoring every candidate by the sum
/// of `1 / (k + rank)` across the lists it appears in. Returns `(index, score)`
/// pairs sorted by fused score, ties broken by index for determinism.
fn reciprocal_rank_fusion(lists: &[Vec<usize>], k: f64) -> Vec<(usize, f64)> {
    let mut scores: BTreeMap<usize, f64> = BTreeMap::new();
    for list in lists {
        for (rank, &index) in list.iter().enumerate() {
            *scores.entry(index).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut fused = scores.into_iter().collect::<Vec<_>>();
    fused.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.0.cmp(&right.0))
    });
    fused
}

/// Tokens the caller actually pays for a chunk: its numbered content and path,
/// plus the JSON envelope. Budgeting and the reported `estimatedTokens` both
/// use this so the token budget matches the delivered footprint instead of
/// undercounting by the envelope.
fn delivered_tokens(content: &str, path: &str) -> usize {
    estimate_tokens(content) + estimate_tokens(path) + ENVELOPE_TOKENS_PER_CHUNK
}

/// Approximate BPE token count of delivered text. Word runs tokenize at roughly
/// four characters each (min one token), symbol runs at two (dense punctuation
/// only partly merges), and whitespace is free but breaks runs. This tracks a
/// real tokenizer far better than a flat `len / 4`, which undercounts the
/// symbol-dense punctuation of source code and so silently lets the token
/// budget overshoot the delivered footprint. It errs conservative (never below
/// a real count), so the budget is a ceiling the output cannot breach.
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

/// Lowercased identifier-ish tokens of a chunk's content, for redundancy
/// comparison. The line-number prefixes and short tokens are dropped so two
/// chunks are judged similar by the identifiers they share, not their line
/// numbering or punctuation.
fn content_tokens(content: &str) -> std::collections::HashSet<String> {
    content
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| token.len() >= 3 && !token.chars().all(|character| character.is_numeric()))
        .map(str::to_lowercase)
        .collect()
}

/// Jaccard overlap of two token sets, in `[0, 1]`.
fn jaccard(
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
fn similarity(
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
fn in_git_worktree(cwd: &std::path::Path) -> bool {
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

    use std::collections::BTreeMap;

    use crate::{
        application::ports::{ChangeSource, CodeSearch, DocumentLoader},
        domain::{
            DenseHit, Document, FileChange, Limits, LineRange, LoadedDocuments, RetrievedChunk,
            SearchHit,
        },
    };

    use super::{
        ChangeScope, MAX_BLOCK_LINES, MIN_SCORE_PERCENT, MMR_LAMBDA, RetrieveInput, decode_terms,
        estimate_tokens, execute, fuse_dense, identifier_tokens, is_compound_identifier,
        reciprocal_rank_fusion,
    };

    #[test]
    fn identifier_tokens_keeps_compound_symbols_and_drops_plain_words() {
        let tokens = identifier_tokens("let matchingColumns = filter_method(return, value);");
        assert!(tokens.contains(&"matchingColumns".to_owned()));
        assert!(tokens.contains(&"filter_method".to_owned()));
        // Bare keywords and short lowercase words carry no hump or underscore.
        assert!(!tokens.contains(&"return".to_owned()));
        assert!(!tokens.contains(&"value".to_owned()));
        assert!(!tokens.contains(&"let".to_owned()));
    }

    #[test]
    fn compound_identifier_needs_a_hump_or_underscore() {
        assert!(is_compound_identifier("matchingColumns"));
        assert!(is_compound_identifier("query_terms"));
        assert!(is_compound_identifier("v2Router"));
        // A lowercase run or an all-caps run is not a compound identifier.
        assert!(!is_compound_identifier("function"));
        assert!(!is_compound_identifier("HTML"));
    }

    #[test]
    fn decode_terms_maps_mask_bits_to_terms() {
        let terms = vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()];
        // bits 0 and 2 set -> alpha and gamma, in term order.
        assert_eq!(decode_terms(0b101, &terms), vec!["alpha", "gamma"]);
        assert_eq!(decode_terms(0, &terms), Vec::<String>::new());
    }

    #[test]
    fn reciprocal_rank_fusion_rewards_agreement_across_lists() {
        // One list ranks A>B>C, the other C>A>B. A appears near the top of both,
        // so it wins; C's single first place lifts it above B.
        let fused = reciprocal_rank_fusion(&[vec![0, 1, 2], vec![2, 0, 1]], 60.0);
        let order = fused.iter().map(|(index, _)| *index).collect::<Vec<_>>();
        assert_eq!(order, vec![0, 2, 1]);
    }

    struct MockRerank(Vec<f32>);
    impl crate::application::ports::Reranker for MockRerank {
        fn scores(&self, _question: &str, candidates: &[String]) -> Result<Vec<f32>> {
            Ok(self.0[..candidates.len()].to_vec())
        }
    }

    #[test]
    fn rerank_lifts_the_model_preferred_chunk_to_the_top() {
        // Lexical order a > b > c; the reranker judges c most relevant, so it
        // must lead after reranking, the rest following its score order.
        let chunk = |path: &str, score: usize| RetrievedChunk {
            path: path.to_owned(),
            start_line: 1,
            end_line: 1,
            score,
            estimated_tokens: 10,
            content: format!("{path} body"),
            source: None,
            matched_terms: None,
        };
        let mut candidates = vec![chunk("a", 100), chunk("b", 90), chunk("c", 80)];
        let rerank = MockRerank(vec![0.1, 0.2, 0.9]);
        super::rerank_candidates(&rerank, "question", &mut candidates).unwrap();
        let order = candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["c", "b", "a"]);
    }

    struct MockExpander(Vec<String>);
    impl crate::application::ports::QueryExpander for MockExpander {
        fn expand(&self, _question: &str) -> Result<Vec<String>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn expansion_appends_terms_to_the_lexical_search() {
        struct RecordingSearch(Mutex<String>);
        impl CodeSearch for RecordingSearch {
            fn terms(&self, question: &str) -> Vec<String> {
                vec![question.to_owned()]
            }
            fn search(
                &self,
                _root: &Path,
                question: &str,
                _max: usize,
                _globs: &[String],
            ) -> Result<Vec<SearchHit>> {
                *self.0.lock().unwrap() = question.to_owned();
                Ok(vec![SearchHit {
                    path: "a.rs".to_owned(),
                    line: 1,
                    score: 5,
                    matched_terms: 1,
                }])
            }
            fn available(&self) -> bool {
                true
            }
        }
        struct OneDoc;
        impl DocumentLoader for OneDoc {
            fn load(&self, _r: &Path, _p: &[PathBuf], _l: &Limits) -> Result<LoadedDocuments> {
                Ok(LoadedDocuments {
                    total_bytes: 20,
                    documents: vec![Document {
                        path: "a.rs".to_owned(),
                        bytes: 20,
                        line_count: 1,
                        lines: vec!["fn a() { work(); }".to_owned()],
                        numbered_content: String::new(),
                        allowed_ranges: Vec::new(),
                    }],
                })
            }
        }
        let search = RecordingSearch(Mutex::new(String::new()));
        let expander = MockExpander(vec!["synonymterm".to_owned()]);
        let resolver = crate::application::resolver::HeuristicResolver;
        super::execute_with_resolver(
            &search,
            &Changes(Vec::new()),
            &OneDoc,
            &resolver,
            None,
            None,
            Some(&expander),
            &input(),
        )
        .unwrap();
        let recorded = search.0.lock().unwrap().clone();
        assert!(
            recorded.contains("synonymterm"),
            "expanded term missing from search: {recorded}"
        );
        assert!(
            recorded.contains("needle"),
            "original term dropped: {recorded}"
        );
    }

    #[test]
    fn unscoped_retrieval_boosts_hits_in_changed_files() {
        struct TwoHitSearch;
        impl CodeSearch for TwoHitSearch {
            fn terms(&self, _: &str) -> Vec<String> {
                vec!["needle".to_owned()]
            }
            fn search(&self, _: &Path, _: &str, _: usize, _: &[String]) -> Result<Vec<SearchHit>> {
                Ok(vec![
                    SearchHit {
                        path: "old.rs".to_owned(),
                        line: 1,
                        score: 10,
                        matched_terms: 1,
                    },
                    SearchHit {
                        path: "new.rs".to_owned(),
                        line: 1,
                        score: 6,
                        matched_terms: 1,
                    },
                ])
            }
            fn available(&self) -> bool {
                true
            }
        }
        struct TwoDocs;
        impl DocumentLoader for TwoDocs {
            fn load(&self, _: &Path, _: &[PathBuf], _: &Limits) -> Result<LoadedDocuments> {
                let doc = |path: &str| Document {
                    path: path.to_owned(),
                    bytes: 8,
                    line_count: 1,
                    lines: vec!["needle".to_owned()],
                    numbered_content: String::new(),
                    allowed_ranges: Vec::new(),
                };
                Ok(LoadedDocuments {
                    total_bytes: 16,
                    documents: vec![doc("old.rs"), doc("new.rs")],
                })
            }
        }
        // new.rs is untracked; its raw score (6) is below old.rs (10), but the
        // changed-file boost (x2 -> 12) lifts it above.
        let changed = Changes(vec![FileChange {
            path: "new.rs".to_owned(),
            hunks: Vec::new(),
            whole_file: true,
            changed_lines: "needle".to_owned(),
        }]);
        let resolver = crate::application::resolver::HeuristicResolver;
        let input = RetrieveInput {
            budget_tokens: 1000,
            context_lines: 0,
            ..input()
        };
        let (result, _) = super::execute_with_resolver(
            &TwoHitSearch,
            &changed,
            &TwoDocs,
            &resolver,
            None,
            None,
            None,
            &input,
        )
        .unwrap();
        let order = result
            .chunks
            .iter()
            .map(|chunk| chunk.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order.first(),
            Some(&"new.rs"),
            "changed file not boosted above older match: {order:?}"
        );
    }

    #[test]
    fn dense_fusion_injects_a_new_chunk_and_boosts_it() {
        // Lexical pool holds a.rs and b.rs. The dense index recalls c.rs (a file
        // the lexical search never hit) as its top match, so fusion must inject
        // c.rs and rank it above the lexically weaker b.rs.
        let chunk = |path: &str, score: usize| RetrievedChunk {
            path: path.to_owned(),
            start_line: 1,
            end_line: 1,
            score,
            estimated_tokens: 10,
            content: format!("{path} body"),
            source: None,
            matched_terms: None,
        };
        let mut candidates = vec![chunk("a.rs", 100), chunk("b.rs", 90)];
        let doc = Document {
            path: "c.rs".to_owned(),
            bytes: 12,
            line_count: 1,
            lines: vec!["dense body".to_owned()],
            numbered_content: String::new(),
            allowed_ranges: Vec::new(),
        };
        let mut by_path: BTreeMap<String, &Document> = BTreeMap::new();
        by_path.insert("c.rs".to_owned(), &doc);
        let hits = vec![DenseHit {
            path: "c.rs".to_owned(),
            range: LineRange {
                start_line: 1,
                end_line: 1,
            },
            similarity: 0.9,
        }];
        fuse_dense(&mut candidates, &hits, &by_path, 10_000, false);
        assert!(
            candidates.iter().any(|candidate| candidate.path == "c.rs"),
            "dense-only chunk was not injected"
        );
        let order = candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>();
        // a.rs leads both rankings; c.rs (dense #1) outranks b.rs.
        assert_eq!(order[0], "a.rs");
        let c_pos = order.iter().position(|&p| p == "c.rs").unwrap();
        let b_pos = order.iter().position(|&p| p == "b.rs").unwrap();
        assert!(
            c_pos < b_pos,
            "dense recall did not outrank weak lexical: {order:?}"
        );
    }

    #[test]
    fn estimate_tokens_counts_symbol_dense_code_above_a_flat_quarter() {
        // Punctuation-heavy code tokenizes near 1:1, so the estimate must exceed
        // the old `len / 4`, which undercounted and let the budget overshoot.
        let code = "let x = foo(a, b).bar()?;";
        assert!(estimate_tokens(code) > code.len() / 4);
        // Whitespace is free: indentation does not inflate the count.
        assert_eq!(estimate_tokens("   word"), estimate_tokens("word"));
        // A lone word stays cheap.
        assert_eq!(estimate_tokens("path"), 1);
    }

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
            mmr_lambda: MMR_LAMBDA,
            max_block_lines: MAX_BLOCK_LINES,
            min_score_percent: MIN_SCORE_PERCENT,
            why: false,
            prf: false,
            review: false,
        }
    }

    struct FloodSearch;
    impl CodeSearch for FloodSearch {
        fn terms(&self, _: &str) -> Vec<String> {
            vec!["needle".to_owned()]
        }
        fn search(&self, _: &Path, _: &str, _: usize, _: &[String]) -> Result<Vec<SearchHit>> {
            // Eight well-separated hits in one file, so each snaps to its own
            // chunk (no merging) and the per-file cap is what limits selection.
            Ok((0..8)
                .map(|index| SearchHit {
                    path: "src/flood.rs".to_owned(),
                    line: index * 4 + 2,
                    score: 5,
                    matched_terms: 1,
                })
                .collect())
        }
        fn available(&self) -> bool {
            true
        }
    }

    struct FloodLoader;
    impl DocumentLoader for FloodLoader {
        fn load(&self, _: &Path, _: &[PathBuf], _: &Limits) -> Result<LoadedDocuments> {
            // 32 top-level lines, each a standalone `needle` statement, so hits
            // never share an enclosing block or merge.
            let lines = (0..32)
                .map(|index| format!("let needle{index} = {index};"))
                .collect::<Vec<_>>();
            Ok(LoadedDocuments {
                total_bytes: 256,
                documents: vec![Document {
                    path: "src/flood.rs".to_owned(),
                    bytes: 256,
                    line_count: lines.len(),
                    numbered_content: String::new(),
                    lines,
                    allowed_ranges: Vec::new(),
                }],
            })
        }
    }

    struct DupSearch;
    impl CodeSearch for DupSearch {
        fn terms(&self, _: &str) -> Vec<String> {
            vec!["needle".to_owned()]
        }
        fn search(&self, _: &Path, _: &str, _: usize, _: &[String]) -> Result<Vec<SearchHit>> {
            // Three files, one hit each; b.rs is a near-duplicate of the
            // top-scoring a.rs, c.rs is distinct and scores lowest.
            Ok(vec![
                SearchHit {
                    path: "a.rs".to_owned(),
                    line: 1,
                    score: 10,
                    matched_terms: 1,
                },
                SearchHit {
                    path: "b.rs".to_owned(),
                    line: 1,
                    score: 9,
                    matched_terms: 1,
                },
                SearchHit {
                    path: "c.rs".to_owned(),
                    line: 1,
                    score: 8,
                    matched_terms: 1,
                },
            ])
        }
        fn available(&self) -> bool {
            true
        }
    }

    struct DupLoader;
    impl DocumentLoader for DupLoader {
        fn load(&self, _: &Path, paths: &[PathBuf], _: &Limits) -> Result<LoadedDocuments> {
            let content = |path: &str| match path {
                // a.rs and b.rs share every identifier (jaccard 1); c.rs shares none.
                "c.rs" => "needle foxtrot golf hotel india juliet",
                _ => "needle alpha bravo charlie delta echo",
            };
            let documents = paths
                .iter()
                .map(|path| {
                    let name = path.to_string_lossy().into_owned();
                    let line = content(&name).to_owned();
                    Document {
                        path: name,
                        bytes: line.len(),
                        line_count: 1,
                        numbered_content: String::new(),
                        lines: vec![line],
                        allowed_ranges: Vec::new(),
                    }
                })
                .collect();
            Ok(LoadedDocuments {
                documents,
                total_bytes: 200,
            })
        }
    }

    #[test]
    fn mmr_prefers_a_distinct_chunk_over_a_higher_scoring_near_duplicate() {
        // Budget fits exactly two of the three equal-cost chunks. Score order
        // would take a.rs then its near-duplicate b.rs; MMR takes a.rs then the
        // distinct c.rs instead.
        let tight = RetrieveInput {
            budget_tokens: 130,
            context_lines: 0,
            ..input()
        };
        let (result, _) = execute(&DupSearch, &Changes(Vec::new()), &DupLoader, &tight).unwrap();
        let paths = result
            .chunks
            .iter()
            .map(|chunk| chunk.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"a.rs"));
        assert!(
            paths.contains(&"c.rs"),
            "MMR kept the near-duplicate: {paths:?}"
        );
        assert!(!paths.contains(&"b.rs"));
    }

    #[test]
    fn one_file_cannot_exceed_the_per_file_chunk_cap() {
        let ample = RetrieveInput {
            budget_tokens: 10_000,
            context_lines: 0,
            ..input()
        };
        let (result, _) =
            execute(&FloodSearch, &Changes(Vec::new()), &FloodLoader, &ample).unwrap();
        let from_flood = result
            .chunks
            .iter()
            .filter(|chunk| chunk.path == "src/flood.rs")
            .count();
        assert_eq!(from_flood, super::MAX_CHUNKS_PER_FILE);
        // Budget had ample room, so the omission is due to the cap and the
        // result is honestly marked truncated.
        assert!(result.truncated);
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

    /// One hit per listed file, at line 2, with the given score.
    struct ListSearch(Vec<(&'static str, usize)>);
    impl CodeSearch for ListSearch {
        fn terms(&self, _: &str) -> Vec<String> {
            vec!["needle".to_owned()]
        }
        fn search(&self, _: &Path, _: &str, _: usize, _: &[String]) -> Result<Vec<SearchHit>> {
            Ok(self
                .0
                .iter()
                .map(|(path, score)| SearchHit {
                    path: (*path).to_owned(),
                    line: 2,
                    score: *score,
                    matched_terms: 1,
                })
                .collect())
        }
        fn available(&self) -> bool {
            true
        }
    }

    /// Loads a four-line document for every listed path. Paths the ranking
    /// dropped are simply never looked up, so returning extras is harmless.
    struct ListLoader(Vec<&'static str>);
    impl DocumentLoader for ListLoader {
        fn load(&self, _: &Path, _: &[PathBuf], _: &Limits) -> Result<LoadedDocuments> {
            let lines = ["a", "needle", "c", "d"].map(str::to_owned).to_vec();
            let documents = self
                .0
                .iter()
                .map(|path| Document {
                    path: (*path).to_owned(),
                    bytes: 20,
                    line_count: lines.len(),
                    numbered_content: String::new(),
                    lines: lines.clone(),
                    allowed_ranges: vec![LineRange {
                        start_line: 1,
                        end_line: 4,
                    }],
                })
                .collect::<Vec<_>>();
            Ok(LoadedDocuments {
                total_bytes: 20 * self.0.len(),
                documents,
            })
        }
    }

    /// A gradually decaying chain (100, 78, 55, 30, 13): every adjacent step is
    /// a small percent-of-top drop, so the concentration cliff never fires, but
    /// the faintest file falls below the relevance floor (15% of the top hit)
    /// and is dropped before selection — with the result still marked truncated.
    #[test]
    fn relevance_floor_drops_gradual_tail_and_marks_truncated() {
        let search = ListSearch(vec![
            ("src/top.rs", 100),
            ("src/high.rs", 78),
            ("src/mid.rs", 55),
            ("src/low.rs", 30),
            ("src/faint.rs", 13),
        ]);
        let loader = ListLoader(vec![
            "src/top.rs",
            "src/high.rs",
            "src/mid.rs",
            "src/low.rs",
            "src/faint.rs",
        ]);
        let big_budget = RetrieveInput {
            budget_tokens: 1_000,
            ..input()
        };
        let (result, documents) =
            execute(&search, &Changes(Vec::new()), &loader, &big_budget).unwrap();
        let paths = result
            .chunks
            .iter()
            .map(|chunk| chunk.path.as_str())
            .collect::<Vec<_>>();
        // The faint file is floored out; the three stronger files survive.
        assert!(!paths.contains(&"src/faint.rs"));
        assert!(paths.contains(&"src/top.rs"));
        assert!(
            documents
                .iter()
                .all(|document| document.path != "src/faint.rs")
        );
        // Evidence was omitted (the faint file was ranked, then floored).
        assert!(result.truncated);
    }

    /// A steep drop after the leader (100 then a 20/18/16/14 shoulder) opens a
    /// concentration cliff. It trims the faint shoulder, but never below
    /// MIN_CLIFF_FILES, so the two weakest files are cut while the leader and
    /// the top of the shoulder are retained.
    #[test]
    fn concentration_cliff_trims_shoulder_but_keeps_a_floor_of_files() {
        let search = ListSearch(vec![
            ("src/top.rs", 100),
            ("src/s1.rs", 20),
            ("src/s2.rs", 18),
            ("src/s3.rs", 16),
            ("src/s4.rs", 14),
        ]);
        let loader = ListLoader(vec![
            "src/top.rs",
            "src/s1.rs",
            "src/s2.rs",
            "src/s3.rs",
            "src/s4.rs",
        ]);
        let big_budget = RetrieveInput {
            budget_tokens: 1_000,
            ..input()
        };
        let (result, _) = execute(&search, &Changes(Vec::new()), &loader, &big_budget).unwrap();
        let files = result
            .chunks
            .iter()
            .map(|chunk| chunk.path.as_str())
            .collect::<std::collections::HashSet<_>>();
        // Cut below the floor: the two weakest shoulder files are gone.
        assert!(!files.contains("src/s3.rs"));
        assert!(!files.contains("src/s4.rs"));
        // The leader and the top of the shoulder are kept (MIN_CLIFF_FILES = 3).
        assert!(files.contains("src/top.rs"));
        assert_eq!(files.len(), super::MIN_CLIFF_FILES);
    }
}
