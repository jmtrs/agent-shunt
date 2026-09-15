use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::domain::Limits;

pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
pub const DEFAULT_FALLBACK_MODEL: &str = "z-ai/glm-4.7-flash";
pub const BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Clone)]
pub struct Config {
    pub model: String,
    pub fallback_models: Vec<String>,
    pub codex_homes: Vec<PathBuf>,
    pub claude_homes: Vec<PathBuf>,
    pub base_url: String,
    pub response_format: String,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub local_provider: bool,
    pub limits: Limits,
    pub config_file: PathBuf,
    /// Reasoning-model handling: when true, the worker injects the provider's
    /// "disable thinking" parameter so any reasoning model behaves as a fast,
    /// direct responder (this tool does grounded extraction, not deliberation).
    pub disable_reasoning: bool,
    /// Arbitrary JSON object merged into every worker request body, applied
    /// last so it overrides tool defaults. The escape hatch for any provider
    /// parameter the built-in fields do not cover.
    pub extra_body: Value,
    /// Optional retrieval-tuning overrides. When set they supply the default a
    /// bare `retrieve` uses; an explicit CLI flag still wins. Validated on load.
    pub mmr_lambda: Option<f64>,
    pub max_block_lines: Option<usize>,
    pub min_score_percent: Option<usize>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The API key is never rendered, even in logs and test failures.
        f.debug_struct("Config")
            .field("model", &self.model)
            .field("fallback_models", &self.fallback_models)
            .field("codex_homes", &self.codex_homes)
            .field("claude_homes", &self.claude_homes)
            .field("base_url", &self.base_url)
            .field("response_format", &self.response_format)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("api_key_env", &self.api_key_env)
            .field("local_provider", &self.local_provider)
            .field("limits", &self.limits)
            .field("config_file", &self.config_file)
            .field("disable_reasoning", &self.disable_reasoning)
            .field("extra_body", &self.extra_body)
            .field("mmr_lambda", &self.mmr_lambda)
            .field("max_block_lines", &self.max_block_lines)
            .field("min_score_percent", &self.min_score_percent)
            .finish()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    model: Option<String>,
    fallback_models: Option<Vec<String>>,
    codex_homes: Option<Vec<String>>,
    claude_homes: Option<Vec<String>>,
    base_url: Option<String>,
    response_format: Option<String>,
    api_key: Option<String>,
    api_key_env: Option<String>,
    timeout_ms: Option<u64>,
    max_output_tokens: Option<u64>,
    max_response_bytes: Option<usize>,
    max_question_bytes: Option<usize>,
    max_request_bytes: Option<usize>,
    max_files: Option<usize>,
    max_file_bytes: Option<usize>,
    max_total_bytes: Option<usize>,
    disable_reasoning: Option<bool>,
    extra_body: Option<Value>,
    mmr_lambda: Option<f64>,
    max_block_lines: Option<usize>,
    min_score_percent: Option<usize>,
}

