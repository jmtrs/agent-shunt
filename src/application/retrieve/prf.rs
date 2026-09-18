use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

use crate::application::ports::{CodeSearch, DocumentLoader};

use super::RetrieveInput;

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
pub(super) fn prf_terms(
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
pub(super) fn identifier_tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|token| token.len() >= 4 && is_compound_identifier(token))
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn is_compound_identifier(token: &str) -> bool {
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
