use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Take},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    application::ports::DocumentLoader,
    domain::{Document, Limits, LineRange, LoadedDocuments},
};

pub struct SecureFilesystem;

impl DocumentLoader for SecureFilesystem {
    fn load(&self, root: &Path, paths: &[PathBuf], limits: &Limits) -> Result<LoadedDocuments> {
        if paths.is_empty() {
            bail!("at least one --path is required");
        }
        if paths.len() > limits.max_files {
            bail!(
                "too many files: {} exceeds {}",
                paths.len(),
                limits.max_files
            );
        }
        let root = fs::canonicalize(root)
            .with_context(|| format!("cannot resolve working directory: {}", root.display()))?;
        let mut seen = HashSet::new();
        let mut documents = Vec::new();
        let mut total_bytes = 0usize;
        for input in paths {
            let relative = if input.is_absolute() {
                input
                    .strip_prefix(&root)
                    .map(Path::to_path_buf)
                    .map_err(|_| {
                        anyhow::anyhow!("path escapes working directory: {}", input.display())
                    })?
            } else {
                input.clone()
            };
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
            {
                bail!("path escapes working directory: {}", input.display());
            }
            let relative = relative
                .components()
                .filter_map(|component| match component {
                    Component::Normal(value) => Some(value),
                    _ => None,
                })
                .collect::<PathBuf>();
            if !seen.insert(relative.clone()) {
                continue;
            }
            let mut file = open_beneath(&root, &relative)
                .with_context(|| format!("cannot open path: {}", input.display()))?;
            let before = file.metadata()?;
            if !before.is_file() {
                bail!("path is not a regular file: {}", input.display());
            }
            let size = usize::try_from(before.len()).unwrap_or(usize::MAX);
            if size > limits.max_file_bytes {
                bail!(
                    "file exceeds {} bytes: {}",
                    limits.max_file_bytes,
                    input.display()
                );
            }
            let mut buffer = Vec::with_capacity(size);
            let mut bounded: Take<&mut File> = (&mut file).take((limits.max_file_bytes + 1) as u64);
            bounded.read_to_end(&mut buffer)?;
            if buffer.len() > limits.max_file_bytes {
                bail!(
                    "file exceeds {} bytes: {}",
                    limits.max_file_bytes,
                    input.display()
                );
            }
            let after = file.metadata()?;
            if !same_file_snapshot(&before, &after) || buffer.len() as u64 != after.len() {
                bail!("file changed while being read: {}", input.display());
            }
            // A binary or non-UTF-8 file is never useful evidence. Skip it
            // rather than aborting the whole batch: automatic retrieval can
            // select an asset (matched by filename) alongside real source, and
            // one such file must not sink every other selected document.
            if buffer.iter().take(8_192).any(|byte| *byte == 0) {
                continue;
            }
            let byte_len = buffer.len();
            let Ok(text) = String::from_utf8(buffer) else {
                continue;
            };
            total_bytes = total_bytes.saturating_add(byte_len);
            if total_bytes > limits.max_total_bytes {
                bail!(
                    "input exceeds aggregate limit of {} bytes",
                    limits.max_total_bytes
                );
            }
            let lines = text
                .split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
                .collect::<Vec<_>>();
            let line_count = lines.len();
            let numbered_content = lines
                .iter()
                .enumerate()
                .map(|(index, line)| format!("{}: {line}", index + 1))
                .collect::<Vec<_>>()
                .join("\n");
            documents.push(Document {
                path: relative.to_string_lossy().into_owned(),
                bytes: byte_len,
                line_count,
                lines,
                numbered_content,
                allowed_ranges: vec![LineRange {
                    start_line: 1,
                    end_line: line_count,
                }],
            });
        }
        Ok(LoadedDocuments {
            documents,
            total_bytes,
        })
    }
}

#[cfg(unix)]
fn open_beneath(root: &Path, relative: &Path) -> std::io::Result<File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
            unix::fs::OpenOptionsExt,
        },
    };

    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(root)?;
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-normal path component",
            ));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))?;
        let is_final = index + 1 == components.len();
        let flags = if is_final {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        } else {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY
        };
        // SAFETY: both the directory descriptor and CString remain valid for the call.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        let opened = unsafe { File::from_raw_fd(fd) };
        if is_final {
            return Ok(opened);
        }
        directory = opened;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "empty path",
    ))
}

#[cfg(not(unix))]
fn open_beneath(root: &Path, relative: &Path) -> std::io::Result<File> {
    let resolved = fs::canonicalize(root.join(relative))?;
    if !resolved.starts_with(root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "path escapes root",
        ));
    }
    OpenOptions::new().read(true).open(resolved)
}

#[cfg(unix)]
fn same_file_snapshot(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file_snapshot(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use crate::{application::ports::DocumentLoader, domain::Limits};

    use super::SecureFilesystem;

    fn limits() -> Limits {
        Limits {
            max_files: 3,
            max_file_bytes: 1000,
            max_total_bytes: 2000,
            ..Limits::default()
        }
    }

    #[test]
    fn loads_text_with_stable_lines() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/a.js"), "one\r\ntwo").unwrap();
        let loaded = SecureFilesystem
            .load(root.path(), &[PathBuf::from("src/a.js")], &limits())
            .unwrap();
        assert_eq!(loaded.documents[0].path, "src/a.js");
        assert_eq!(loaded.documents[0].numbered_content, "1: one\n2: two");
    }

    #[test]
    fn skips_binary_without_aborting_batch() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("binary"), [1, 0, 2]).unwrap();
        fs::write(root.path().join("real.rs"), "let ok = 1;").unwrap();
        let loaded = SecureFilesystem
            .load(
                root.path(),
                &[PathBuf::from("binary"), PathBuf::from("real.rs")],
                &limits(),
            )
            .unwrap();
        assert_eq!(loaded.documents.len(), 1);
        assert_eq!(loaded.documents[0].path, "real.rs");
    }

    #[test]
    fn skips_invalid_utf8_even_after_binary_probe() {
        let root = tempdir().unwrap();
        let mut bytes = vec![b'a'; 9_000];
        bytes.push(0xff);
        fs::write(root.path().join("invalid"), bytes).unwrap();
        let mut generous = limits();
        generous.max_file_bytes = 10_000;
        generous.max_total_bytes = 10_000;
        let loaded = SecureFilesystem
            .load(root.path(), &[PathBuf::from("invalid")], &generous)
            .unwrap();
        assert!(loaded.documents.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret"), "secret").unwrap();
        symlink(outside.path().join("secret"), root.path().join("escape")).unwrap();
        let error = SecureFilesystem
            .load(root.path(), &[PathBuf::from("escape")], &limits())
            .unwrap_err();
        assert!(error.to_string().contains("cannot open path"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_ancestor_even_when_target_is_inside_root() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        fs::write(root.path().join("real/file"), "safe").unwrap();
        symlink(root.path().join("real"), root.path().join("alias")).unwrap();
        let error = SecureFilesystem
            .load(root.path(), &[PathBuf::from("alias/file")], &limits())
            .unwrap_err();
        assert!(error.to_string().contains("cannot open path"));
    }
}
