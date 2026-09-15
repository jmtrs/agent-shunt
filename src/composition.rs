use std::{path::PathBuf, time::Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    adapters::{
        claude_install::ClaudeInstaller, codex_install::CodexInstaller,
        credentials::EnvironmentCredentials, filesystem::SecureFilesystem,
        gemini_install::GeminiInstaller, git::GitChangeSource, metrics::JsonlMetrics,
        openai_compatible::OpenAiCompatibleWorker, opencode_install::OpencodeInstaller,
        repo_install::RepoInstaller, ripgrep::RipgrepSearch,
    },
    application::{
        ports::{CodeSearch, CredentialResolver, HostInstaller, MetricsSink},
        retrieve::{self, RetrieveInput},
        scan::{self, ScanInput},
    },
    config::Config,
    domain::{MetricRecord, Usage},
};

/// Repo-level instruction host selected on the `install` subcommand.
pub enum RepoHost {
    AgentsMd,
    Copilot,
    Cursor,
    Cline,
    Roo,
}

pub struct Application {
    filesystem: SecureFilesystem,
    search: RipgrepSearch,
    changes: GitChangeSource,
    metrics: JsonlMetrics,
    codex_installer: CodexInstaller,
    claude_installer: ClaudeInstaller,
    gemini_installer: GeminiInstaller,
    opencode_installer: OpencodeInstaller,
    repo_installer: RepoInstaller,
}

struct RecordedResult {
    value: Value,
    input_bytes: usize,
    files: usize,
    usage: Option<Usage>,
    model: Option<String>,
    fallback: bool,
    /// Savings telemetry for deterministic retrieve: whole size of the loaded
    /// files vs the tokens actually delivered. Zero for every other operation.
    baseline_bytes: usize,
    delivered_tokens: usize,
}

fn default_executable() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/opt/homebrew/bin/agent-shunt"))
}

impl Default for Application {
    fn default() -> Self {
        Self {
            filesystem: SecureFilesystem,
            search: RipgrepSearch,
            changes: GitChangeSource,
            metrics: JsonlMetrics::new(JsonlMetrics::default_path()),
            codex_installer: CodexInstaller::new(default_executable()),
            claude_installer: ClaudeInstaller::new(default_executable()),
            gemini_installer: GeminiInstaller,
            opencode_installer: OpencodeInstaller,
            repo_installer: RepoInstaller,
        }
    }
}

impl Application {
    fn worker_for(&self, config: &Config) -> OpenAiCompatibleWorker {
        OpenAiCompatibleWorker::with_options(
            &config.base_url,
            &config.response_format,
            config.disable_reasoning,
            config.extra_body.clone(),
        )
    }

    fn credentials_for(&self, config: &Config) -> EnvironmentCredentials {
        EnvironmentCredentials::new(
            config.api_key.clone(),
            config.api_key_env.clone(),
            config.local_provider,
        )
    }