pub fn load(model_override: Option<&str>) -> Result<Config> {
    let config_file = env::var_os("AGENT_SHUNT_CONFIG")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config/agent-shunt/config.json")))
        .context("cannot determine agent-shunt config path")?;
    let stored = if config_file.exists() {
        serde_json::from_slice::<StoredConfig>(&fs::read(&config_file)?)
            .with_context(|| format!("invalid configuration object: {}", config_file.display()))?
    } else {
        StoredConfig::default()
    };
    let base_url = env::var("AGENT_SHUNT_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(stored.base_url)
        .unwrap_or_else(|| BASE_URL.to_owned());
    let base_url = base_url.trim().to_owned();
    if base_url.is_empty() {
        bail!("baseUrl must not be empty");
    }
    let parsed_url = validate_base_url(&base_url)?;
    let local_provider = is_local_host(&parsed_url);
    let response_format = stored
        .response_format
        .as_deref()
        .unwrap_or("json_schema")
        .trim()
        .to_owned();
    if !matches!(response_format.as_str(), "json_schema" | "json_object") {
        bail!("responseFormat must be \"json_schema\" or \"json_object\"");
    }
    let api_key = stored
        .api_key
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty());
    let api_key_env = stored
        .api_key_env
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    let defaults = Limits::default();
    let limits = Limits {
        timeout_ms: bounded(stored.timeout_ms, defaults.timeout_ms, 300_000, "timeoutMs")?,
        max_output_tokens: bounded(
            stored.max_output_tokens,
            defaults.max_output_tokens,
            8_192,
            "maxOutputTokens",
        )?,
        max_response_bytes: bounded(
            stored.max_response_bytes,
            defaults.max_response_bytes,
            4_000_000,
            "maxResponseBytes",
        )?,
        max_question_bytes: bounded(
            stored.max_question_bytes,
            defaults.max_question_bytes,
            64_000,
            "maxQuestionBytes",
        )?,
        max_request_bytes: bounded(
            stored.max_request_bytes,
            defaults.max_request_bytes,
            8_000_000,
            "maxRequestBytes",
        )?,
        max_files: bounded(stored.max_files, defaults.max_files, 200, "maxFiles")?,
        max_file_bytes: bounded(
            stored.max_file_bytes,
            defaults.max_file_bytes,
            5_000_000,
            "maxFileBytes",
        )?,
        max_total_bytes: bounded(
            stored.max_total_bytes,
            defaults.max_total_bytes,
            8_000_000,
            "maxTotalBytes",
        )?,
    };
    let model = model_override
        .map(str::trim)
        .map(ToOwned::to_owned)
        .or(stored.model)
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
    let model = model.trim().to_owned();
    if model.is_empty() {
        bail!("model must not be empty");
    }
    let mut fallback_models = stored
        .fallback_models
        .unwrap_or_else(|| vec![DEFAULT_FALLBACK_MODEL.to_owned()])
        .into_iter()
        .map(|candidate| candidate.trim().to_owned())
        .filter(|candidate| !candidate.is_empty() && candidate != &model)
        .collect::<Vec<_>>();
    fallback_models.dedup();
    let configured_codex_homes = stored.codex_homes.is_some();
    let codex_homes = stored
        .codex_homes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|raw| expand_home(raw))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    if configured_codex_homes && codex_homes.is_empty() {
        bail!("codexHomes must contain at least one non-empty path");
    }
    let configured_claude_homes = stored.claude_homes.is_some();
    let claude_homes = stored
        .claude_homes
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|raw| expand_home(raw))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    if configured_claude_homes && claude_homes.is_empty() {
        bail!("claudeHomes must contain at least one non-empty path");
    }
    let disable_reasoning = stored.disable_reasoning.unwrap_or(false);
    let extra_body = normalize_extra_body(stored.extra_body)?;
    if let Some(lambda) = stored.mmr_lambda
        && !(0.0..=1.0).contains(&lambda)
    {
        bail!("mmrLambda must be between 0.0 and 1.0");
    }
    if let Some(percent) = stored.min_score_percent
        && percent > 100
    {
        bail!("minScorePercent must be between 0 and 100");
    }
    if stored.max_block_lines == Some(0) {
        bail!("maxBlockLines must be a positive integer");
    }
    Ok(Config {
        model,
        fallback_models,
        codex_homes,
        claude_homes,
        base_url,
        response_format,
        api_key,
        api_key_env,
        local_provider,
        limits,
        config_file,
        disable_reasoning,
        extra_body,
        mmr_lambda: stored.mmr_lambda,
        max_block_lines: stored.max_block_lines,
        min_score_percent: stored.min_score_percent,
    })
}

/// Normalizes the optional `extraBody` into an object, rejecting any non-object
/// JSON. Absent or explicit null becomes an empty object (a no-op merge).
fn normalize_extra_body(value: Option<Value>) -> Result<Value> {
    match value {
        None | Some(Value::Null) => Ok(Value::Object(serde_json::Map::new())),
        Some(value @ Value::Object(_)) => Ok(value),
        Some(_) => bail!("extraBody must be a JSON object"),
    }
}

/// Expands a leading `~` to the user's home directory; absolute and relative
/// paths are kept as-is. Whitespace-only input yields no path.
fn expand_home(raw: &str) -> Option<PathBuf> {
    if raw.trim().is_empty() {
        return None;
    }
    if raw.trim() == "~" {
        return dirs::home_dir();
    }
    let Some(rest) = raw.trim().strip_prefix("~/") else {
        return Some(PathBuf::from(raw.trim()));
    };
    dirs::home_dir().map(|home| home.join(rest))
}

