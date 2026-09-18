use anyhow::{Result, bail};

use crate::{application::ports::Reranker, domain::RetrievedChunk};

use super::RERANK_TOP_K;

/// Reorders the top-k candidates by an LLM relevance score, lifting them above
/// the untouched tail so the budget packs the model-preferred chunks first. The
/// tail keeps its ranking; only the head is re-judged, bounding the LLM cost.
pub(super) fn rerank_candidates(
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
