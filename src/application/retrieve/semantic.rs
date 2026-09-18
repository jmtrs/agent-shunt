use std::collections::BTreeMap;

use crate::domain::{DenseHit, Document, RetrievedChunk};

use super::{SEMANTIC_MIN_RELATIVE_FILE_MARGIN, support::delivered_tokens};

/// Returns the top semantic file only when it is meaningfully separated from
/// the next distinct file. The ratio is relative to the leader's own cosine
/// similarity, avoiding an absolute similarity threshold tied to one model.
fn confident_dense_head_path(hits: &[DenseHit]) -> Option<&str> {
    let head = hits.first()?;
    if !head.similarity.is_finite() || head.similarity <= 0.0 {
        return None;
    }

    let Some(runner_up) = hits.iter().find(|hit| hit.path != head.path) else {
        return Some(head.path.as_str());
    };
    if !runner_up.similarity.is_finite() {
        return None;
    }

    let relative_margin =
        (head.similarity - runner_up.similarity) / head.similarity.abs().max(f32::EPSILON);
    (relative_margin >= SEMANTIC_MIN_RELATIVE_FILE_MARGIN).then_some(head.path.as_str())
}

/// Preserves the lexical head while allowing one novel semantic region to
/// compete with the lexical tail. Dense recall is a bounded escape hatch for
/// evidence the lexical search missed, not a second authority over the ranking:
/// existing lexical scores are never rewritten and dense evidence can never
/// tie or outrank the strongest lexical candidate.
///
/// Only one non-overlapping dense region is admitted, and only when that file
/// is not already represented at or above the lexical score tier dense would
/// receive. Semantic recall may strengthen a weakly represented file, but it
/// must not spend budget deepening a file that is already strong lexically.
pub(super) fn fuse_dense(
    candidates: &mut Vec<RetrievedChunk>,
    hits: &[DenseHit],
    by_path: &BTreeMap<String, &Document>,
    budget: usize,
    why: bool,
) {
    // When lexical retrieval found nothing, semantic recall is the only evidence
    // source. Keep its native order and give every valid, budget-fit hit a score.
    if candidates.is_empty() {
        for (rank, hit) in hits.iter().enumerate() {
            let Some(document) = by_path.get(&hit.path) else {
                continue;
            };
            if hit.range.end_line > document.line_count {
                continue;
            }
            let content = document.numbered_range(hit.range);
            let estimated_tokens = delivered_tokens(&content, &hit.path);
            if estimated_tokens > budget {
                continue;
            }
            candidates.push(RetrievedChunk {
                path: hit.path.clone(),
                start_line: hit.range.start_line,
                end_line: hit.range.end_line,
                score: hits.len().saturating_sub(rank).max(1),
                estimated_tokens,
                content,
                source: why.then(|| "dense".to_owned()),
                matched_terms: why.then(Vec::new),
            });
        }
        return;
    }

    let top_score = candidates[0].score;
    // A zero-scored lexical head carries no meaningful score gap to preserve.
    // Refuse to invent semantic authority in that degenerate case.
    if top_score == 0 {
        return;
    }

    let Some(dense_head_path) = confident_dense_head_path(hits) else {
        return;
    };

    // Dense recall should challenge the weak half of the lexical ranking, not
    // jump directly behind the lexical head. The lower-half boundary adapts to
    // the number and spread of candidates without a benchmark-specific rank or
    // score percentage. Subtract one so an admitted dense region never ties the
    // lexical candidate that defines that boundary.
    let middle_score = candidates[candidates.len() / 2].score;
    let dense_score = middle_score
        .saturating_sub(1)
        .min(top_score.saturating_sub(1));

    // Semantic recall is a side-channel for evidence that lexical retrieval is
    // missing or underweighting. If this file is already represented in the
    // upper half of the lexical ranking, another region from it is depth rather
    // than recall and must not consume fixed evidence budget.
    if candidates
        .iter()
        .any(|candidate| candidate.path == dense_head_path && candidate.score >= middle_score)
    {
        return;
    }

    // Dense hits arrive sorted by descending similarity. Only the confident
    // head file may contribute a region; falling through to a lower-ranked
    // semantic file would spend lexical budget on evidence the confidence test
    // did not actually validate.
    for hit in hits {
        if hit.path != dense_head_path {
            continue;
        }
        if candidates
            .iter()
            .any(|candidate| candidate.path == hit.path && overlaps(candidate, hit))
        {
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
        if estimated_tokens > budget {
            continue;
        }
        candidates.push(RetrievedChunk {
            path: hit.path.clone(),
            start_line: hit.range.start_line,
            end_line: hit.range.end_line,
            score: dense_score,
            estimated_tokens,
            content,
            source: why.then(|| "dense".to_owned()),
            matched_terms: why.then(Vec::new),
        });
        break;
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
