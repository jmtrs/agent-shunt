use std::{path::PathBuf, time::Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    adapters::{
        codex_install::CodexInstaller, credentials::EnvironmentCredentials,
        filesystem::SecureFilesystem, metrics::JsonlMetrics, openrouter::OpenRouterWorker,
        ripgrep::RipgrepSearch,
    },
    application::{
        ports::{CodeSearch, CredentialResolver, HostInstaller, MetricsSink},
        retrieve::{self, RetrieveInput},
        scan::{self, ScanInput},
    },
    config::Config,
    domain::{MetricRecord, Usage},
};

pub struct Application {
    filesystem: SecureFilesystem,
    search: RipgrepSearch,
    worker: OpenRouterWorker,
    credentials: EnvironmentCredentials,
    metrics: JsonlMetrics,
    installer: CodexInstaller,
}

struct RecordedResult {
    value: Value,
    input_bytes: usize,
    files: usize,
    usage: Option<Usage>,
    model: Option<String>,
    fallback: bool,
}

impl Default for Application {
    fn default() -> Self {
        Self {
            filesystem: SecureFilesystem,
            search: RipgrepSearch,
            worker: OpenRouterWorker,
            credentials: EnvironmentCredentials,
            metrics: JsonlMetrics::new(JsonlMetrics::default_path()),
            installer: CodexInstaller::new(
                std::env::current_exe()
                    .unwrap_or_else(|_| PathBuf::from("/opt/homebrew/bin/agent-shunt")),
            ),
        }
    }
}

impl Application {
    pub fn scan(&self, input: ScanInput, dry_run: bool) -> Result<Value> {
        let started = Instant::now();
        let result = if dry_run {
            scan::dry_run(&self.filesystem, &input).and_then(|(result, bytes)| {
                let files = result.files_read.len();
                Ok(RecordedResult {
                    model: Some(result.model.clone()),
                    value: serde_json::to_value(result)?,
                    input_bytes: bytes,
                    files,
                    usage: None,
                    fallback: false,
                })
            })
        } else {
            scan::execute(&self.filesystem, &self.credentials, &self.worker, &input).and_then(
                |(result, bytes, fallback)| {
                    let files = result.files_read.len();
                    let usage = result.usage.clone();
                    Ok(RecordedResult {
                        model: Some(result.model.clone()),
                        value: serde_json::to_value(result)?,
                        input_bytes: bytes,
                        files,
                        usage,
                        fallback,
                    })
                },
            )
        };
        self.record_result("scan", Some(&input.model), started, &result);
        result.map(|result| result.value)
    }

    pub fn retrieve(&self, input: RetrieveInput, analyze: bool, config: &Config) -> Result<Value> {
        let started = Instant::now();
        let result = if analyze {
            retrieve::execute_analyzed(
                &self.search,
                &self.filesystem,
                &self.credentials,
                &self.worker,
                &input,
                &config.model,
                &config.fallback_models,
            )
            .and_then(|(scan_result, bytes, fallback)| {
                let files = scan_result.files_read.len();
                let usage = scan_result.usage.clone();
                Ok(RecordedResult {
                    model: Some(scan_result.model.clone()),
                    value: serde_json::to_value(scan_result)?,
                    input_bytes: bytes,
                    files,
                    usage,
                    fallback,
                })
            })
        } else {
            retrieve::execute(&self.search, &self.filesystem, &input).and_then(
                |(retrieval_result, documents)| {
                    let input_bytes = documents.iter().map(|document| document.bytes).sum();
                    let files = documents.len();
                    Ok(RecordedResult {
                        value: serde_json::to_value(retrieval_result)?,
                        input_bytes,
                        files,
                        usage: None,
                        model: None,
                        fallback: false,
                    })
                },
            )
        };
        self.record_result(
            "retrieve",
            analyze.then_some(config.model.as_str()),
            started,
            &result,
        );
        result.map(|result| result.value)
    }

    pub fn check(&self, config: &Config) -> Result<Value> {
        let credential = self.credentials.resolve()?;
        Ok(json!({
            "configured": true,
            "remoteValidation": "not-performed",
            "model": config.model,
            "credentialSource": credential.source
        }))
    }

    pub fn doctor(&self, config: &Config) -> Value {
        let credential = self.credentials.resolve();
        json!({
            "version": 1,
            "healthy": credential.is_ok() && self.search.available(),
            "rustImplementation": true,
            "model": config.model,
            "fallbackModels": config.fallback_models,
            "configFile": config.config_file,
            "credential": if credential.is_ok() { "found" } else { "missing" },
            "ripgrep": if self.search.available() { "available" } else { "missing" },
            "remoteValidation": "not-performed"
        })
    }

    pub fn metrics(&self) -> Result<Value> {
        Ok(json!({
            "version": 1,
            "path": self.metrics.path(),
            "summary": self.metrics.summary()?
        }))
    }

    pub fn install_codex(&self, homes: &[PathBuf], hook: bool) -> Result<Value> {
        Ok(serde_json::to_value(
            self.installer.install_codex(homes, hook)?,
        )?)
    }

    fn record_result(
        &self,
        operation: &str,
        model: Option<&str>,
        started: Instant,
        result: &Result<RecordedResult>,
    ) {
        let (success, input_bytes, files, usage, used_model, fallback) = match result {
            Ok(result) => (
                true,
                result.input_bytes,
                result.files,
                result.usage.as_ref(),
                result.model.as_deref().or(model),
                result.fallback,
            ),
            Err(error) => {
                let exhausted = error.downcast_ref::<scan::FallbackExhausted>();
                (
                    false,
                    0,
                    0,
                    exhausted.and_then(|value| value.usage()),
                    model,
                    exhausted.is_some(),
                )
            }
        };
        let metric = MetricRecord {
            timestamp: chrono::Utc::now(),
            operation: operation.to_owned(),
            success,
            model: used_model.map(ToOwned::to_owned),
            duration_ms: started.elapsed().as_millis(),
            input_bytes,
            files,
            prompt_tokens: usage.and_then(|value| value.prompt_tokens),
            completion_tokens: usage.and_then(|value| value.completion_tokens),
            total_tokens: usage.and_then(|value| value.total_tokens),
            cost: usage.and_then(|value| value.cost),
            fallback,
        };
        let _ = self.metrics.record(&metric);
    }
}

pub fn scan_input(
    question: String,
    paths: Vec<PathBuf>,
    cwd: PathBuf,
    config: &Config,
) -> ScanInput {
    ScanInput {
        question,
        paths,
        cwd,
        model: config.model.clone(),
        fallback_models: config.fallback_models.clone(),
        limits: config.limits.clone(),
    }
}
