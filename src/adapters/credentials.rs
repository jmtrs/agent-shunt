use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};

use crate::application::ports::{CredentialResolver, ResolvedCredential};

/// Environment variable names checked for the worker API key, in order.
const ENV_KEY_NAMES: &[&str] = &["AGENT_SHUNT_API_KEY", "OPENROUTER_API_KEY"];

pub struct EnvironmentCredentials {
    config_key: Option<String>,
    config_env_name: Option<String>,
    local_provider: bool,
}

impl EnvironmentCredentials {
    pub fn new(
        config_key: Option<String>,
        config_env_name: Option<String>,
        local_provider: bool,
    ) -> Self {
        Self {
            config_key,
            config_env_name,
            local_provider,
        }
    }

    fn env_names(&self) -> Vec<&str> {
        let mut names = ENV_KEY_NAMES.to_vec();
        if let Some(custom) = self.config_env_name.as_deref() {
            names.push(custom);
        }
        names
    }

    fn env_file_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(path) = env::var("AGENT_SHUNT_ENV_FILE")
            && !path.trim().is_empty()
        {
            candidates.push(PathBuf::from(path));
        }
        if let Some(home) = dirs::home_dir() {
            candidates.extend([
                home.join(".config/agent-shunt/.env"),
                home.join(".config/claude-openrouter/.env"),
                home.join(".config/or-info/.env"),
            ]);
        }
        candidates
    }
}

impl CredentialResolver for EnvironmentCredentials {
    fn resolve(&self) -> Result<ResolvedCredential> {
        self.resolve_with(
            &|name| env::var(name).ok().filter(|value| !value.trim().is_empty()),
            &|path| fs::read_to_string(path).ok(),
        )
    }
}

impl EnvironmentCredentials {
    /// Core resolution with the environment and file lookups injected, so the
    /// precedence contract is testable hermetically. Order: configuration key,
    /// `AGENT_SHUNT_API_KEY`, `OPENROUTER_API_KEY`, the configured
    /// `apiKeyEnv` variable, env files, then keyless mode for local
    /// providers. A real key always beats keyless mode: local endpoints that
    /// do accept keys (LM Studio, gated vLLM) must still receive one when the
    /// user explicitly provides it.
    pub(crate) fn resolve_with(
        &self,
        env_lookup: &dyn Fn(&str) -> Option<String>,
        file_read: &dyn Fn(&Path) -> Option<String>,
    ) -> Result<ResolvedCredential> {
        if let Some(key) = self.config_key.as_ref().filter(|key| !key.is_empty()) {
            return Ok(ResolvedCredential {
                api_key: key.clone(),
                source: "configuration".to_owned(),
            });
        }
        let names = self.env_names();
        for name in &names {
            if let Some(value) = env_lookup(name) {
                return Ok(ResolvedCredential {
                    api_key: value.trim().to_owned(),
                    source: "environment".to_owned(),
                });
            }
        }
        for path in Self::env_file_candidates() {
            let Some(content) = file_read(&path) else {
                continue;
            };
            let parsed = parse_env(&content);
            for name in &names {
                if let Some(value) = parsed.get(*name)
                    && !value.trim().is_empty()
                {
                    return Ok(ResolvedCredential {
                        api_key: value.trim().to_owned(),
                        source: path.display().to_string(),
                    });
                }
            }
        }
        if self.local_provider {
            return Ok(ResolvedCredential {
                api_key: String::new(),
                source: "not-required (local)".to_owned(),
            });
        }
        bail!(
            "no worker API key found: set apiKey in the configuration, export \
             AGENT_SHUNT_API_KEY, or point apiKeyEnv at your provider's variable"
        );
    }
}

fn parse_env(content: &str) -> HashMap<String, String> {
    content
        .lines()
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let normalized = line.strip_prefix("export ").unwrap_or(line).trim();
            let (key, raw_value) = normalized.split_once('=')?;
            let value = raw_value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            Some((key.trim().to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{EnvironmentCredentials, parse_env};

    fn none_env(_: &str) -> Option<String> {
        None
    }

    fn none_file(_: &Path) -> Option<String> {
        None
    }

    #[test]
    fn parses_export_and_quotes() {
        let parsed = parse_env("# no\nexport OPENROUTER_API_KEY=\"secret\"\n");
        assert_eq!(parsed["OPENROUTER_API_KEY"], "secret");
    }

    #[test]
    fn configured_key_wins_over_environment() {
        let resolved = EnvironmentCredentials::new(Some("config-secret".to_owned()), None, false)
            .resolve_with(
                &|name| (name == "AGENT_SHUNT_API_KEY").then(|| "env-secret".to_owned()),
                &none_file,
            )
            .unwrap();
        assert_eq!(resolved.api_key, "config-secret");
        assert_eq!(resolved.source, "configuration");
    }

    #[test]
    fn env_names_resolve_in_order() {
        let resolved = EnvironmentCredentials::new(None, None, false)
            .resolve_with(
                &|name| {
                    [
                        ("AGENT_SHUNT_API_KEY", "primary"),
                        ("OPENROUTER_API_KEY", "legacy"),
                    ]
                    .into_iter()
                    .find(|(candidate, _)| *candidate == name)
                    .map(|(_, value)| value.to_owned())
                },
                &none_file,
            )
            .unwrap();
        assert_eq!(resolved.api_key, "primary");
        assert_eq!(resolved.source, "environment");
    }

    #[test]
    fn custom_env_name_resolves_after_builtins() {
        let resolved = EnvironmentCredentials::new(None, Some("GROQ_API_KEY".to_owned()), false)
            .resolve_with(
                &|name| (name == "GROQ_API_KEY").then(|| "groq-secret".to_owned()),
                &none_file,
            )
            .unwrap();
        assert_eq!(resolved.api_key, "groq-secret");
        assert_eq!(resolved.source, "environment");
    }

    #[test]
    fn env_file_is_read_after_environment() {
        let resolved = EnvironmentCredentials::new(None, None, false)
            .resolve_with(&none_env, &|path: &Path| {
                (path.ends_with(".config/agent-shunt/.env"))
                    .then(|| "export AGENT_SHUNT_API_KEY=\"file-secret\"".to_owned())
            })
            .unwrap();
        assert_eq!(resolved.api_key, "file-secret");
        assert!(resolved.source.ends_with("agent-shunt/.env"));
    }

    #[test]
    fn remote_provider_without_key_fails_closed() {
        let error = EnvironmentCredentials::new(None, None, false)
            .resolve_with(&none_env, &none_file)
            .unwrap_err();
        assert!(error.to_string().contains("no worker API key found"));
    }

    #[test]
    fn local_providers_run_keyless_only_without_real_key() {
        let keyless = EnvironmentCredentials::new(None, None, true)
            .resolve_with(&none_env, &none_file)
            .unwrap();
        assert_eq!(keyless.api_key, "");
        assert_eq!(keyless.source, "not-required (local)");

        // A real key still wins for local endpoints: the user provided it on
        // purpose (gated LM Studio/vLLM servers), so it must be delivered.
        let keyed = EnvironmentCredentials::new(None, None, true)
            .resolve_with(
                &|name| (name == "AGENT_SHUNT_API_KEY").then(|| "explicit".to_owned()),
                &none_file,
            )
            .unwrap();
        assert_eq!(keyed.api_key, "explicit");
        assert_eq!(keyed.source, "environment");
    }
}
