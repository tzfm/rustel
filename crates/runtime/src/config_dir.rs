//! Where rustel keeps what is its own: one folder for the configuration,
//! the sets, the sessions and the sample cache, so there is one place to
//! copy, inspect and version. `~/.rustel` on every platform - Windows's
//! `%USERPROFILE%` is the home when `HOME` is unset - and wherever
//! `RUSTEL_CONFIG_DIR` says, on any of them.
//!
//! The studio's configuration module builds on this - it adds the import
//! from the folder each platform used before - and the sample cache and the
//! session log read it directly, which is why the location rule lives here.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// An explicit configuration directory, overriding every platform rule.
///
/// It exists so a process can be told where its own state lives instead of
/// finding the one belonging to whoever is logged in. That is a reasonable
/// thing to want in general - a portable install, a second profile - and it
/// is a requirement for the test suite, which otherwise reads the real
/// preferences and appends to the real session log of a studio that may be
/// playing at the time.
pub const CONFIG_DIR_VAR: &str = "RUSTEL_CONFIG_DIR";

/// An operating system family whose configuration-folder rules differ.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Macos,
    Unix,
    Windows,
}

/// The platform this build runs on, or `None` where no rule applies.
pub fn current_platform() -> Option<Platform> {
    #[cfg(target_os = "macos")]
    return Some(Platform::Macos);
    #[cfg(all(unix, not(target_os = "macos")))]
    return Some(Platform::Unix);
    #[cfg(windows)]
    return Some(Platform::Windows);
    #[cfg(not(any(unix, windows)))]
    return None;
}

/// The home folder: `HOME`, else `USERPROFILE`, which is where Windows
/// keeps it when nothing has set `HOME`.
pub fn home() -> Option<OsString> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
}

/// The canonical directory under `home`. It is the same on every platform,
/// so a folder copied from one machine is where another looks for it.
pub fn canonical_for(home: Option<&OsStr>) -> Option<PathBuf> {
    home.filter(|value| !value.is_empty())
        .map(|home| Path::new(home).join(crate::product::DATA_DIRECTORY_NAME))
}

/// Where `platform` kept the configuration before it moved to `~/.rustel`:
/// Application Support on macOS, `$XDG_CONFIG_HOME/rustel` or
/// `~/.config/rustel` on other Unix systems, and `%XDG_CONFIG_HOME%\rustel`
/// or `%APPDATA%\rustel` on Windows.
pub fn legacy_for(
    platform: Platform,
    home: Option<&OsStr>,
    xdg_config_home: Option<&OsStr>,
    appdata: Option<&OsStr>,
) -> Option<PathBuf> {
    let set = |value: Option<&OsStr>| value.filter(|value| !value.is_empty()).map(PathBuf::from);
    let (home, xdg, appdata) = (set(home), set(xdg_config_home), set(appdata));
    let name = crate::product::CACHE_DIRECTORY_NAME;
    match platform {
        Platform::Macos => Some(home?.join("Library").join("Application Support").join(name)),
        Platform::Unix => Some(
            xdg.or_else(|| home.map(|home| home.join(".config")))?
                .join(name),
        ),
        Platform::Windows => Some(xdg.or(appdata)?.join(name)),
    }
}

/// The canonical directory on this machine: the override, else `.rustel`
/// in the home folder. Nothing is created.
pub fn canonical() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(CONFIG_DIR_VAR).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    canonical_for(home().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_keeps_one_dot_rustel_and_remembers_its_old_folder() {
        assert_eq!(
            canonical_for(Some(OsStr::new("/Users/EXAMPLE"))),
            Some(PathBuf::from("/Users/EXAMPLE/.rustel"))
        );
        assert_eq!(
            canonical_for(Some(OsStr::new(r"C:\Users\E"))),
            Some(Path::new(r"C:\Users\E").join(".rustel"))
        );
        assert_eq!(canonical_for(None), None);
        assert_eq!(
            canonical_for(Some(OsStr::new(""))),
            None,
            "an empty HOME is no home"
        );

        let home = Some(OsStr::new("/home/E"));
        assert_eq!(
            legacy_for(Platform::Macos, home, None, None),
            Some(PathBuf::from("/home/E/Library/Application Support/rustel"))
        );
        assert_eq!(
            legacy_for(Platform::Unix, home, None, None),
            Some(PathBuf::from("/home/E/.config/rustel"))
        );
        assert_eq!(
            legacy_for(Platform::Unix, home, Some(OsStr::new("/tmp/xdg")), None),
            Some(PathBuf::from("/tmp/xdg/rustel"))
        );
        let appdata = Some(OsStr::new(r"C:\Users\E\AppData\Roaming"));
        assert_eq!(
            legacy_for(Platform::Windows, None, None, appdata),
            Some(Path::new(r"C:\Users\E\AppData\Roaming").join("rustel"))
        );
        assert_eq!(
            legacy_for(
                Platform::Windows,
                None,
                Some(OsStr::new(r"D:\xdg")),
                appdata
            ),
            Some(Path::new(r"D:\xdg").join("rustel"))
        );
        assert_eq!(legacy_for(Platform::Windows, home, None, None), None);
    }
}
