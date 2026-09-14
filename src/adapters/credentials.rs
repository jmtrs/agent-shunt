use std::{collections::HashMap, env, fs, path::PathBuf};

use anyhow::{Result, bail};

use crate::application::ports::{CredentialResolver, ResolvedCredential};

pub struct EnvironmentCredentials;

impl CredentialResolver for EnvironmentCredentials {
    fn resolve(&self) -> Result<ResolvedCredential> {
        if let Ok(value) = env::var("OPENROUTER_API_KEY")
            && !value.trim().is_empty()
        {
            return Ok(ResolvedCredential {
                api_key: value.trim().to_owned(),
                source: "environment".to_owned(),
            });
        }
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
        for path in candidates {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let parsed = parse_env(&content);
            if let Some(value) = parsed.get("OPENROUTER_API_KEY")
                && !value.trim().is_empty()
            {
                return Ok(ResolvedCredential {
                    api_key: value.trim().to_owned(),
                    source: path.display().to_string(),
                });
            }
        }
        bail!("OPENROUTER_API_KEY not found in the environment or supported user config files")
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
    use super::parse_env;

    #[test]
    fn parses_export_and_quotes() {
        let parsed = parse_env("# no\nexport OPENROUTER_API_KEY=\"secret\"\n");
        assert_eq!(parsed["OPENROUTER_API_KEY"], "secret");
    }
}
