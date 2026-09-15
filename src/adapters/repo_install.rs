//! Repository-level integrations: instruction files that live in the repo a
//! coding agent is working on, not in the agent's home directory. Two kinds:
//! files we own exclusively (Cursor `.mdc`, Cline and Roo rule files —
//! written wholesale, idempotent) and files shared with user content
//! (`AGENTS.md`, `.github/copilot-instructions.md`), where a managed section
//! between `agent-shunt` markers is appended or replaced in place so every
//! line of user configuration survives. Same safety as the home installer:
//! preflight the root, snapshot every target, roll back on any failure.

use std::{fs, path::Path, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::{
    adapters::host_install::{PriorState, atomic_write},
    domain::InstallReport,
};

const BEGIN_MARKER: &str = "<!-- agent-shunt:begin -->";
const END_MARKER: &str = "<!-- agent-shunt:end -->";

const AGENTS_MD: &str = include_str!("../../integrations/repo/agents-md.md");
const COPILOT: &str = include_str!("../../integrations/repo/copilot.md");
const CURSOR: &str = include_str!("../../integrations/repo/cursor.mdc");
const CLINE: &str = include_str!("../../integrations/repo/cline.md");
const ROO: &str = include_str!("../../integrations/repo/roo.md");

/// Per-repo-host description: files we own outright, and shared files that
/// get a marker-delimited managed section.
struct RepoConfig {
    label: &'static str,
    /// (relative path, full content, mode) — replaced wholesale; ours alone.
    own_files: Vec<(PathBuf, &'static str)>,
    /// (relative path, section body without markers) — merged into a file
    /// the user may already own.
    managed_files: Vec<(PathBuf, &'static str)>,
}

/// Installs repo-level instruction files. Unlike home installers there is a
/// single destination root (the repository), so the report is singular.
pub struct RepoInstaller;

impl RepoInstaller {
    pub fn install_agents_md(&self, root: &Path) -> Result<InstallReport> {
        self.install(
            root,
            RepoConfig {
                label: "AGENTS.md",
                own_files: Vec::new(),
                managed_files: vec![(PathBuf::from("AGENTS.md"), AGENTS_MD)],
            },
        )
    }

    pub fn install_copilot(&self, root: &Path) -> Result<InstallReport> {
        self.install(
            root,
            RepoConfig {
                label: "GitHub Copilot",
                own_files: Vec::new(),
                managed_files: vec![(PathBuf::from(".github/copilot-instructions.md"), COPILOT)],
            },
        )
    }

    pub fn install_cursor(&self, root: &Path) -> Result<InstallReport> {
        self.install(
            root,
            RepoConfig {
                label: "Cursor",
                own_files: vec![(PathBuf::from(".cursor/rules/agent-shunt.mdc"), CURSOR)],
                managed_files: Vec::new(),
            },
        )
    }

    pub fn install_cline(&self, root: &Path) -> Result<InstallReport> {
        self.install(
            root,
            RepoConfig {
                label: "Cline",
                own_files: vec![(PathBuf::from(".clinerules/agent-shunt.md"), CLINE)],
                managed_files: Vec::new(),
            },
        )
    }

    pub fn install_roo(&self, root: &Path) -> Result<InstallReport> {
        self.install(
            root,
            RepoConfig {
                label: "Roo Code",
                own_files: vec![(PathBuf::from(".roo/rules/agent-shunt.md"), ROO)],
                managed_files: Vec::new(),
            },
        )
    }

    fn install(&self, root: &Path, config: RepoConfig) -> Result<InstallReport> {
        if !root.is_dir() {
            bail!(
                "{} target directory does not exist: {}",
                config.label,
                root.display()
            );
        }
        let targets = config
            .own_files
            .iter()
            .map(|(relative, _)| root.join(relative))
            .chain(
                config
                    .managed_files
                    .iter()
                    .map(|(relative, _)| root.join(relative)),
            )
            .collect::<Vec<_>>();
        let snapshots = targets
            .iter()
            .map(|path| (path.clone(), PriorState::probe(path)))
            .collect::<Vec<_>>();
        let mut changed = false;
        let result: Result<()> = (|| {
            for (relative, content) in &config.own_files {
                let path = root.join(relative);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                if fs::read(&path).ok().as_deref() != Some(content.as_bytes()) {
                    backup_fresh(&path)?;
                    atomic_write(&path, content.as_bytes(), 0o644)?;
                    changed = true;
                }
            }
            for (relative, section) in &config.managed_files {
                let path = root.join(relative);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                // "Missing" and "present but unreadable" must stay distinct:
                // silently treating an unreadable file as empty would
                // replace the user's content with just our section.
                let existing = match fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("cannot read existing {}", path.display()));
                    }
                };
                let merged = merge_managed(&existing, section)?;
                if merged != existing {
                    backup_fresh(&path)?;
                    atomic_write(&path, merged.as_bytes(), 0o644)?;
                    changed = true;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            rollback(&snapshots);
            return Err(error.context(format!("{} installation rolled back", config.label)));
        }
        Ok(InstallReport {
            home: root.to_path_buf(),
            skill_changed: changed,
            hook_changed: false,
            hook_backup: None,
            requires_hook_trust: false,
        })
    }
}

/// Inserts the managed section between our markers: replaces any existing
/// managed section in place, or appends one after the user's content. Every
/// byte outside the markers is preserved byte-for-byte — including CRLF line
/// endings and trailing blank lines. Markers are matched as whole lines
/// (prose that quotes a marker mid-line is left alone); any stray marker we
/// cannot pair unambiguously is an error, not a guess.
/// A line's content without its terminator, tolerating CRLF.
fn line_body(line: &str) -> &str {
    let stripped = line.strip_suffix('\n').unwrap_or(line);
    stripped.strip_suffix('\r').unwrap_or(stripped)
}

fn merge_managed(existing: &str, section: &str) -> Result<String> {
    // `split_inclusive` keeps each line's terminator with it, so splicing
    // whole lines never rewrites the endings of lines we do not own.
    let lines: Vec<&str> = existing.split_inclusive('\n').collect();
    let mut marker_lines = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line_body(line) == BEGIN_MARKER || line_body(line) == END_MARKER);
    if let Some((index, line)) = marker_lines.next() {
        let (begin, end) = if line_body(line) == BEGIN_MARKER {
            let Some((end_index, second)) = marker_lines.next() else {
                bail!("managed section has a begin marker with no end marker");
            };
            if line_body(second) != END_MARKER {
                bail!("found two begin markers with no end marker between them");
            }
            (index, end_index)
        } else {
            bail!("found an end marker with no begin marker before it");
        };
        if marker_lines.next().is_some() {
            bail!("found more than one managed marker pair");
        }
        let mut merged = String::with_capacity(existing.len() + section.len() + 64);
        merged.push_str(&lines[..begin].concat());
        merged.push_str(BEGIN_MARKER);
        merged.push('\n');
        merged.push_str(section);
        merged.push('\n');
        merged.push_str(END_MARKER);
        merged.push('\n');
        // The end-marker line keeps a trailing newline from the splice above;
        // everything after it lands byte-exactly as it was.
        merged.push_str(&lines[end + 1..].concat());
        return Ok(merged);
    }
    let trimmed = existing.trim_end();
    if trimmed.is_empty() {
        Ok(format!("{BEGIN_MARKER}\n{section}\n{END_MARKER}\n"))
    } else {
        Ok(format!(
            "{trimmed}\n\n{BEGIN_MARKER}\n{section}\n{END_MARKER}\n"
        ))
    }
}

/// Repo targets are user-owned and edited between runs, so every changed
/// install refreshes the backup instead of keeping the oldest-ever copy.
fn backup_fresh(path: &Path) -> Result<()> {
    if path.exists() {
        let backup = backup_path(path);
        fs::copy(path, &backup)?;
    }
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.agent-shunt.bak",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
    ))
}

