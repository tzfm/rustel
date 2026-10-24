//! The scan cache: what the load test found in each bundle.
//!
//! The load test starts a process and loads the plugin there, so a test
//! takes about as long as a load. The host keeps the answer for each bundle
//! in one file: a bundle with the same files as at its test is not tested
//! again, and its plugins have their names before the first load. The
//! answer is the plugins of a bundle, with the fault of each plugin that
//! does not load, or the fault of a bundle with no plugin that loads. So
//! the next start tests no unchanged bundle a second time.

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
    /// The class id as a `.vstpreset` file writes it, so a preset file
    /// finds its plugin. A file of an older version has no such field.
    #[serde(default)]
    pub class: String,
    /// The reason the plugin did not load at its test, while other plugins
    /// of its bundle did. A file of an older version has no such field.
    #[serde(default)]
    pub fault: Option<String>,
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
    /// The reason the bundle did not load at its test. A file of an older
    /// version has no such field, and a reader of an older version drops
    /// the field of a new file, so the form of the file stays.
    #[serde(default)]
    fault: Option<String>,
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
    /// reader never finds a half file. The file's own entries come in
    /// first: another host can have written since this one read the file,
    /// and its bundles must not vanish behind this write. Where both have
    /// an answer for one bundle, this cache's own wins.
    pub(crate) fn write(&self, file: &Path) {
        let mut merged = Self::read(file);
        for (path, tested) in &self.bundles {
            merged.bundles.insert(path.clone(), tested.clone());
        }
        let cache = Self {
            version: VERSION,
            bundles: merged.bundles,
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

    /// The plugins of a bundle, if the bundle has the stamp of its test and
    /// the test found plugins.
    pub(crate) fn plugins(&self, bundle: &Path, stamp: Stamp) -> Option<&[Scanned]> {
        let tested = self.bundles.get(bundle.to_str()?)?;
        (tested.stamp == stamp && tested.fault.is_none()).then_some(tested.plugins.as_slice())
    }

    /// The reason a bundle did not load at its test, if the bundle has the
    /// stamp of that test.
    pub(crate) fn fault(&self, bundle: &Path, stamp: Stamp) -> Option<&str> {
        let tested = self.bundles.get(bundle.to_str()?)?;
        (tested.stamp == stamp)
            .then_some(tested.fault.as_deref())
            .flatten()
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
        self.bundles.insert(
            path.to_owned(),
            Tested {
                stamp,
                plugins,
                fault: None,
            },
        );
        true
    }

    /// Stores the fault of a bundle that did not load at its test. False
    /// when the cache has an answer for the bundle at this stamp already: a
    /// fault that comes and goes does not remove the plugins of a bundle,
    /// and the same fault needs no new line. A path that is not text has no
    /// place in the file, as in [`Cache::store`].
    pub(crate) fn store_fault(&mut self, bundle: &Path, stamp: Stamp, reason: &str) -> bool {
        let Some(path) = bundle.to_str() else {
            return false;
        };
        if self
            .bundles
            .get(path)
            .is_some_and(|tested| tested.stamp == stamp)
        {
            return false;
        }
        self.bundles.insert(
            path.to_owned(),
            Tested {
                stamp,
                plugins: Vec::new(),
                fault: Some(reason.to_owned()),
            },
        );
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
        let plugins = vec![
            Scanned {
                name: "One \"Deluxe\"".into(),
                vendor: "Maker".into(),
                categories: "Fx|Delay".into(),
                class: String::new(),
                fault: None,
            },
            Scanned {
                name: "One Synth".into(),
                vendor: "Maker".into(),
                categories: "Instrument|Synth".into(),
                class: String::new(),
                fault: Some("the plugin processor did not start".into()),
            },
        ];

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

    #[test]
    fn a_fault_stays_for_a_bundle_with_the_same_files() {
        let root = std::env::temp_dir().join(format!("rustel-vst3-fault-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let bundle = root.join("Bad.vst3");
        std::fs::write(&bundle, [0u8; 16]).unwrap();
        let first = stamp(&bundle);

        let file = root.join("vst").join(FILE_NAME);
        let mut cache = Cache::read(&file);
        assert!(cache.store_fault(&bundle, first, "no audio plugin"));
        assert!(!cache.store_fault(&bundle, first, "no audio plugin"));
        assert_eq!(cache.plugins(&bundle, first), None);
        cache.write(&file);
        let mut cache = Cache::read(&file);
        assert_eq!(cache.fault(&bundle, first), Some("no audio plugin"));
        assert_eq!(cache.plugins(&bundle, first), None);

        // Plugins that load after all win over the fault of the same files,
        // and the fault then does not remove them again.
        let plugins = vec![Scanned {
            name: "Bad".into(),
            vendor: "Maker".into(),
            categories: "Fx".into(),
            class: String::new(),
            fault: None,
        }];
        assert!(cache.store(&bundle, first, &plugins));
        assert_eq!(cache.plugins(&bundle, first), Some(plugins.as_slice()));
        assert_eq!(cache.fault(&bundle, first), None);
        assert!(!cache.store_fault(&bundle, first, "no audio plugin"));
        assert_eq!(cache.plugins(&bundle, first), Some(plugins.as_slice()));

        // A bundle with a changed file is a bundle with no answer.
        std::fs::write(&bundle, [0u8; 17]).unwrap();
        let second = stamp(&bundle);
        assert_ne!(first, second);
        assert_eq!(cache.fault(&bundle, second), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_later_writer_keeps_the_entries_of_an_earlier_one() {
        let root =
            std::env::temp_dir().join(format!("rustel-vst3-cache-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let one = root.join("One.vst3");
        let two = root.join("Two.vst3");
        std::fs::write(&one, [0u8; 16]).unwrap();
        std::fs::write(&two, [0u8; 16]).unwrap();
        let file = root.join(FILE_NAME);

        // Two hosts that each read an empty file before either writes: the
        // second write must not erase the first's answer.
        let mut first = Cache::read(&file);
        let mut second = Cache::read(&file);
        assert!(first.store(
            &one,
            stamp(&one),
            &[Scanned {
                name: "One".into(),
                vendor: "Maker".into(),
                categories: "Fx".into(),
                class: String::new(),
                fault: None,
            }]
        ));
        assert!(second.store_fault(&two, stamp(&two), "no audio plugin"));
        first.write(&file);
        second.write(&file);

        let merged = Cache::read(&file);
        assert!(
            merged.plugins(&one, stamp(&one)).is_some(),
            "bundle one is kept"
        );
        assert_eq!(
            merged.fault(&two, stamp(&two)),
            Some("no audio plugin"),
            "bundle two is kept"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