    pub fn scan(&self, config: &Config, input: ScanInput, dry_run: bool) -> Result<Value> {
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
                    baseline_bytes: 0,
                    delivered_tokens: 0,
                })
            })
        } else {
            let credentials = self.credentials_for(config);
            let worker = self.worker_for(config);
            scan::execute(&self.filesystem, &credentials, &worker, &input).and_then(
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
                        baseline_bytes: 0,
                        delivered_tokens: 0,
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
            let credentials = self.credentials_for(config);
            let worker = self.worker_for(config);
            retrieve::execute_analyzed(
                &self.search,
                &self.changes,
                &self.filesystem,
                &credentials,
                &worker,
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
                    baseline_bytes: 0,
                    delivered_tokens: 0,
                })
            })
        } else {
            retrieve::execute(&self.search, &self.changes, &self.filesystem, &input).and_then(
                |(retrieval_result, documents)| {
                    let input_bytes = documents.iter().map(|document| document.bytes).sum();
                    let files = documents.len();
                    // Savings = the whole loaded files versus the tokens returned.
                    let baseline_bytes = retrieval_result.baseline_bytes;
                    let delivered_tokens = retrieval_result.estimated_tokens;
                    Ok(RecordedResult {
                        value: serde_json::to_value(retrieval_result)?,
                        input_bytes,
                        files,
                        usage: None,
                        model: None,
                        fallback: false,
                        baseline_bytes,
                        delivered_tokens,
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
        let credential = self.credentials_for(config).resolve()?;
        Ok(json!({
            "configured": true,
            "remoteValidation": "not-performed",
            "baseUrl": config.base_url,
            "responseFormat": config.response_format,
            "model": config.model,
            "credentialSource": credential.source
        }))
    }

    pub fn doctor(&self, config: &Config) -> Value {
        let credential = self.credentials_for(config).resolve();
        let credential_status = match &credential {
            Ok(credential) if credential.api_key.is_empty() => "not-required (local)",
            Ok(_) => "found",
            Err(_) => "missing",
        };
        json!({
            "version": 1,
            "healthy": credential.is_ok() && self.search.available(),
            "rustImplementation": true,
            "baseUrl": config.base_url,
            "responseFormat": config.response_format,
            "model": config.model,
            "fallbackModels": config.fallback_models,
            "configFile": config.config_file,
            "credential": credential_status,
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

    /// Curated worker models for this tool's job: grounded, structured
    /// extraction over supplied chunks — cheap, fast, reliable JSON, and
    /// faithful to the exact line ranges shown. Reasoning is unnecessary here,
    /// so reasoning models are only listed with `disableReasoning` set. Each
    /// entry carries a ready-to-merge `config` fragment.
    pub fn recommend(&self) -> Value {
        json!({
            "version": 1,
            "note": "Set these keys in the config file (default ~/.config/agent-shunt/config.json). `config` fragments are additive. `disableReasoning` auto-injects the provider's disable-thinking parameter; `extraBody` overrides any request field for models the built-in handling does not cover.",
            "criteria": [
                "reliable structured output (json_schema or json_object)",
                "faithful line ranges (findings must cite only supplied lines)",
                "low latency and cost; reasoning adds neither value nor speed here"
            ],
            "recommended": [
                {
                    "model": "deepseek/deepseek-v4-flash",
                    "tested": true,
                    "privacy": "zdr (OpenRouter routes with zero data retention / no training)",
                    "notes": "Default. Fast (~22s), cheap (~$0.0004), valid json_schema, respects line ranges.",
                    "config": {"baseUrl": "https://openrouter.ai/api/v1", "model": "deepseek/deepseek-v4-flash", "responseFormat": "json_schema"}
                },
                {
                    "model": "z-ai/glm-4.7-flash",
                    "tested": true,
                    "privacy": "zdr (OpenRouter)",
                    "notes": "Default fallback. Works with json_schema; occasional provider 429 and stricter line-range failures fall back automatically.",
                    "config": {"baseUrl": "https://openrouter.ai/api/v1", "model": "z-ai/glm-4.7-flash", "responseFormat": "json_schema"}
                },
                {
                    "model": "glm-5.3-flash (z.ai coding plan)",
                    "tested": true,
                    "privacy": "none — source leaves to z.ai (no zdr guarantee); avoid for proprietary code",
                    "notes": "Fast (~3.4s) ONLY with reasoning disabled; otherwise it burns the token budget and truncates JSON. Uses json_object (z.ai does not enforce strict json_schema).",
                    "config": {"baseUrl": "https://api.z.ai/api/coding/paas/v4", "model": "glm-5.3-flash", "responseFormat": "json_object", "disableReasoning": true, "maxOutputTokens": 8192}
                },
                {
                    "model": "local (Ollama / LM Studio / vLLM)",
                    "tested": false,
                    "privacy": "full — nothing leaves the machine",
                    "notes": "Any capable instruct model served locally. No API key required. Use json_object; disableReasoning for reasoning models.",
                    "config": {"baseUrl": "http://localhost:11434/v1", "model": "<your-local-model>", "responseFormat": "json_object"}
                }
            ]
        })
    }

    pub fn install_codex(&self, homes: &[PathBuf], hook: bool) -> Result<Value> {
        Ok(serde_json::to_value(
            self.codex_installer.install(homes, hook)?,
        )?)
    }

    pub fn install_claude(&self, homes: &[PathBuf], hook: bool) -> Result<Value> {
        Ok(serde_json::to_value(
            self.claude_installer.install(homes, hook)?,
        )?)
    }

    pub fn install_gemini(&self, homes: &[PathBuf], hook: bool) -> Result<Value> {
        Ok(serde_json::to_value(
            self.gemini_installer.install(homes, hook)?,
        )?)
    }

    pub fn install_opencode(&self, homes: &[PathBuf], hook: bool) -> Result<Value> {
        Ok(serde_json::to_value(
            self.opencode_installer.install(homes, hook)?,
        )?)
    }

    pub fn install_repo(&self, host: RepoHost, root: &std::path::Path) -> Result<Value> {
        let report = match host {
            RepoHost::AgentsMd => self.repo_installer.install_agents_md(root),
            RepoHost::Copilot => self.repo_installer.install_copilot(root),
            RepoHost::Cursor => self.repo_installer.install_cursor(root),
            RepoHost::Cline => self.repo_installer.install_cline(root),
            RepoHost::Roo => self.repo_installer.install_roo(root),
        }?;
        Ok(serde_json::to_value(report)?)
    }

    fn record_result(
        &self,
        operation: &str,
        model: Option<&str>,
        started: Instant,
        result: &Result<RecordedResult>,
    ) {
        let (
            success,
            input_bytes,
            files,
            usage,
            used_model,
            fallback,
            error_kind,
            baseline_bytes,
            delivered_tokens,
        ) = match result {
            Ok(result) => (
                true,
                result.input_bytes,
                result.files,
                result.usage.as_ref(),
                result.model.as_deref().or(model),
                result.fallback,
                None,
                result.baseline_bytes,
                result.delivered_tokens,
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
                    Some(classify_error(error)),
                    0,
                    0,
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
            error_kind,
            baseline_bytes,
            delivered_tokens,
        };
        let _ = self.metrics.record(&metric);
    }
}

/// Reduces a failure to a stable, privacy-safe category for the metrics log.
/// Matches over the whole error chain (so a wrapped transport/timeout source is
/// still recognized) but returns only a fixed token — never the raw message —
/// so no source path or content is ever persisted. Keep the tokens stable:
/// they are aggregated across runs.
fn classify_error(error: &anyhow::Error) -> String {
    if error.downcast_ref::<scan::FallbackExhausted>().is_some() {
        return "fallback_exhausted".to_owned();
    }
    let chain = error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
        .to_ascii_lowercase();
    let kind = if chain.contains("timed out") || chain.contains("timeout") {
        "timeout"
    } else if let Some(code) = http_status_code(&chain) {
        match code / 100 {
            4 => "http_4xx",
            5 => "http_5xx",
            _ => "http_other",
        }
    } else if chain.contains("worker request failed") || chain.contains("error sending request") {
        "transport"
    } else if chain.contains("non-json") {
        "non_json_response"
    } else if chain.contains("no message content") {
        "empty_content"
    } else if chain.contains("free model routes") {
        "free_route_blocked"
    } else if chain.contains("exceeds") {
        "size_limit"
    } else if chain.contains("credential") || chain.contains("api key") {
        "credential"
    } else if chain.contains("no relevant source") {
        "no_results"
    } else {
        "other"
    };
    kind.to_owned()
}

/// Extracts the numeric status from a `worker HTTP <code>` message, if present.
fn http_status_code(chain: &str) -> Option<u16> {
    let rest = chain.split("worker http ").nth(1)?;
    let digits = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
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

#[cfg(test)]
mod tests {
    use super::classify_error;

    fn kind(message: &str) -> String {
        classify_error(&anyhow::anyhow!(message.to_owned()))
    }

    #[test]
    fn classifies_transport_and_http_statuses() {
        assert_eq!(kind("worker HTTP 429: rate limited"), "http_4xx");
        assert_eq!(kind("worker HTTP 503: upstream unavailable"), "http_5xx");
        assert_eq!(kind("worker request failed"), "transport");
        assert_eq!(
            kind("worker returned non-JSON HTTP 200"),
            "non_json_response"
        );
        assert_eq!(
            kind("worker response has no message content"),
            "empty_content"
        );
    }

    #[test]
    fn timeout_recognized_through_wrapped_source() {
        let wrapped = anyhow::anyhow!("operation timed out").context("worker request failed");
        // Timeout must win over the generic transport message it wraps.
        assert_eq!(classify_error(&wrapped), "timeout");
    }

    #[test]
    fn error_kind_never_leaks_paths() {
        // Filesystem errors carry a path; the classifier must reduce them to a
        // fixed token so the metrics log stays free of source-derived strings.
        let leaky = kind("cannot open path: /Users/secret/project/src/auth.rs");
        assert_eq!(leaky, "other");
        assert!(!leaky.contains('/'));
        assert!(!leaky.contains("path"));
    }
}
