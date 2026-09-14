use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use url::Url;

use crate::domain::Limits;

pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
pub const DEFAULT_FALLBACK_MODEL: &str = "z-ai/glm-4.7-flash";
pub const BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub fallback_models: Vec<String>,
    pub codex_homes: Vec<PathBuf>,
    pub limits: Limits,
    pub config_file: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    model: Option<String>,
    fallback_models: Option<Vec<String>>,
    codex_homes: Option<Vec<String>>,
    base_url: Option<String>,
    timeout_ms: Option<u64>,
    max_output_tokens: Option<u64>,
    max_response_bytes: Option<usize>,
    max_question_bytes: Option<usize>,
    max_request_bytes: Option<usize>,
    max_files: Option<usize>,
    max_file_bytes: Option<usize>,
    max_total_bytes: Option<usize>,
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
    if let Some(base_url) = stored.base_url.as_deref() {
        validate_base_url(base_url)?;
    }
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
    Ok(Config {
        model,
        fallback_models,
        codex_homes,
        limits,
        config_file,
    })
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

fn validate_base_url(value: &str) -> Result<()> {
    let url =
        Url::parse(value).map_err(|_| anyhow::anyhow!("baseUrl must be exactly {BASE_URL}"))?;
    let normalized_path = url.path().trim_end_matches('/');
    if url.scheme() != "https"
        || url.host_str() != Some("openrouter.ai")
        || normalized_path != "/api/v1"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("baseUrl must be exactly {BASE_URL}");
    }
    Ok(())
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

    use super::{bounded, expand_home, validate_base_url};

    #[test]
    fn only_accepts_fixed_openrouter_origin() {
        assert!(validate_base_url("https://openrouter.ai/api/v1/").is_ok());
        assert!(validate_base_url("https://attacker.example/api/v1").is_err());
        assert!(validate_base_url("https://openrouter.ai/api/v1?x=1").is_err());
    }

    #[test]
    fn numeric_limits_fail_closed() {
        assert!(bounded(Some(0usize), 1, 200, "maxFiles").is_err());
        assert!(bounded(Some(201usize), 1, 200, "maxFiles").is_err());
        assert_eq!(bounded(Some(30usize), 1, 200, "maxFiles").unwrap(), 30);
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