/// Accepts any absolute http(s) worker origin. Plain `http` is allowed only
/// for loopback and private-network hosts; everything else must use `https`
/// so credentials are never sent in cleartext to a remote provider.
fn validate_base_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|_| anyhow::anyhow!("baseUrl must be an absolute http(s) URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("baseUrl must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("baseUrl must not embed credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("baseUrl must not include a query or fragment");
    }
    if url.scheme() == "http" && !is_local_host(&url) {
        bail!(
            "plain http is only allowed for loopback or private-network hosts; use https for {}",
            url.host_str().unwrap_or("this host")
        );
    }
    Ok(url)
}

fn is_local_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        Some(url::Host::Ipv6(address)) => {
            // IPv4-mapped addresses (`::ffff:127.0.0.1`) must inherit the
            // IPv4 classification instead of counting as global IPv6.
            if let Some(mapped) = address.to_ipv4_mapped() {
                return mapped.is_loopback() || mapped.is_private() || mapped.is_link_local();
            }
            address.is_loopback() || address.is_unique_local()
        }
        None => false,
    }
}

fn bounded<T>(value: Option<T>, fallback: T, hard_max: T, name: &str) -> Result<T>
where
    T: Copy + Ord + Default + std::fmt::Display,
{
    let value = value.unwrap_or(fallback);
    if value <= T::default() {
        bail!("{name} must be a positive integer");
    }
    if value > hard_max {
        bail!("{name} exceeds hard maximum {hard_max}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{bounded, expand_home, normalize_extra_body, validate_base_url};

    #[test]
    fn accepts_any_https_origin_but_guards_plain_http() {
        assert!(validate_base_url("https://openrouter.ai/api/v1/").is_ok());
        assert!(validate_base_url("https://api.groq.com/openai/v1").is_ok());
        assert!(validate_base_url("http://localhost:11434/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(validate_base_url("http://192.168.1.10:8080/v1").is_ok());
        assert!(validate_base_url("http://[::1]:1234/v1").is_ok());
        assert!(validate_base_url("http://[::ffff:127.0.0.1]/v1").is_ok());
        assert!(validate_base_url("http://[::ffff:192.168.1.10]/v1").is_ok());
        assert!(validate_base_url("http://[fd00::1]/v1").is_ok());
        assert!(validate_base_url("http://[::ffff:8.8.8.8]/v1").is_err());
        assert!(validate_base_url("http://[2001:db8::1]/v1").is_err());
        assert!(validate_base_url("http://example.com/v1").is_err());
        assert!(validate_base_url("ftp://example.com/v1").is_err());
        assert!(validate_base_url("https://user:pass@example.com/v1").is_err());
        assert!(validate_base_url("https://example.com/v1?x=1").is_err());
        assert!(validate_base_url("not a url").is_err());
    }

    #[test]
    fn numeric_limits_fail_closed() {
        assert!(bounded(Some(0usize), 1, 200, "maxFiles").is_err());
        assert!(bounded(Some(201usize), 1, 200, "maxFiles").is_err());
        assert_eq!(bounded(Some(30usize), 1, 200, "maxFiles").unwrap(), 30);
    }

    #[test]
    fn extra_body_must_be_object() {
        use serde_json::json;
        assert!(normalize_extra_body(None).unwrap().is_object());
        assert!(
            normalize_extra_body(Some(serde_json::Value::Null))
                .unwrap()
                .is_object()
        );
        assert_eq!(
            normalize_extra_body(Some(json!({"thinking": {"type": "disabled"}}))).unwrap(),
            json!({"thinking": {"type": "disabled"}})
        );
        assert!(normalize_extra_body(Some(json!([1, 2]))).is_err());
        assert!(normalize_extra_body(Some(json!("nope"))).is_err());
    }

    #[test]
    fn home_expansion_accepts_tilde_absolute_and_relative() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home("~"), Some(home.clone()));
        assert_eq!(expand_home("~/.codex"), Some(home.join(".codex")));
        assert_eq!(
            expand_home("/opt/custom"),
            Some(PathBuf::from("/opt/custom"))
        );
        assert_eq!(
            expand_home("relative/.codex"),
            Some(PathBuf::from("relative/.codex"))
        );
        assert_eq!(expand_home(""), None);
        assert_eq!(expand_home("   "), None);
    }
}
