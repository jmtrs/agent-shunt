//! Claude Code host integration: another thin `HostConfig` on the shared
//! transactional installer core. Owns `~/.claude/settings.json` — merged,
//! never replaced, so `model`, `permissions`, and unrelated hooks survive —
//! with a `Read|Bash` matcher and a longer timeout than Codex (Claude Code
//! runs hooks per tool call with its own scheduling). Skills live at
//! `~/.claude/skills` with the same format Codex uses. Claude Code has no
//! post-change trust flow, so reports never flag `requires_hook_trust`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use shell_words::quote;

use crate::{
    adapters::host_install::{HookSpec, HostConfig, install_homes},
    application::ports::HostInstaller,
    domain::InstallReport,
};

const SKILL: &str = include_str!("../../integrations/skills/agent-shunt/SKILL.md");
const HOOK_SUFFIX: &str = " hook claude-pre-tool-use";

pub struct ClaudeInstaller {
    executable: PathBuf,
}

impl ClaudeInstaller {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn config(&self) -> HostConfig {
        HostConfig {
            label: "Claude Code",
            hook: Some(HookSpec {
                hook_file: "settings.json",
                hook_command: format!(
                    "{} hook claude-pre-tool-use",
                    quote(&self.executable.to_string_lossy())
                ),
                owned_suffix: HOOK_SUFFIX,
                matcher: "Read|Bash",
                timeout_secs: 10,
                handler_extra: None,
                requires_hook_trust: false,
            }),
            skill_files: vec![(
                Path::new("skills/agent-shunt/SKILL.md").to_path_buf(),
                SKILL.as_bytes(),
                0o644,
            )],
        }
    }
}

impl HostInstaller for ClaudeInstaller {
    fn install(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<InstallReport>> {
        install_homes(homes, hook, &self.config())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::ClaudeInstaller;
    use crate::application::ports::HostInstaller;

    #[test]
    fn merges_into_settings_preserving_model_and_other_hooks() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("settings.json"),
            r#"{"model":"opus","permissions":{"allow":["Bash(ls)"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"notify-done"}]}]}}"#,
        )
        .unwrap();
        let installer = ClaudeInstaller::new("/opt/homebrew/bin/agent-shunt".into());
        let first = installer
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(first[0].skill_changed && first[0].hook_changed);
        assert!(!first[0].requires_hook_trust);
        let value: Value =
            serde_json::from_slice(&fs::read(root.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(value["model"], "opus");
        assert_eq!(value["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"][0]["command"],
            "notify-done"
        );
        let group = &value["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], "Read|Bash");
        assert_eq!(
            group["hooks"][0]["command"],
            "/opt/homebrew/bin/agent-shunt hook claude-pre-tool-use"
        );
        assert_eq!(group["hooks"][0]["timeout"], 10);
        assert!(group["hooks"][0].get("statusMessage").is_none());

        let second = installer
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(!second[0].skill_changed && !second[0].hook_changed);
    }

    #[test]
    fn backs_up_settings_and_rolls_back_across_homes() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        fs::write(second.path().join("settings.json"), "not json").unwrap();
        let installer = ClaudeInstaller::new("/opt/homebrew/bin/agent-shunt".into());
        assert!(
            installer
                .install(
                    &[first.path().to_path_buf(), second.path().to_path_buf()],
                    true
                )
                .is_err()
        );
        assert!(!first.path().join("skills/agent-shunt/SKILL.md").exists());
    }

    #[test]
    fn missing_home_error_names_claude() {
        let installer = ClaudeInstaller::new("/opt/homebrew/bin/agent-shunt".into());
        let error = installer
            .install(
                &[std::path::PathBuf::from("/nonexistent-claude-home")],
                false,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Claude Code home does not exist")
        );
    }
}
