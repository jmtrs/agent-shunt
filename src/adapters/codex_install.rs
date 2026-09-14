use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{application::ports::HostInstaller, domain::CodexInstallReport};

const SKILL: &str = include_str!("../../integrations/codex/skills/agent-shunt/SKILL.md");
const OPENAI_YAML: &str =
    include_str!("../../integrations/codex/skills/agent-shunt/agents/openai.yaml");
const STATUS_MESSAGE: &str = "Checking large whole-file read";

pub struct CodexInstaller {
    executable: PathBuf,
}

impl CodexInstaller {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn hook_command(&self) -> String {
        format!(
            "{} hook codex-pre-tool-use",
            shell_words::quote(&self.executable.to_string_lossy())
        )
    }
}

struct HookPlan {
    encoded: Vec<u8>,
    changed: bool,
}

struct Snapshot {
    path: PathBuf,
    content: Option<Vec<u8>>,
    mode: u32,
}

impl HostInstaller for CodexInstaller {
    fn install_codex(&self, homes: &[PathBuf], hook: bool) -> Result<Vec<CodexInstallReport>> {
        if homes.is_empty() {
            bail!("at least one Codex home is required");
        }
        let command = self.hook_command();
        let plans = homes
            .iter()
            .map(|home| preflight_home(home, hook, &command))
            .collect::<Result<Vec<_>>>()?;
        let snapshots = homes
            .iter()
            .flat_map(|home| target_paths(home, hook))
            .map(|(path, mode)| Snapshot {
                content: fs::read(&path).ok(),
                path,
                mode,
            })
            .collect::<Vec<_>>();
        let mut reports = Vec::new();
        for (home, hook_plan) in homes.iter().zip(plans) {
            match install_home(home, hook_plan) {
                Ok(report) => reports.push(report),
                Err(error) => {
                    rollback(&snapshots);
                    return Err(error.context("Codex installation rolled back"));
                }
            }
        }
        Ok(reports)
    }
}

fn target_paths(home: &Path, hook: bool) -> Vec<(PathBuf, u32)> {
    let mut paths = vec![
        (home.join("skills/agent-shunt/SKILL.md"), 0o644),
        (home.join("skills/agent-shunt/agents/openai.yaml"), 0o644),
    ];
    if hook {
        paths.push((home.join("hooks.json"), 0o600));
    }
    paths
}

fn preflight_home(home: &Path, hook: bool, command: &str) -> Result<Option<HookPlan>> {
    if !home.is_dir() {
        bail!("Codex home does not exist: {}", home.display());
    }
    hook.then(|| plan_hook(&home.join("hooks.json"), command))
        .transpose()
}

fn install_home(home: &Path, hook_plan: Option<HookPlan>) -> Result<CodexInstallReport> {
    let skill_dir = home.join("skills/agent-shunt");
    let agents_dir = skill_dir.join("agents");
    fs::create_dir_all(&agents_dir)?;
    let skill_changed = install_owned_file(&skill_dir.join("SKILL.md"), SKILL.as_bytes(), 0o644)?
        | install_owned_file(
            &agents_dir.join("openai.yaml"),
            OPENAI_YAML.as_bytes(),
            0o644,
        )?;
    let hook_path = home.join("hooks.json");
    let (hook_changed, hook_backup) = match hook_plan {
        Some(plan) if plan.changed => {
            let backup = if hook_path.exists() {
                let backup = hook_path.with_extension("json.agent-shunt.bak");
                if !backup.exists() {
                    fs::copy(&hook_path, &backup)?;
                }
                Some(backup)
            } else {
                None
            };
            atomic_write(&hook_path, &plan.encoded, 0o600)?;
            (true, backup)
        }
        _ => (false, None),
    };
    Ok(CodexInstallReport {
        home: home.to_path_buf(),
        skill_changed,
        hook_changed,
        hook_backup,
        requires_hook_trust: hook_changed,
    })
}

fn plan_hook(path: &Path, command: &str) -> Result<HookPlan> {
    let existing = if path.exists() {
        fs::read(path).with_context(|| format!("cannot read {}", path.display()))?
    } else {
        b"{\"hooks\":{}}".to_vec()
    };
    let mut root: Value = serde_json::from_slice(&existing)
        .with_context(|| format!("invalid hooks JSON: {}", path.display()))?;
    let object = root
        .as_object_mut()
        .context("hooks.json root must be an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("hooks must be an object")?;
    let groups = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("PreToolUse must be an array")?;
    let mut found = false;
    for group in groups.iter_mut() {
        let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        for handler in handlers {
            let owned = handler.get("statusMessage").and_then(Value::as_str)
                == Some(STATUS_MESSAGE)
                || handler
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.ends_with(" hook codex-pre-tool-use"));
            if owned {
                *handler = json!({
                    "type": "command",
                    "command": command,
                    "timeout": 2,
                    "statusMessage": STATUS_MESSAGE
                });
                found = true;
            }
        }
    }
    if !found {
        groups.push(json!({
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": 2,
                "statusMessage": STATUS_MESSAGE
            }]
        }));
    }
    let encoded = serde_json::to_vec_pretty(&root)?;
    Ok(HookPlan {
        changed: encoded != existing,
        encoded,
    })
}

fn install_owned_file(path: &Path, content: &[u8], mode: u32) -> Result<bool> {
    if fs::read(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if path.exists() {
        let backup = path.with_extension(format!(
            "{}.agent-shunt.bak",
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("file")
        ));
        if !backup.exists() {
            fs::copy(path, backup)?;
        }
    }
    atomic_write(path, content, mode)?;
    Ok(true)
}

fn rollback(snapshots: &[Snapshot]) {
    for snapshot in snapshots.iter().rev() {
        match &snapshot.content {
            Some(content) => {
                let _ = atomic_write(&snapshot.path, content, snapshot.mode);
            }
            None => {
                if snapshot.path.is_file() {
                    let _ = fs::remove_file(&snapshot.path);
                }
            }
        }
    }
}

fn atomic_write(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".agent-shunt-{}-{}.tmp",
        std::process::id(),
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use crate::application::ports::HostInstaller;

    use super::CodexInstaller;

    #[test]
    fn merges_without_replacing_existing_hook_and_is_idempotent() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("hooks.json"),
            r#"{"hooks":{"PermissionRequest":[{"matcher":"*","hooks":[{"type":"command","command":"existing"}]}]}}"#,
        )
        .unwrap();
        let installer = CodexInstaller::new("/custom/bin/agent-shunt".into());
        let first = installer
            .install_codex(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(first[0].hook_changed);
        let value: Value =
            serde_json::from_slice(&fs::read(root.path().join("hooks.json")).unwrap()).unwrap();
        assert_eq!(
            value["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "existing"
        );
        assert_eq!(
            value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/custom/bin/agent-shunt hook codex-pre-tool-use"
        );
        let second = installer
            .install_codex(&[root.path().to_path_buf()], true)
            .unwrap();
        assert!(!second[0].hook_changed);
        assert!(!second[0].skill_changed);
    }

    #[test]
    fn malformed_second_home_does_not_mutate_first_home() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        fs::write(second.path().join("hooks.json"), "not json").unwrap();
        let installer = CodexInstaller::new("/bin/agent-shunt".into());
        assert!(
            installer
                .install_codex(
                    &[first.path().to_path_buf(), second.path().to_path_buf()],
                    true
                )
                .is_err()
        );
        assert!(!first.path().join("skills/agent-shunt/SKILL.md").exists());
    }
}
