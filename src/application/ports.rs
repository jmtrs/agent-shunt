use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::domain::{
    CodexInstallReport, Limits, LoadedDocuments, MetricRecord, SearchHit, WorkerRequest,
    WorkerResponse,
};

pub trait DocumentLoader {
    fn load(&self, root: &Path, paths: &[PathBuf], limits: &Limits) -> Result<LoadedDocuments>;
}

pub trait CodeSearch {
    fn terms(&self, question: &str) -> Vec<String>;
    fn search(&self, root: &Path, question: &str, max_hits: usize) -> Result<Vec<SearchHit>>;
    fn available(&self) -> bool;
}

pub trait ContextWorker {
    fn analyze(&self, request: &WorkerRequest, api_key: &str) -> Result<WorkerResponse>;
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
    fn install_codex(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<CodexInstallReport>>;
}
