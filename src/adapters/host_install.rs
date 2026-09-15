//! Transactional installer core shared by every host integration
//! (Codex, Claude Code, future hosts). Each host is a thin `HostConfig`
//! describing where its hook file lives, what our handler looks like, and
//! which skill files to drop; this module owns the safety: preflight every
//! home before writing anything, snapshot every target, roll back all homes
//! on any failure, preserve unrelated configuration, and stay idempotent.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::domain::InstallReport;

/// Per-host installer description. `hook_file` is merged (never replaced),
/// with our handlers recognized by `owned_suffix` at the end of their
/// command. `handler_extra` adds host-specific handler keys (Codex uses
/// `statusMessage`). `requires_hook_trust` marks hosts that need an explicit
/// trust step in their UI after a hook change (Codex `/hooks`).
pub(crate) struct HostConfig {
    pub label: &'static str,
    pub hook_file: &'static str,
    pub hook_command: String,
    pub owned_suffix: &'static str,
    pub matcher: &'static str,
    pub timeout_secs: u64,
    pub handler_extra: Option<(&'static str, &'static str)>,
    pub skill_files: Vec<(PathBuf, &'static [u8], u32)>,
    pub requires_hook_trust: bool,
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

pub(crate) fn install_homes(
    homes: &[PathBuf],
    hook: bool,
    config: &HostConfig,
) -> Result<Vec<InstallReport>> {
    if homes.is_empty() {
        bail!("at least one {} home is required", config.label);
    }
    let plans = homes
        .iter()
        .map(|home| preflight_home(home, hook, config))
        .collect::<Result<Vec<_>>>()?;
    let snapshots = homes
        .iter()
        .flat_map(|home| target_paths(home, hook, config))
        .map(|(path, mode)| Snapshot {
            content: fs::read(&path).ok(),
            path,
            mode,
        })
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    for (home, hook_plan) in homes.iter().zip(plans) {
        match install_home(home, hook_plan, config) {
            Ok(report) => reports.push(report),
            Err(error) => {
                rollback(&snapshots);
                return Err(error.context(format!("{} installation rolled back", config.label)));
            }
        }
    }
    Ok(reports)
}

fn target_paths(home: &Path, hook: bool, config: &HostConfig) -> Vec<(PathBuf, u32)> {
    let mut paths = config
        .skill_files
        .iter()
        .map(|(relative, _, mode)| (home.join(relative), *mode))
        .collect::<Vec<_>>();
    if hook {
        paths.push((home.join(config.hook_file), 0o600));
    }
    paths
}

fn preflight_home(home: &Path, hook: bool, config: &HostConfig) -> Result<Option<HookPlan>> {
    if !home.is_dir() {
        bail!("{} home does not exist: {}", config.label, home.display());
    }
    hook.then(|| plan_hook(&home.join(config.hook_file), config))
        .transpose()
}

fn install_home(
    home: &Path,
    hook_plan: Option<HookPlan>,
    config: &HostConfig,
) -> Result<InstallReport> {
    let mut skill_changed = false;
    for (relative, content, mode) in &config.skill_files {
        let path = home.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        skill_changed |= install_owned_file(&path, content, *mode)?;
    }
    let hook_path = home.join(config.hook_file);
    let (hook_changed, hook_backup) = match hook_plan {
        Some(plan) if plan.changed => {
            let backup = backup_existing(&hook_path)?;
            atomic_write(&hook_path, &plan.encoded, 0o600)?;
            (true, backup)
        }
        _ => (false, None),
    };
    Ok(InstallReport {
        home: home.to_path_buf(),
        skill_changed,
        hook_changed,
        hook_backup,
        requires_hook_trust: hook_changed && config.requires_hook_trust,
    })
}

/// Merges our handler into the host's hook configuration, preserving every
/// unrelated key, group, and handler. Our existing handlers are replaced in
/// place; a fresh group is appended only when none of ours is present.
fn plan_hook(path: &Path, config: &HostConfig) -> Result<HookPlan> {
    let existing = if path.exists() {
        fs::read(path).with_context(|| format!("cannot read {}", path.display()))?
    } else {
        b"{\"hooks\":{}}".to_vec()
    };
    let mut root: Value = serde_json::from_slice(&existing)
        .with_context(|| format!("invalid hooks JSON: {}", path.display()))?;
    let object = root
        .as_object_mut()
        .with_context(|| format!("{} root must be an object", path.display()))?;
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
    let handler = fresh_handler(config);
    let mut found = false;
    for group in groups.iter_mut() {
        let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        for target in handlers {
            let owned = target
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|value| value.ends_with(config.owned_suffix));
            if owned {
                *target = handler.clone();
                found = true;
            }
        }
    }
    if !found {
        groups.push(json!({
            "matcher": config.matcher,
            "hooks": [handler]
        }));
    }
    let encoded = serde_json::to_vec_pretty(&root)?;
    Ok(HookPlan {
        changed: encoded != existing,
        encoded,
    })
}

fn fresh_handler(config: &HostConfig) -> Value {
    let mut handler = json!({
        "type": "command",
        "command": config.hook_command,
        "timeout": config.timeout_secs
    });
    if let Some((key, value)) = config.handler_extra {
        handler[key] = json!(value);
    }
    handler
}

fn backup_existing(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let backup = path.with_extension(format!(
        "{}.agent-shunt.bak",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
    ));
    if !backup.exists() {
        fs::copy(path, &backup)?;
    }
    Ok(Some(backup))
}

fn install_owned_file(path: &Path, content: &[u8], mode: u32) -> Result<bool> {
    if fs::read(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if let Some(backup) = backup_existing(path)? {
        let _ = backup;
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

    use super::{HostConfig, install_homes};

    fn config(matcher: &'static str, extra: Option<(&'static str, &'static str)>) -> HostConfig {
        HostConfig {
            label: "TestHost",
            hook_file: "hooks.json",
            hook_command: "/bin/agent-shunt hook test-pre-tool-use".to_owned(),
            owned_suffix: " hook test-pre-tool-use",
            matcher,
            timeout_secs: 5,
            handler_extra: extra,
            skill_files: vec![(
                std::path::PathBuf::from("skills/agent-shunt/SKILL.md"),
                b"# skill\n".as_slice(),
                0o644,
            )],
            requires_hook_trust: true,
        }
    }

    #[test]
    fn preserves_unrelated_keys_and_is_idempotent() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("hooks.json"),
            r#"{"model":"sonnet","hooks":{"PostToolUse":[{"matcher":"Write","hooks":[{"type":"command","command":"existing"}]}],"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"someone-elses-hook"}]}]}}"#,
        )
        .unwrap();
        let first = install_homes(
            &[root.path().to_path_buf()],
            true,
            &config("Read|Bash", None),
        )
        .unwrap();
        assert!(first[0].hook_changed && first[0].skill_changed);
        let value: Value =
            serde_json::from_slice(&fs::read(root.path().join("hooks.json")).unwrap()).unwrap();
        assert_eq!(value["model"], "sonnet");
        assert_eq!(
            value["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "existing"
        );
        assert_eq!(
            value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "someone-elses-hook"
        );
        assert_eq!(value["hooks"]["PreToolUse"][1]["matcher"], "Read|Bash");
        assert_eq!(
            value["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
            "/bin/agent-shunt hook test-pre-tool-use"
        );
        assert_eq!(value["hooks"]["PreToolUse"][1]["hooks"][0]["timeout"], 5);

        let second = install_homes(
            &[root.path().to_path_buf()],
            true,
            &config("Read|Bash", None),
        )
        .unwrap();
        assert!(!second[0].hook_changed && !second[0].skill_changed);
    }

    #[test]
    fn replaces_owned_handler_in_place_and_keeps_extras() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"/old/bin/agent-shunt hook test-pre-tool-use","statusMessage":"stale"}]}]}}"#,
        )
        .unwrap();
        let config = config(
            "*",
            Some(("statusMessage", "Checking large whole-file read")),
        );
        install_homes(&[root.path().to_path_buf()], true, &config).unwrap();
        let value: Value =
            serde_json::from_slice(&fs::read(root.path().join("hooks.json")).unwrap()).unwrap();
        let groups = value["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["matcher"], "*");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            "/bin/agent-shunt hook test-pre-tool-use"
        );
        assert_eq!(
            groups[0]["hooks"][0]["statusMessage"],
            "Checking large whole-file read"
        );
    }

    #[test]
    fn malformed_second_home_rolls_back_first_home() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        fs::write(second.path().join("hooks.json"), "not json").unwrap();
        let config = config("Read|Bash", None);
        assert!(
            install_homes(
                &[first.path().to_path_buf(), second.path().to_path_buf()],
                true,
                &config
            )
            .is_err()
        );
        assert!(!first.path().join("skills/agent-shunt/SKILL.md").exists());
    }

    #[test]
    fn missing_home_fails_before_any_write() {
        let existing = tempdir().unwrap();
        let config = config("Read|Bash", None);
        let missing = existing.path().join("does-not-exist");
        assert!(install_homes(&[existing.path().to_path_buf(), missing], false, &config).is_err());
        assert!(!existing.path().join("skills/agent-shunt/SKILL.md").exists());
    }
}
