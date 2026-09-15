use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct HookEvent {
    cwd: PathBuf,
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
}

pub fn run_pre_tool_use(threshold_bytes: u64) -> Result<()> {
    let mut encoded = String::new();
    io::stdin()
        .take(1_000_001)
        .read_to_string(&mut encoded)
        .context("cannot read hook input")?;
    if encoded.len() > 1_000_000 {
        return Ok(());
    }
    let Ok(event) = serde_json::from_str::<HookEvent>(&encoded) else {
        return Ok(());
    };
    let Some(candidate) = whole_file_candidate(&event) else {
        return Ok(());
    };
    let Some(size) = safe_size(&event.cwd, &candidate) else {
        return Ok(());
    };
    if size < threshold_bytes {
        return Ok(());
    }
    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "Whole-file read of {} bytes redirected. Use `agent-shunt retrieve --question \"<current task>\" --dir \"{}\"`; use a normal ranged read if exact lines are needed.",
                size,
                event.cwd.display()
            )
        }
    });
    println!("{output}");
    Ok(())
}

fn whole_file_candidate(event: &HookEvent) -> Option<PathBuf> {
    let tool = event.tool_name.to_ascii_lowercase();
    if matches!(
        tool.as_str(),
        "bash" | "shell" | "unified_exec" | "exec_command"
    ) {
        let command = event
            .tool_input
            .get("command")
            .or_else(|| event.tool_input.get("cmd"))?
            .as_str()?;
        let words = shell_words::split(command).ok()?;
        return match words.as_slice() {
            [program, path] if program == "cat" => Some(PathBuf::from(path)),
            [program, separator, path] if program == "cat" && separator == "--" => {
                Some(PathBuf::from(path))
            }
            _ => None,
        };
    }
    // Claude Code names its tool `Read`; Codex and filesystem MCP servers
    // use the variants below. All share the same input shape (`file_path` or
    // `path`, ranges via offset/limit/... keys handled above).
    if !matches!(
        tool.as_str(),
        "read" | "read_file" | "read_text_file" | "mcp__filesystem__read_file"
    ) {
        return None;
    }
    let object = event.tool_input.as_object()?;
    if [
        "offset",
        "limit",
        "start",
        "end",
        "start_line",
        "end_line",
        "line_start",
        "line_end",
        "range",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
    {
        return None;
    }
    object
        .get("path")
        .or_else(|| object.get("file_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn safe_size(cwd: &Path, candidate: &Path) -> Option<u64> {
    let root = fs::canonicalize(cwd).ok()?;
    let relative = if candidate.is_absolute() {
        candidate.strip_prefix(&root).ok()?.to_path_buf()
    } else {
        candidate.to_path_buf()
    };
    let mut joined = root.clone();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        joined.push(component);
        if fs::symlink_metadata(&joined).ok()?.file_type().is_symlink() {
            return None;
        }
    }
    let metadata = fs::symlink_metadata(&joined).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let canonical = fs::canonicalize(joined).ok()?;
    canonical.starts_with(root).then_some(metadata.len())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{HookEvent, whole_file_candidate};

    #[test]
    fn detects_only_simple_whole_file_reads() {
        let event = HookEvent {
            cwd: ".".into(),
            tool_name: "Bash".to_owned(),
            tool_input: json!({"command":"cat -- src/main.rs"}),
        };
        assert_eq!(
            whole_file_candidate(&event).unwrap(),
            std::path::PathBuf::from("src/main.rs")
        );

        let ranged = HookEvent {
            cwd: ".".into(),
            tool_name: "read_file".to_owned(),
            tool_input: json!({"path":"src/main.rs", "start_line":1, "end_line":20}),
        };
        assert!(whole_file_candidate(&ranged).is_none());

        let compound = HookEvent {
            cwd: ".".into(),
            tool_name: "Bash".to_owned(),
            tool_input: json!({"command":"cat src/main.rs | head"}),
        };
        assert!(whole_file_candidate(&compound).is_none());

        let unknown = HookEvent {
            cwd: ".".into(),
            tool_name: "read_unknown_future_tool".to_owned(),
            tool_input: json!({"path":"src/main.rs"}),
        };
        assert!(whole_file_candidate(&unknown).is_none());
    }

    #[test]
    fn detects_claude_code_tool_shapes() {
        let claude_read = HookEvent {
            cwd: ".".into(),
            tool_name: "Read".to_owned(),
            tool_input: json!({"file_path":"src/main.rs"}),
        };
        assert_eq!(
            whole_file_candidate(&claude_read).unwrap(),
            std::path::PathBuf::from("src/main.rs")
        );

        let ranged = HookEvent {
            cwd: ".".into(),
            tool_name: "Read".to_owned(),
            tool_input: json!({"file_path":"src/main.rs", "offset":100, "limit":50}),
        };
        assert!(whole_file_candidate(&ranged).is_none());

        let claude_bash = HookEvent {
            cwd: ".".into(),
            tool_name: "Bash".to_owned(),
            tool_input: json!({"command":"cat src/main.rs"}),
        };
        assert_eq!(
            whole_file_candidate(&claude_bash).unwrap(),
            std::path::PathBuf::from("src/main.rs")
        );
    }
}
