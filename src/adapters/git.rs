use std::{io::Read, path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::{
    application::ports::ChangeSource,
    domain::{FileChange, LineRange},
};

pub struct GitChangeSource;

/// Cap on the text read from one untracked file for term extraction: only
/// enough to name its identifiers, never the whole file.
const MAX_UNTRACKED_TEXT: u64 = 64_000;

impl ChangeSource for GitChangeSource {
    fn changes(&self, cwd: &Path, base: &str) -> Result<Vec<FileChange>> {
        // A repository with no commits yet (or an unknown base ref) has no
        // tracked diff, but its untracked files are still reviewable.
        let tracked = match git(
            cwd,
            &[
                "diff",
                "--no-color",
                "--unified=0",
                // Added, copied, modified, renamed: files that exist in the
                // working tree with new content. Pure deletions have no
                // working-tree lines to search.
                "--diff-filter=ACMR",
                // Paths relative to `cwd` even when it is a subdirectory of
                // the repository, matching how the search reports them.
                "--relative",
                base,
                "--",
            ],
        ) {
            Ok(diff) => parse_diff(&diff),
            Err(error)
                if error.to_string().contains("unknown revision")
                    || error.to_string().contains("bad revision")
                    || error.to_string().contains("ambiguous argument") =>
            {
                Vec::new()
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot read local changes against {base}"));
            }
        };
        let mut changes = tracked;
        // `-z` emits raw NUL-separated paths, bypassing core.quotePath C-style
        // escaping that would never match a real filesystem path.
        for path in git(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])
            .context("cannot list untracked files")?
            .split('\0')
            .filter(|path| !path.is_empty())
        {
            changes.push(FileChange {
                path: path.to_owned(),
                // The whole file is new, so every line is a changed line and
                // no hunk restriction applies.
                hunks: Vec::new(),
                whole_file: true,
                changed_lines: read_untracked_text(cwd, path),
            });
        }
        Ok(changes)
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        // Pin the diff output format: `diff.noprefix` and `diff.mnemonicPrefix`
        // (w/i/c/m prefixes) would defeat `+++ b/` parsing, and `core.quotePath`
        // would escape non-ASCII paths in both diff and ls-files output.
        .args([
            "--no-optional-locks",
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.mnemonicprefix=false",
            "-c",
            "core.quotepath=false",
        ])
        .args(args)
        .output()
        .context("failed to execute git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses `git diff --unified=0` output into per-file changes: `+++ b/…`
/// selects the working-tree path, `@@ … +c,d @@` headers the new-side line
/// ranges, and `+` lines the added text.
fn parse_diff(output: &str) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    let mut current: Option<FileChange> = None;
    // Only the `+++` line following a `diff --git` line can be a file header;
    // anything else starting with `+++` is added *content* (an embedded diff
    // sample) and must not flush the file being parsed. With `--unified=0`
    // there are no context lines, so a bare `diff --git` line can only come
    // from git itself (added content always arrives prefixed with `+`).
    let mut expect_file_header = false;
    for line in output.lines() {
        if line.starts_with("diff --git ") {
            expect_file_header = true;
        } else if expect_file_header && line.starts_with("+++") {
            expect_file_header = false;
            if let Some(path) = line.strip_prefix("+++ b/") {
                if let Some(done) = current.take() {
                    changes.push(done);
                }
                current = Some(FileChange {
                    path: path.to_owned(),
                    hunks: Vec::new(),
                    whole_file: false,
                    changed_lines: String::new(),
                });
            } else {
                // `+++ /dev/null` (deleted) or a path we cannot attribute: skip
                // the file rather than mis-attribute its hunks.
                if let Some(done) = current.take() {
                    changes.push(done);
                }
            }
        } else if let Some(range) = parse_hunk_header(line) {
            if let (Some(change), Some(range)) = (current.as_mut(), range) {
                change.hunks.push(range);
            }
        } else if let Some(added) = line.strip_prefix('+')
            && let Some(change) = current.as_mut()
        {
            change.changed_lines.push_str(added);
            change.changed_lines.push('\n');
        }
    }
    if let Some(done) = current.take() {
        changes.push(done);
    }
    changes
}

/// Extracts the new-side line range from a `@@ -a,b +c,d @@` header.
/// `None` when the line is not a hunk header; `Some(None)` for a
/// zero-length new side (a pure deletion hunk with no working-tree lines).
fn parse_hunk_header(line: &str) -> Option<Option<LineRange>> {
    let rest = line.strip_prefix("@@ ")?;
    let plus = rest
        .split(' ')
        .find(|part| part.starts_with('+') && part.len() > 1)?;
    let body = plus.trim_start_matches('+');
    let mut parts = body.splitn(2, ',');
    let start = parts.next()?.parse().ok()?;
    let length = match parts.next() {
        // `+c` without a length means exactly one line.
        None => 1,
        Some(length) => length.parse().ok()?,
    };
    if length == 0 {
        return Some(None);
    }
    Some(Some(LineRange {
        start_line: start,
        end_line: start + length - 1,
    }))
}

/// Reads at most [`MAX_UNTRACKED_TEXT`] bytes: a stray multi-megabyte worktree
/// artifact must not be fully buffered just to name its identifiers.
fn read_untracked_text(cwd: &Path, path: &str) -> String {
    std::fs::File::open(cwd.join(path))
        .and_then(|mut file| {
            let mut buffer = String::new();
            file.by_ref()
                .take(MAX_UNTRACKED_TEXT)
                .read_to_string(&mut buffer)?;
            Ok(buffer)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use tempfile::tempdir;

    use super::{GitChangeSource, parse_diff};
    use crate::application::ports::ChangeSource;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.email=test@example.com", "-c", "user.name=test"])
            .args(args)
            .status()
            .expect("git command runs");
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo_with_change() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
        if !git_available() {
            return None;
        }
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        run_git(&root, &["init", "-q"]);
        fs::create_dir_all(root.join("src")).unwrap();
        // Ten lines so expected hunk positions are unambiguous.
        fs::write(
            root.join("src/a.rs"),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
        )
        .unwrap();
        fs::write(root.join("keep.rs"), "stable\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-qm", "base"]);
        // Replace lines 5–6; hunk header will be `@@ -5,2 +5,2 @@`.
        fs::write(
            root.join("src/a.rs"),
            "one\ntwo\nthree\nfour\nFIVE_CHANGED\nSIX_CHANGED\nseven\neight\nnine\nten\n",
        )
        .unwrap();
        fs::write(root.join("new_untracked.rs"), "fn brand_new() {}\n").unwrap();
        Some((dir, root))
    }

    #[test]
    fn reports_modified_hunks_and_untracked_files() {
        let Some((_dir, root)) = repo_with_change() else {
            return;
        };
        let changes = GitChangeSource.changes(&root, "HEAD").unwrap();
        let modified = changes
            .iter()
            .find(|change| change.path == "src/a.rs")
            .expect("modified file reported");
        assert_eq!(
            modified.hunks,
            vec![crate::domain::LineRange {
                start_line: 5,
                end_line: 6
            }]
        );
        assert!(!modified.whole_file);
        assert!(modified.changed_lines.contains("FIVE_CHANGED"));
        assert!(!modified.changed_lines.contains("one\n"));
        let untracked = changes
            .iter()
            .find(|change| change.path == "new_untracked.rs")
            .expect("untracked file reported");
        assert!(untracked.hunks.is_empty());
        assert!(untracked.whole_file);
        assert!(untracked.changed_lines.contains("brand_new"));
        // The untouched committed file is not a change.
        assert!(!changes.iter().any(|change| change.path == "keep.rs"));
    }

    #[test]
    fn embedded_diff_content_does_not_flush_the_current_file() {
        // An added line that itself starts with `+++ b/` (a diff sample pasted
        // into the changed file) must be treated as content, not a file header.
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 one
++++ b/fake.rs
 two
 three
diff --git a/keep.rs b/keep.rs
--- a/keep.rs
+++ b/keep.rs
@@ -1 +1 @@
-stable
+changed
";
        let changes = parse_diff(diff);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].path, "src/a.rs");
        assert_eq!(changes[0].hunks.len(), 1);
        assert!(changes[0].changed_lines.contains("+++ b/fake.rs"));
        assert_eq!(changes[1].path, "keep.rs");
        assert!(changes[1].changed_lines.contains("changed"));
    }

    #[test]
    fn pure_deletion_file_has_no_hunks_and_is_not_whole_file() {
        let diff = "\
diff --git a/gone.rs b/gone.rs
--- a/gone.rs
+++ b/gone.rs
@@ -1,2 +0,0 @@
-one
-two
";
        let changes = parse_diff(diff);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "gone.rs");
        assert!(changes[0].hunks.is_empty());
        assert!(!changes[0].whole_file);
    }

    #[test]
    fn untracked_only_repo_without_commits_still_reports_untracked() {
        if !git_available() {
            return;
        }
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("fresh.rs"), "fn fresh() {}\n").unwrap();
        let changes = GitChangeSource.changes(dir.path(), "HEAD").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "fresh.rs");
        assert!(changes[0].whole_file);
    }

    #[test]
    fn non_repo_is_still_an_error() {
        let dir = tempdir().unwrap();
        let error = GitChangeSource.changes(dir.path(), "HEAD").unwrap_err();
        assert!(error.to_string().contains("changes"));
    }
}
