//! The scan cache: what the load test found in each bundle.
//!
//! The load test starts a process and loads the plugin there, so a test
//! takes about as long as a load. The host keeps the plugins of each bundle
//! that loads in one file: a bundle with the same files as at its test is
//! not tested again, and its plugins have their names before the first
//! load. A bundle that does not load is not in the file, so the next start
//! tests the bundle again.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The file with the cache, in the folder of the preset folders.
pub(crate) const FILE_NAME: &str = "scan.json";
/// The form of the file. A file of a different form is not read.
const VERSION: u32 = 1;
/// A bundle with more files than this, or folders deeper than this, has a
/// stamp from the first files only.
const MAX_FILES: u32 = 4_096;
const MAX_DEPTH: usize = 6;

/// One plugin of a bundle, as the load test found the plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Scanned {
    pub name: String,
    pub vendor: String,
    /// For example "Fx|Reverb" or "Instrument|Synth".
    pub categories: String,
}

/// The files of a bundle at one time: the count, and a mark from the path,
/// the size and the change time of each file. A different stamp is a
/// different plugin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    files: u32,
    mark: u64,
}

impl Stamp {
    /// Mixes bytes into the mark: FNV-1a, the same on each system and in
    /// each version.
    fn mix(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.mark = (self.mark ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Reads the stamp of a bundle. A link counts as its target, so the stamp
/// of a bridge bundle changes with the plugin behind the bridge.
pub(crate) fn stamp(bundle: &Path) -> Stamp {
    fn add(path: &Path, depth: usize, stamp: &mut Stamp) {
        let Ok(data) = std::fs::metadata(path) else {
            return;
        };
        if data.is_dir() {
            let entries = std::fs::read_dir(path).into_iter().flatten().flatten();
            // The system gives the entries in no order.
            let mut paths: Vec<_> = entries.map(|entry| entry.path()).collect();
            paths.sort();
            for path in paths {
                if depth > 0 && stamp.files < MAX_FILES {
                    add(&path, depth - 1, stamp);
                }
            }
            return;
        }
        let changed = data.modified().ok().and_then(|time| {
            let since = time.duration_since(std::time::UNIX_EPOCH).ok()?;
            Some(since.as_nanos())
        });
        stamp.files += 1;
        stamp.mix(path.as_os_str().as_encoded_bytes());
        stamp.mix(&data.len().to_le_bytes());
        stamp.mix(&changed.unwrap_or(0).to_le_bytes());
    }
    let mut stamp = Stamp {
        files: 0,
        mark: 0xcbf2_9ce4_8422_2325,
    };
    add(bundle, MAX_DEPTH, &mut stamp);
    stamp
}

#[derive(Clone, Serialize, Deserialize)]
struct Tested {
    stamp: Stamp,
    plugins: Vec<Scanned>,
}

/// The plugins of the bundles that load, by the path of the bundle.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Cache {
    version: u32,
    bundles: BTreeMap<String, Tested>,
}

impl Cache {
    /// Reads the cache file. No file, a file that is not a cache, and a
    /// file of a different form give an empty cache.
    pub(crate) fn read(file: &Path) -> Self {
        let cache = std::fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(|cache| cache.version == VERSION);
        cache.unwrap_or_default()
    }

    /// Writes the cache file: a new file first, then the name change, so a
    /// reader never finds a half file.
    pub(crate) fn write(&self, file: &Path) {
        let cache = Self {
            version: VERSION,
            bundles: self.bundles.clone(),
        };
        let Ok(json) = serde_json::to_vec(&cache) else {
            return;
        };
        if let Some(folder) = file.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        // Two processes can write at one time: each has a file of its own.
        let new = file.with_extension(format!("{}.new", std::process::id()));
        if std::fs::write(&new, json).is_ok() {
            let _ = std::fs::rename(&new, file);
        }
    }

    /// The plugins of a bundle, if the bundle has the stamp of its test.
    pub(crate) fn plugins(&self, bundle: &Path, stamp: Stamp) -> Option<&[Scanned]> {
        let tested = self.bundles.get(bundle.to_str()?)?;
        (tested.stamp == stamp).then_some(tested.plugins.as_slice())
    }

    /// Stores the plugins of a bundle. False when the cache has the same
    /// ones already. A path that is not text has no place in the file: two
    /// such paths can have the same text form, so such a bundle gets a test
    /// at each start.
    pub(crate) fn store(&mut self, bundle: &Path, stamp: Stamp, plugins: &[Scanned]) -> bool {
        let Some(path) = bundle.to_str() else {
            return false;
        };
        if self.plugins(bundle, stamp) == Some(plugins) {
            return false;
        }
        let plugins = plugins.to_vec();
        self.bundles
            .insert(path.to_owned(), Tested { stamp, plugins });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugins_stay_for_a_bundle_with_the_same_files() {
        let root = std::env::temp_dir().join(format!("rustel-vst3-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundle = root.join("One.vst3");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents/plugin.so"), [0u8; 16]).unwrap();
        let first = stamp(&bundle);
        let plugins = vec![Scanned {
            name: "One \"Deluxe\"".into(),
            vendor: "Maker".into(),
            categories: "Fx|Delay".into(),
        }];

        let file = root.join("vst").join(FILE_NAME);
        let mut cache = Cache::read(&file);
        assert!(cache.store(&bundle, first, &plugins));
        assert!(!cache.store(&bundle, first, &plugins));
        cache.write(&file);
        let cache = Cache::read(&file);
        assert_eq!(cache.plugins(&bundle, first), Some(plugins.as_slice()));

        // A bundle with a changed file is a bundle with no test.
        std::fs::write(bundle.join("Contents/plugin.so"), [0u8; 17]).unwrap();
        let second = stamp(&bundle);
        assert_ne!(first, second);
        assert_eq!(cache.plugins(&bundle, second), None);
        std::fs::write(&file, "not a cache").unwrap();
        assert_eq!(Cache::read(&file).plugins(&bundle, first), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
