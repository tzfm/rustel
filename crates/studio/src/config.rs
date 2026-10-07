//! Studio-owned configuration paths.
//!
//! Preferences, prebakes and user themes are written to `~/.rustel` on
//! every platform, beside the session tapes and the studio log. That is
//! the folder people expect to copy, inspect and version. The older
//! per-platform folders (`~/Library/Application Support/rustel`,
//! `~/.config/rustel`, `%APPDATA%\rustel`) are legacy locations, and the
//! import from them is non-destructive. Keep the path policy and that
//! import in one place so preferences, prebakes and user themes cannot
//! drift apart.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rustel_runtime::config_dir::{CONFIG_DIR_VAR, Platform};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfigLocations {
    canonical: PathBuf,
    legacy: Option<PathBuf>,
}

impl ConfigLocations {
    fn new(
        platform: Platform,
        home: Option<&OsStr>,
        xdg_config_home: Option<&OsStr>,
        appdata: Option<&OsStr>,
    ) -> Option<Self> {
        let canonical = rustel_runtime::config_dir::canonical_for(home)?;
        // Every platform used to keep these files somewhere of its own;
        // they are imported from there, never written there.
        let legacy =
            rustel_runtime::config_dir::legacy_for(platform, home, xdg_config_home, appdata);
        Some(Self { canonical, legacy })
    }

    /// Import the old folder's files without modifying their source or replacing
    /// anything already present at the canonical path. A failed best-effort
    /// import does not make the canonical location itself unavailable.
    fn prepare(&self) {
        if let Some(legacy) = &self.legacy {
            let _ = import_legacy_config(legacy, &self.canonical);
        }
    }

    fn read_path(&self, relative: &Path) -> PathBuf {
        let canonical = self.canonical.join(relative);
        if canonical.exists() {
            return canonical;
        }
        self.legacy
            .as_ref()
            .map(|legacy| legacy.join(relative))
            .filter(|legacy| is_real_file(legacy).unwrap_or(false))
            .unwrap_or(canonical)
    }

    fn read_directories(&self, relative: &Path) -> Vec<PathBuf> {
        let mut directories = vec![self.canonical.join(relative)];
        if let Some(legacy) = &self.legacy {
            let legacy = legacy.join(relative);
            if legacy != directories[0] && is_real_directory(&legacy).unwrap_or(false) {
                directories.push(legacy);
            }
        }
        directories
    }
}

fn system_locations() -> Option<ConfigLocations> {
    if let Some(explicit) = std::env::var_os(CONFIG_DIR_VAR).filter(|value| !value.is_empty()) {
        let canonical = PathBuf::from(explicit);
        return Some(ConfigLocations {
            canonical,
            legacy: None,
        });
    }
    let home = rustel_runtime::config_dir::home();
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let appdata = std::env::var_os("APPDATA");
    ConfigLocations::new(
        rustel_runtime::config_dir::current_platform()?,
        home.as_deref(),
        xdg.as_deref(),
        appdata.as_deref(),
    )
}

/// The directory new and changed Studio configuration is always written to.
pub(crate) fn directory() -> Option<PathBuf> {
    let locations = system_locations()?;
    locations.prepare();
    Some(locations.canonical)
}

/// The authoritative readable path for one file. The canonical file always
/// wins; the old folder's file is only a fallback when importing it failed.
pub(crate) fn read_path(relative: impl AsRef<Path>) -> Option<PathBuf> {
    let locations = system_locations()?;
    locations.prepare();
    Some(locations.read_path(relative.as_ref()))
}

/// Canonical then legacy directories for read-only discovery. Normally the
/// import has already populated the first; retaining the second makes custom
/// themes visible even when the canonical directory cannot be created.
pub(crate) fn read_directories(relative: impl AsRef<Path>) -> Vec<PathBuf> {
    let Some(locations) = system_locations() else {
        return Vec::new();
    };
    locations.prepare();
    locations.read_directories(relative.as_ref())
}

