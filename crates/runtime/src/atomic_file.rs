//! Replacing a file in one rename: a reader finds the old contents or the
//! new ones, never a mix, and a failed write leaves the old file as it was.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `bytes` to a new file beside `path`, named `{prefix}{pid}-{n}.tmp`
/// and given `path`'s permissions when it exists, sync it, `commit` it over
/// `path`, then sync the folder. The new file is removed whenever it is not
/// committed. The commit seam makes replacement failure testable.
pub(crate) fn replace_file(
    path: &Path,
    prefix: &str,
    bytes: &[u8],
    commit: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    static NEXT_REPLACEMENT: AtomicU64 = AtomicU64::new(0);

    struct Pending {
        path: PathBuf,
        file: Option<File>,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            // Close before unlinking so cleanup also works on Windows.
            self.file.take();
            let _ = std::fs::remove_file(&self.path);
        }
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut pending = loop {
        let sequence = NEXT_REPLACEMENT.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("{prefix}{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                break Pending {
                    path: candidate,
                    file: Some(file),
                };
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let file = pending.file.as_mut().expect("new temporary file");
    if let Ok(metadata) = std::fs::metadata(path) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(pending.file.take());
    commit(&pending.path, path)?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_replacement_keeps_original_bytes_and_removes_the_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original.rustel-session");
        let original = b"the original tape must survive\n";
        std::fs::write(&path, original).unwrap();
        let result = replace_file(
            &path,
            ".rustel-session-",
            b"complete replacement\n",
            |pending, target| {
                assert_eq!(target, path);
                assert_eq!(std::fs::read(pending).unwrap(), b"complete replacement\n");
                assert_eq!(std::fs::read(target).unwrap(), original);
                Err(std::io::Error::new(
                    ErrorKind::PermissionDenied,
                    "injected replacement failure",
                ))
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        replace_file(
            &path,
            ".rustel-session-",
            b"complete replacement\n",
            |pending, target| std::fs::rename(pending, target),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "complete replacement\n"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
