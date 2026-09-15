//! opencode host integration: a hookless `HostConfig` on the shared
//! transactional installer core. opencode loads Claude-format skills from
//! `~/.config/opencode/skills/<name>/SKILL.md`, so the skill body matches the
//! Claude/Codex template. opencode has no hook surface we wire into, so
//! `install --hook` installs the skill only.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    adapters::host_install::{HostConfig, install_homes},
    application::ports::HostInstaller,
    domain::InstallReport,
};

const SKILL: &str = include_str!("../../integrations/opencode/skills/agent-shunt/SKILL.md");

pub struct OpencodeInstaller;

impl OpencodeInstaller {
    fn config() -> HostConfig {
        HostConfig {
            label: "opencode",
            hook: None,
            skill_files: vec![(
                Path::new("skills/agent-shunt/SKILL.md").to_path_buf(),
                SKILL.as_bytes(),
                0o644,
            )],
        }
    }
}

impl HostInstaller for OpencodeInstaller {
    fn install(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<InstallReport>> {
        install_homes(homes, hook, &Self::config())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::OpencodeInstaller;
    use crate::application::ports::HostInstaller;

    #[test]
    fn installs_claude_format_skill_without_any_hook_file() {
        let root = tempdir().unwrap();
        let first = OpencodeInstaller
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(first[0].skill_changed);
        assert!(!first[0].hook_changed);
        let skill = root.path().join("skills/agent-shunt/SKILL.md");
        assert!(skill.is_file());
        let body = std::fs::read_to_string(skill).unwrap();
        assert!(body.starts_with("---\nname: agent-shunt\n"));
        assert!(!root.path().join("hooks.json").exists());
        let second = OpencodeInstaller
            .install(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(!second[0].skill_changed);
    }

    #[test]
    fn missing_home_error_names_opencode() {
        let error = OpencodeInstaller
            .install(
                &[std::path::PathBuf::from("/nonexistent-opencode-home")],
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("opencode home does not exist"));
    }
}
