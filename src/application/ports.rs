use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::domain::{
    DenseRecall, FileChange, InstallReport, Limits, LineRange, LoadedDocuments, MetricRecord,
    SearchHit, WorkerRequest, WorkerResponse,
};

/// Resolves the enclosing self-contained block of a hit line — a function,
/// method, or class with its signature, decorators, and doc-comments — so a
/// retrieved chunk is structurally complete rather than a fixed line window.
/// Implementations may use a language grammar; a heuristic fallback covers the
/// rest. Returns `None` when no block fits within `max_span`, and the caller
/// keeps its fixed context window.
pub trait StructureResolver {
    fn enclosing_block(
        &self,
        path: &Path,
        lines: &[String],
        line: usize,
        max_span: usize,
    ) -> Option<LineRange>;

    /// Every self-contained block in a file, for whole-file chunking by the
    /// dense index: each definition (AST) or a fixed window (heuristic), none
    /// exceeding `max_span` lines. Overlaps are the implementation's to avoid.
    fn all_blocks(&self, path: &Path, lines: &[String], max_span: usize) -> Vec<LineRange>;
}

pub trait DocumentLoader {
    fn load(&self, root: &Path, paths: &[PathBuf], limits: &Limits) -> Result<LoadedDocuments>;
}

/// Reads which files changed locally (tracked modifications against a base
/// ref plus untracked files), with paths relative to the given root.
pub trait ChangeSource {
    fn changes(&self, cwd: &Path, base: &str) -> Result<Vec<FileChange>>;
}

pub trait CodeSearch {
    fn terms(&self, question: &str) -> Vec<String>;
    fn search(
        &self,
        root: &Path,
        question: &str,
        max_hits: usize,
        globs: &[String],
    ) -> Result<Vec<SearchHit>>;
    fn available(&self) -> bool;
}

pub trait ContextWorker {
    fn analyze(&self, request: &WorkerRequest, api_key: &str) -> Result<WorkerResponse>;
}

/// Embeds a batch of texts into vectors, one per input in order. The dense
/// index owns the model, key, transport, and batching; the application only
/// fuses rankings, never touching raw vectors. Optional: built for `--semantic`.
pub trait Embedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Recalls the chunks of a whole repository whose meaning matches the question,
/// from a persistent embedding index. This surfaces relevant code the lexical
/// search never hit (few shared terms), which retrieval then fuses with the
/// lexical ranking. Optional: only the opt-in `--semantic` path constructs one.
pub trait DenseIndex {
    fn recall(
        &self,
        question: &str,
        root: &Path,
        globs: &[String],
        limits: &Limits,
    ) -> Result<DenseRecall>;
}

/// Scores how directly each candidate answers the question, in `[0, 1]` and in
/// the same order — a cross-encoder-style relevance judgement. The application
/// reorders the top candidates by this score before packing the budget. The
/// implementation owns the model, key, and transport. Optional: only the opt-in
/// `--rerank` path constructs one.
pub trait Reranker {
    fn scores(&self, question: &str, candidates: &[String]) -> Result<Vec<f32>>;
}

pub trait CredentialResolver {
    fn resolve(&self) -> Result<ResolvedCredential>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedCredential {
    pub api_key: String,
    pub source: String,
}

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key itself is never rendered; only where it came from.
        f.debug_struct("ResolvedCredential")
            .field("api_key", &"***")
            .field("source", &self.source)
            .finish()
    }
}

pub trait MetricsSink {
    fn record(&self, metric: &MetricRecord) -> Result<()>;
}

pub trait HostInstaller {
    fn install(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<InstallReport>>;
}
