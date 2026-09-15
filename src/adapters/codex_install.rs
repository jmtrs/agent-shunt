//! Codex host integration: a thin `HostConfig` on the shared transactional
//! installer core. Owns `~/.codex/hooks.json` (merge-safe, matcher `"*"`,
//! with a `statusMessage` for Codex's UI) and the agent-shunt skill plus its
//! OpenAI agent definition. Codex asks the user to re-trust hook
//! definitions after they change, so reports flag `requires_hook_trust`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use shell_words::quote;

use crate::{
    adapters::host_install::{HookSpec, HostConfig, install_homes},
    application::ports::HostInstaller,
    domain::InstallReport,
};

const SKILL: &str = include_str!("../../integrations/skills/agent-shunt/SKILL.md");
const OPENAI_YAML: &str =
    include_str!("../../integrations/codex/skills/agent-shunt/agents/openai.yaml");
const STATUS_MESSAGE: &str = "Checking large whole-file read";
const HOOK_SUFFIX: &str = " hook codex-pre-tool-use";

pub struct CodexInstaller {
    executable: PathBuf,
}

impl CodexInstaller {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn config(&self) -> HostConfig {
        HostConfig {
            label: "Codex",
            hook: Some(HookSpec {
                hook_file: "hooks.json",
                hook_command: format!(
                    "{} hook codex-pre-tool-use",
                    quote(&self.executable.to_string_lossy())
                ),
                owned_suffix: HOOK_SUFFIX,
                matcher: "*",
                timeout_secs: 2,
                handler_extra: Some(("statusMessage", STATUS_MESSAGE)),
                requires_hook_trust: true,
            }),
            skill_files: vec![
                (
                    Path::new("skills/agent-shunt/SKILL.md").to_path_buf(),
                    SKILL.as_bytes(),
                    0o644,
                ),
                (
                    Path::new("skills/agent-shunt/agents/openai.yaml").to_path_buf(),
                    OPENAI_YAML.as_bytes(),
                    0o644,
                ),
            ],
        }
    }
}

impl HostInstaller for CodexInstaller {
    fn install(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<InstallReport>> {
        install_homes(homes, hook, &self.config())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::CodexInstaller;
    use crate::application::ports::HostInstaller;

    #[test]
    fn installs_skill_files_and_codex_shaped_hook() {
        let root = tempdir().unwrap();
        let installer = CodexInstaller::new("/opt/homebrew/bin/agent-shunt".into());
        let reports = installer
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(reports[0].skill_changed && reports[0].hook_changed);
        assert!(reports[0].requires_hook_trust);
        assert!(root.path().join("skills/agent-shunt/SKILL.md").is_file());
        assert!(
            root.path()
                .join("skills/agent-shunt/agents/openai.yaml")
                .is_file()
        );
        let value: Value =
            serde_json::from_slice(&fs::read(root.path().join("hooks.json")).unwrap()).unwrap();
        let group = &value["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], "*");
        assert_eq!(
            group["hooks"][0]["command"],
            "/opt/homebrew/bin/agent-shunt hook codex-pre-tool-use"
        );
        assert_eq!(group["hooks"][0]["timeout"], 2);
        assert_eq!(
            group["hooks"][0]["statusMessage"],
            "Checking large whole-file read"
        );
    }

    #[test]
    fn missing_home_error_names_codex() {
        let installer = CodexInstaller::new("/opt/homebrew/bin/agent-shunt".into());
        let error = installer
            .install(
                &[std::path::PathBuf::from("/nonexistent-codex-home")],
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("Codex home does not exist"));
    }
}