fn rollback(snapshots: &[(PathBuf, PriorState)]) {
    for (path, state) in snapshots.iter().rev() {
        match state {
            PriorState::Bytes(content) => {
                let _ = atomic_write(path, content, 0o644);
            }
            PriorState::Absent => {
                if path.is_file() {
                    let _ = fs::remove_file(path);
                }
            }
            // Never delete a target we could not read: it existed, we just
            // could not capture it.
            PriorState::Unreadable => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::RepoInstaller;

    #[test]
    fn managed_section_appends_preserves_and_updates() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("AGENTS.md"), "# My rules\n\nBe careful.\n").unwrap();
        let first = RepoInstaller.install_agents_md(root.path()).unwrap();
        assert!(first.skill_changed);
        let after = fs::read_to_string(root.path().join("AGENTS.md")).unwrap();
        assert!(after.starts_with("# My rules\n\nBe careful.\n"));
        assert!(after.contains("<!-- agent-shunt:begin -->"));
        assert!(after.contains("agent-shunt retrieve"));

        // Idempotent: same section content reports no change.
        let second = RepoInstaller.install_agents_md(root.path()).unwrap();
        assert!(!second.skill_changed);

        // A user edit outside the markers survives a re-install; the
        // unchanged section is left alone (byte-identical merge, no rewrite).
        let mut edited = fs::read_to_string(root.path().join("AGENTS.md")).unwrap();
        edited.push_str("\nAdded later by the user.\n");
        fs::write(root.path().join("AGENTS.md"), edited).unwrap();
        let third = RepoInstaller.install_agents_md(root.path()).unwrap();
        assert!(!third.skill_changed);
        let final_content = fs::read_to_string(root.path().join("AGENTS.md")).unwrap();
        assert_eq!(
            final_content.matches("<!-- agent-shunt:begin -->").count(),
            1
        );
        assert!(final_content.contains("Added later by the user."));
    }

    #[test]
    fn managed_section_creates_missing_file_and_parent_dirs() {
        let root = tempdir().unwrap();
        let report = RepoInstaller.install_copilot(root.path()).unwrap();
        assert!(report.skill_changed);
        let content =
            fs::read_to_string(root.path().join(".github/copilot-instructions.md")).unwrap();
        assert!(content.starts_with("<!-- agent-shunt:begin -->\n"));
        assert!(content.trim_end().ends_with("<!-- agent-shunt:end -->"));
    }

    #[test]
    fn own_files_land_and_are_idempotent() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(".cursor/rules")).unwrap();
        fs::write(root.path().join(".cursor/rules/keep.mdc"), "user rule\n").unwrap();
        let first = RepoInstaller.install_cursor(root.path()).unwrap();
        assert!(first.skill_changed);
        let rule = root.path().join(".cursor/rules/agent-shunt.mdc");
        assert!(rule.is_file());
        assert!(
            fs::read_to_string(&rule)
                .unwrap()
                .starts_with("---\ndescription: ")
        );
        // The user's unrelated rule survives untouched.
        assert!(root.path().join(".cursor/rules/keep.mdc").is_file());
        let second = RepoInstaller.install_cursor(root.path()).unwrap();
        assert!(!second.skill_changed);
    }

    #[test]
    fn cline_and_roo_rule_files_land() {
        let root = tempdir().unwrap();
        RepoInstaller.install_cline(root.path()).unwrap();
        RepoInstaller.install_roo(root.path()).unwrap();
        assert!(root.path().join(".clinerules/agent-shunt.md").is_file());
        assert!(root.path().join(".roo/rules/agent-shunt.md").is_file());
    }

    /// `to_string` shows only the outermost context; `{:#}` walks the chain
    /// so assertions see the root cause.
    fn full_error(error: &anyhow::Error) -> String {
        format!("{error:#}")
    }

    #[test]
    fn non_utf8_managed_file_is_an_error_not_a_wipe() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("AGENTS.md"), b"# caf\xe9\n").unwrap();
        let error = RepoInstaller.install_agents_md(root.path()).unwrap_err();
        assert!(full_error(&error).contains("cannot read existing"));
        // The unreadable file must survive byte-for-byte.
        assert_eq!(
            fs::read(root.path().join("AGENTS.md")).unwrap(),
            b"# caf\xe9\n"
        );
    }

    #[test]
    fn orphan_marker_is_an_error_not_a_duplication() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("AGENTS.md"),
            "intro\n<!-- agent-shunt:end -->\n",
        )
        .unwrap();
        let error = RepoInstaller.install_agents_md(root.path()).unwrap_err();
        assert!(full_error(&error).contains("end marker with no begin marker"));
        assert_eq!(
            fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
            "intro\n<!-- agent-shunt:end -->\n"
        );
    }

    #[test]
    fn unpaired_begin_marker_is_an_error_too() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("AGENTS.md"),
            "<!-- agent-shunt:begin -->\nold section\n",
        )
        .unwrap();
        let error = RepoInstaller.install_agents_md(root.path()).unwrap_err();
        assert!(full_error(&error).contains("no end marker"));
    }

    #[cfg(unix)]
    #[test]
    fn rollback_never_deletes_an_unreadable_target() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempdir().unwrap();
        let target = root.path().join(".github/copilot-instructions.md");
        fs::create_dir_all(root.path().join(".github")).unwrap();
        fs::write(&target, b"secret mode-000 content\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
        let error = RepoInstaller.install_copilot(root.path()).unwrap_err();
        assert!(full_error(&error).contains("cannot read existing"));
        // Unreadable at snapshot time: rollback must leave it exactly as it
        // was instead of treating it as absent.
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"secret mode-000 content\n");
    }

    #[test]
    fn marker_quoted_mid_line_is_not_a_marker() {
        let existing = "notes about `<!-- agent-shunt:begin -->` syntax\n";
        let merged = super::merge_managed(existing, "section body").unwrap();
        assert!(merged.starts_with(existing));
        assert_eq!(merged.matches("<!-- agent-shunt:begin -->").count(), 2);
        let orphan = "notes about `<!-- agent-shunt:end -->` syntax\n";
        // Quoted markers are invisible to detection: no error, no mangling.
        assert!(
            super::merge_managed(orphan, "body")
                .unwrap()
                .contains(orphan.trim_end())
        );
    }

    #[test]
    fn trailing_blank_lines_after_end_survive_and_converge() {
        let root = tempdir().unwrap();
        let before = "A\n<!-- agent-shunt:begin -->\nold\n<!-- agent-shunt:end -->\n\n\n\n";
        fs::write(root.path().join("AGENTS.md"), before).unwrap();
        let first = RepoInstaller.install_agents_md(root.path()).unwrap();
        assert!(first.skill_changed);
        let after = fs::read_to_string(root.path().join("AGENTS.md")).unwrap();
        // Section body differs, so the first run rewrites — but every blank
        // line after the end marker must still be there.
        assert!(after.ends_with("<!-- agent-shunt:end -->\n\n\n\n"));
        // Second run is byte-stable: no newline shaved, no change reported.
        let second = RepoInstaller.install_agents_md(root.path()).unwrap();
        assert!(!second.skill_changed);
        assert_eq!(
            fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
            after
        );
    }

    #[test]
    fn crlf_endings_outside_the_section_survive() {
        let existing =
            "A\r\n<!-- agent-shunt:begin -->\r\nold\r\n<!-- agent-shunt:end -->\r\nTAIL\r\n";
        let merged = super::merge_managed(existing, "new body").unwrap();
        // Markers are recognized despite the \r line endings: the section is
        // replaced in place, not appended.
        assert_eq!(merged.matches("<!-- agent-shunt:begin -->").count(), 1);
        assert!(merged.contains("new body"));
        assert!(!merged.contains("old"));
        // User lines keep their CRLF endings byte-for-byte.
        assert!(merged.starts_with("A\r\n"));
        assert!(merged.ends_with("TAIL\r\n"));
    }

    #[test]
    fn missing_root_is_an_error_that_names_the_host() {
        let error = RepoInstaller
            .install_agents_md(std::path::Path::new("/nonexistent-repo-root"))
            .unwrap_err();
        assert!(error.to_string().contains("AGENTS.md target directory"));
    }
}
