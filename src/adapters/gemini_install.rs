//! Gemini CLI host integration: a hookless `HostConfig` on the shared
//! transactional installer core. Owns `~/.gemini/commands/agent-shunt.toml`,
//! the TOML v1 custom-command format (`description` + `prompt`). Gemini CLI
//! has no PreToolUse hook surface, so `install --hook` installs the command
//! only; run `/commands reload` in the CLI after installing.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    adapters::host_install::{HostConfig, install_homes},
    application::ports::HostInstaller,
    domain::InstallReport,
};

const COMMAND: &str = include_str!("../../integrations/gemini/commands/agent-shunt.toml");

pub struct GeminiInstaller;

impl GeminiInstaller {
    fn config() -> HostConfig {
        HostConfig {
            label: "Gemini CLI",
            hook: None,
            skill_files: vec![(
                Path::new("commands/agent-shunt.toml").to_path_buf(),
                COMMAND.as_bytes(),
                0o644,
            )],
        }
    }
}

impl HostInstaller for GeminiInstaller {
    fn install(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<InstallReport>> {
        install_homes(homes, hook, &Self::config())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::GeminiInstaller;
    use crate::application::ports::HostInstaller;

    #[test]
    fn installs_toml_command_without_any_hook_file() {
        let root = tempdir().unwrap();
        let first = GeminiInstaller
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(first[0].skill_changed);
        assert!(!first[0].hook_changed);
        let command = root.path().join("commands/agent-shunt.toml");
        assert!(command.is_file());
        let body = std::fs::read_to_string(command).unwrap();
        assert!(body.starts_with("description = \""));
        assert!(body.contains("prompt = \"\"\"\n"));
        assert!(!root.path().join("hooks.json").exists());
        // Idempotent: same content reports no change.
        let second = GeminiInstaller
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(!second[0].skill_changed);
    }

    #[test]
    fn missing_home_error_names_gemini() {
        let error = GeminiInstaller
            .install(
                &[std::path::PathBuf::from("/nonexistent-gemini-home")],
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("Gemini CLI home does not exist"));
    }
}
