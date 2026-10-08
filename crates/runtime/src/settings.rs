//! User settings shared by Rustel's command line and Studio.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

/// Whether automatic update checks are enabled. A missing setting defaults to true.
pub fn check_updates() -> io::Result<bool> {
    check_updates_at(&settings_path()?)
}

/// Save the update-check preference without changing other user settings.
pub fn set_check_updates(enabled: bool) -> io::Result<()> {
    set_check_updates_at(&settings_path()?, enabled)
}

fn settings_path() -> io::Result<PathBuf> {
    crate::config_dir::canonical()
        .map(|directory| directory.join("rustel.json"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no configuration directory is available",
            )
        })
}

fn check_updates_at(path: &Path) -> io::Result<bool> {
    Ok(read_settings(path)?
        .get("check_updates")
        .and_then(Value::as_bool)
        .unwrap_or(true))
}

fn set_check_updates_at(path: &Path, enabled: bool) -> io::Result<()> {
    let mut settings = read_settings(path)?;
    settings.insert("check_updates".to_owned(), Value::Bool(enabled));
    let mut bytes = serde_json::to_vec_pretty(&settings).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(invalid_settings("rustel.json exceeds 64 KiB"));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    crate::atomic_file::replace_file(path, ".rustel-settings-", &bytes, |pending, target| {
        std::fs::rename(pending, target)
    })
}

fn read_settings(path: &Path) -> io::Result<Map<String, Value>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(invalid_settings("rustel.json must be a regular file"));
    }
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(invalid_settings("rustel.json exceeds 64 KiB"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A replacement FIFO must not block between metadata and open.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid_settings("rustel.json must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(invalid_settings("rustel.json exceeds 64 KiB"));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| invalid_settings(format!("invalid rustel.json: {error}")))?;
    let Value::Object(settings) = value else {
        return Err(invalid_settings("rustel.json must contain a JSON object"));
    };
    if settings
        .get("check_updates")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid_settings("check_updates must be true or false"));
    }
    Ok(settings)
}

fn invalid_settings(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_default_to_enabled_without_creating_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/rustel.json");
        assert!(check_updates_at(&path).unwrap());
        assert!(!path.parent().unwrap().exists());

        let path = directory.path().join("rustel.json");
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        assert!(check_updates_at(&path).unwrap());
    }

    #[test]
    fn reads_both_update_check_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rustel.json");
        for enabled in [true, false] {
            std::fs::write(&path, format!(r#"{{"check_updates":{enabled}}}"#)).unwrap();
            assert_eq!(check_updates_at(&path).unwrap(), enabled);
        }
    }

    #[test]
    fn setting_update_checks_creates_the_directory_and_preserves_other_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/rustel.json");
        set_check_updates_at(&path, false).unwrap();
        assert!(!check_updates_at(&path).unwrap());

        std::fs::write(&path, r#"{"other":{"value":42},"check_updates":false}"#).unwrap();
        set_check_updates_at(&path, true).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            serde_json::json!({"other": {"value": 42}, "check_updates": true})
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn invalid_settings_are_reported_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rustel.json");
        for original in [
            "{",
            "[]",
            "null",
            r#"{"check_updates":"false"}"#,
            r#"{"check_updates":null}"#,
            r#"{"check_updates":0}"#,
        ] {
            std::fs::write(&path, original).unwrap();
            assert_eq!(
                check_updates_at(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert!(set_check_updates_at(&path, true).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn oversized_settings_are_reported_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rustel.json");
        let original = format!("{{}}{}", " ".repeat(MAX_SETTINGS_BYTES as usize));
        std::fs::write(&path, &original).unwrap();
        assert!(check_updates_at(&path).is_err());
        assert!(set_check_updates_at(&path, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn a_directory_cannot_be_used_as_settings() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            check_updates_at(directory.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(set_check_updates_at(directory.path(), false).is_err());
        assert!(directory.path().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_rejected_without_opening_it() {
        use std::os::unix::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rustel.json");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is a live, NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert_eq!(
            check_updates_at(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(set_check_updates_at(&path, false).is_err());
    }
}