/// Import only the files the Studio owns: its preferences, global prebake,
/// and flat JSON theme files. Symlinks, nested theme folders, and unrelated
/// files are intentionally ignored.
fn import_legacy_config(source: &Path, destination: &Path) -> io::Result<()> {
    if !is_real_directory(source)? {
        return Ok(());
    }
    ensure_real_directory(destination)?;

    let mut first_error = None;
    for name in [
        super::prefs::PREFS_FILE_NAME,
        super::prebake::PREBAKE_FILE_NAME,
    ] {
        remember_first_error(
            &mut first_error,
            copy_known_regular_file(&source.join(name), &destination.join(name)),
        );
    }

    let source_themes = source.join("themes");
    if is_real_directory(&source_themes)? {
        let destination_themes = destination.join("themes");
        if let Err(error) = ensure_real_directory(&destination_themes) {
            remember_first_error(&mut first_error, Err(error));
        } else {
            for entry in fs::read_dir(source_themes)? {
                let result = (|| {
                    let entry = entry?;
                    let file_type = entry.file_type()?;
                    let path = entry.path();
                    if !file_type.is_file()
                        || path.extension().is_none_or(|extension| extension != "json")
                    {
                        return Ok(());
                    }
                    copy_missing_file(&path, &destination_themes.join(entry.file_name()))
                })();
                remember_first_error(&mut first_error, result);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn remember_first_error(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(error) = result
        && first_error.is_none()
    {
        *first_error = Some(error);
    }
}

fn is_real_directory(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn is_real_file(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn copy_known_regular_file(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    copy_missing_file(source, destination)
}

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a regular directory", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path),
        Err(error) => Err(error),
    }
}

fn copy_missing_file(source: &Path, destination: &Path) -> io::Result<()> {
    if destination.exists() {
        return Ok(());
    }
    let Some(parent) = destination.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration destination has no parent",
        ));
    };
    ensure_real_directory(parent)?;
    let mut source = fs::File::open(source)?;
    let mut destination_file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Err(error) = io::copy(&mut source, &mut destination_file) {
        drop(destination_file);
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

/// Point this process's configuration at a directory of its own.
///
/// Called by every test that builds an `App`. Without it a test reads the real
/// preferences of whoever is logged in and appends to the real session log -
/// concurrently, from every test thread, into a file a running studio has open.
/// That corrupts the log with interleaved writes and makes the suite's
/// behaviour depend on the machine it runs on.
///
/// Idempotent and process-wide, because the environment is.
#[cfg(test)]
pub(crate) fn isolate_for_tests() {
    use std::sync::OnceLock;
    static SANDBOX: OnceLock<PathBuf> = OnceLock::new();
    let directory = SANDBOX.get_or_init(|| {
        // Keep the pre-isolation sample cache available to tests that need
        // pinned manifests. App tests still write to the isolated cache.
        let _ = sample_cache_before_test_isolation();
        let mut path = std::env::temp_dir();
        path.push(format!("rustel-test-config-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    });
    // Tests read colours in the terminal output. A `NO_COLOR` from the
    // shell must not remove them.
    crossterm::style::force_color_output(true);
    // SAFETY: set once per process, before any configuration is read.
    unsafe {
        std::env::set_var(CONFIG_DIR_VAR, directory);
        // The session log follows the configuration directory, but a
        // `RUSTEL_SESSION_DIR` already in the environment would still send
        // every test's writes to the real studio.log.
        std::env::set_var(
            rustel_runtime::product::SESSION_DIRECTORY_ENV,
            directory.join("sessions"),
        );
    }
}

/// The sample cache selected before App tests redirect the config directory.
/// An explicit `RUSTEL_SAMPLE_CACHE` remains the selected path in CI.
#[cfg(test)]
pub(crate) fn sample_cache_before_test_isolation() -> PathBuf {
    use std::sync::OnceLock;
    static SAMPLE_CACHE: OnceLock<PathBuf> = OnceLock::new();
    SAMPLE_CACHE
        .get_or_init(rustel_runtime::samples::sample_cache_dir)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> &OsStr {
        OsStr::new(value)
    }

    #[test]
    fn macos_uses_dot_rustel_and_remembers_the_application_support_location() {
        let locations = ConfigLocations::new(
            Platform::Macos,
            Some(os("/Users/EXAMPLE")),
            Some(os("/tmp/xdg-is-not-the-macos-default")),
            None,
        )
        .unwrap();
        assert_eq!(locations.canonical, Path::new("/Users/EXAMPLE/.rustel"));
        assert_eq!(
            locations.legacy.as_deref(),
            Some(Path::new(
                "/Users/EXAMPLE/Library/Application Support/rustel"
            ))
        );
    }

    #[test]
    fn linux_and_windows_use_dot_rustel_and_remember_their_old_folders() {
        let xdg = ConfigLocations::new(
            Platform::Unix,
            Some(os("/home/EXAMPLE")),
            Some(os("/srv/config")),
            None,
        )
        .unwrap();
        assert_eq!(xdg.canonical, Path::new("/home/EXAMPLE/.rustel"));
        assert_eq!(xdg.legacy.as_deref(), Some(Path::new("/srv/config/rustel")));

        let unix = ConfigLocations::new(
            Platform::Unix,
            Some(os("/home/EXAMPLE")),
            Some(os("")),
            None,
        )
        .unwrap();
        assert_eq!(unix.canonical, Path::new("/home/EXAMPLE/.rustel"));
        assert_eq!(
            unix.legacy.as_deref(),
            Some(Path::new("/home/EXAMPLE/.config/rustel"))
        );

        let appdata = r"C:\Users\EXAMPLE\AppData\Roaming";
        let windows = ConfigLocations::new(
            Platform::Windows,
            Some(os(r"C:\Users\EXAMPLE")),
            None,
            Some(os(appdata)),
        )
        .unwrap();
        assert_eq!(
            windows.canonical,
            Path::new(r"C:\Users\EXAMPLE").join(".rustel"),
            "the settings sit beside the sessions, not in AppData"
        );
        assert_eq!(windows.legacy, Some(Path::new(appdata).join("rustel")));

        let windows_xdg = ConfigLocations::new(
            Platform::Windows,
            Some(os(r"C:\Users\EXAMPLE")),
            Some(os(r"D:\portable-config")),
            Some(os(appdata)),
        )
        .unwrap();
        assert_eq!(
            windows_xdg.canonical,
            Path::new(r"C:\Users\EXAMPLE").join(".rustel"),
            "RUSTEL_CONFIG_DIR moves the folder; XDG only names the old one"
        );
        assert_eq!(
            windows_xdg.legacy,
            Some(Path::new(r"D:\portable-config").join("rustel"))
        );

        assert!(
            ConfigLocations::new(Platform::Windows, None, None, Some(os(appdata))).is_none(),
            "no home, no folder"
        );
    }

    #[test]
    fn legacy_import_is_scoped_and_never_overwrites_or_deletes() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("Application Support").join("rustel");
        let canonical = directory.path().join(".rustel");
        fs::create_dir_all(legacy.join("themes")).unwrap();
        fs::create_dir_all(&canonical).unwrap();
        fs::write(legacy.join("studio.json"), "legacy").unwrap();
        fs::write(legacy.join("prebake.strudel"), "setup").unwrap();
        fs::write(legacy.join("themes").join("night.json"), "night").unwrap();
        fs::write(legacy.join("unrelated.txt"), "leave me").unwrap();
        fs::create_dir_all(legacy.join("themes").join("nested")).unwrap();
        fs::write(
            legacy.join("themes").join("nested").join("hidden.json"),
            "nested",
        )
        .unwrap();
        fs::write(canonical.join("studio.json"), "canonical-newer").unwrap();

        import_legacy_config(&legacy, &canonical).unwrap();

        assert_eq!(
            fs::read_to_string(canonical.join("studio.json")).unwrap(),
            "canonical-newer"
        );
        assert_eq!(
            fs::read_to_string(canonical.join("themes").join("night.json")).unwrap(),
            "night"
        );
        assert_eq!(
            fs::read_to_string(canonical.join("prebake.strudel")).unwrap(),
            "setup"
        );
        assert!(!canonical.join("unrelated.txt").exists());
        assert!(!canonical.join("themes").join("nested").exists());
        assert_eq!(
            fs::read_to_string(legacy.join("studio.json")).unwrap(),
            "legacy"
        );
        assert_eq!(
            fs::read_to_string(legacy.join("themes").join("night.json")).unwrap(),
            "night"
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_import_does_not_follow_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("legacy");
        let canonical = directory.path().join("canonical");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("studio.json"), "outside prefs").unwrap();
        fs::write(outside.join("escape.json"), "outside theme").unwrap();
        symlink(outside.join("studio.json"), legacy.join("studio.json")).unwrap();
        symlink(&outside, legacy.join("themes")).unwrap();

        import_legacy_config(&legacy, &canonical).unwrap();

        assert!(!canonical.join("studio.json").exists());
        assert!(!canonical.join("themes").exists());
        let locations = ConfigLocations {
            canonical: canonical.clone(),
            legacy: Some(legacy.clone()),
        };
        assert_eq!(
            locations.read_path(Path::new("studio.json")),
            canonical.join("studio.json"),
            "a rejected legacy symlink must not return through the read fallback"
        );
        assert_eq!(
            locations.read_directories(Path::new("themes")),
            vec![canonical.join("themes")],
            "a rejected legacy directory symlink must not return through discovery"
        );
        assert_eq!(
            fs::read_to_string(outside.join("studio.json")).unwrap(),
            "outside prefs"
        );
    }

    #[test]
    fn canonical_reads_win_and_legacy_is_only_a_missing_file_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let canonical = directory.path().join(".rustel");
        let legacy = directory.path().join("legacy");
        let locations = ConfigLocations {
            canonical: canonical.clone(),
            legacy: Some(legacy.clone()),
        };
        fs::create_dir_all(&canonical).unwrap();
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("prebake.strudel"), "old").unwrap();
        assert_eq!(
            locations.read_path(Path::new("prebake.strudel")),
            legacy.join("prebake.strudel")
        );
        fs::write(canonical.join("prebake.strudel"), "new").unwrap();
        assert_eq!(
            locations.read_path(Path::new("prebake.strudel")),
            canonical.join("prebake.strudel")
        );
    }

    #[test]
    fn failed_import_still_reads_the_legacy_file() {
        let directory = tempfile::tempdir().unwrap();
        let canonical = directory.path().join("canonical-blocked");
        let legacy = directory.path().join("legacy");
        fs::write(&canonical, "a file blocks the destination directory").unwrap();
        fs::create_dir_all(legacy.join("themes")).unwrap();
        fs::write(legacy.join("studio.json"), "legacy prefs").unwrap();
        let locations = ConfigLocations {
            canonical,
            legacy: Some(legacy.clone()),
        };

        locations.prepare();

        assert_eq!(
            locations.read_path(Path::new("studio.json")),
            legacy.join("studio.json")
        );
        assert_eq!(
            locations.read_directories(Path::new("themes"))[1],
            legacy.join("themes")
        );
    }
}
