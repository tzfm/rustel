//! Where the VST3 bundles are, and where the plugin library is in a bundle.

use std::path::{Path, PathBuf};

/// A vendor folder in a VST3 folder is one or two levels deep.
const MAX_DEPTH: usize = 4;

/// The folders the VST3 standard names for this system.
pub fn default_folders() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut folders = Vec::new();
    if cfg!(target_os = "macos") {
        folders.extend(var("HOME").map(|home| home.join("Library/Audio/Plug-Ins/VST3")));
        folders.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    } else if cfg!(windows) {
        folders.extend(var("LOCALAPPDATA").map(|local| local.join("Programs/Common/VST3")));
        folders.extend(var("CommonProgramFiles").map(|common| common.join("VST3")));
    } else {
        folders.extend(var("HOME").map(|home| home.join(".vst3")));
        folders.push(PathBuf::from("/usr/lib/vst3"));
        folders.push(PathBuf::from("/usr/local/lib/vst3"));
    }
    folders
}

/// Every `.vst3` bundle under the folders, sorted, each one time.
pub fn find_bundles(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    for folder in folders {
        walk(folder, MAX_DEPTH, &mut bundles);
    }
    bundles.sort();
    bundles.dedup();
    bundles
}

fn walk(folder: &Path, depth: usize, bundles: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_bundle = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vst3"));
        if is_bundle {
            bundles.push(path);
        } else if depth > 0 && path.is_dir() {
            walk(&path, depth - 1, bundles);
        }
    }
}

/// The plugin library of a bundle for this system.
pub(crate) fn binary_path(bundle: &Path) -> Option<PathBuf> {
    if bundle.is_file() {
        return Some(bundle.to_path_buf());
    }
    let stem = bundle.file_stem()?;
    let contents = bundle.join("Contents");
    let (folder, name) = if cfg!(target_os = "macos") {
        (contents.join("MacOS"), PathBuf::from(stem))
    } else if cfg!(windows) {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            arch => arch,
        };
        (
            contents.join(format!("{arch}-win")),
            PathBuf::from(stem).with_extension("vst3"),
        )
    } else {
        (
            contents.join(format!("{}-linux", std::env::consts::ARCH)),
            PathBuf::from(stem).with_extension("so"),
        )
    };
    let named = folder.join(name);
    if named.is_file() {
        return Some(named);
    }
    // The library name does not always match the bundle name. A folder
    // with one file has one answer.
    let mut files = std::fs::read_dir(&folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file());
    let only = files.next()?;
    files.next().is_none().then_some(only)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_are_found_in_vendor_folders_and_a_bundle_is_not_searched() {
        let root = std::env::temp_dir().join(format!("rustel-vst3-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let inner = root.join("Vendor/Deep.vst3/Contents/Nested.vst3");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(root.join("Top.VST3")).unwrap();
        std::fs::write(root.join("notes.txt"), "").unwrap();

        let found = find_bundles(&[root.clone(), root.clone(), root.join("missing")]);
        assert_eq!(
            found,
            [root.join("Top.VST3"), root.join("Vendor/Deep.vst3")]
        );
        assert_eq!(binary_path(&root.join("Top.VST3")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
