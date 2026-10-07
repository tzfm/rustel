//! Local filename addresses and file edits made through Studio.

use super::*;
use std::io::Read;

const ORDER_FILE: &str = ".rustel-sample-order.json";
const MAX_ORDER_BYTES: u64 = 64 * 1024;
type Order = BTreeMap<String, String>;
pub(super) type Names = HashMap<String, Option<Bank>>;

fn playable_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
}

fn reserved(stem: &str) -> bool {
    matches!(stem, "in" | "bus") || rustel_voice::is_native_synth_sound(stem)
}

/// Built on publication, never by walking files on the playback path. A
/// duplicate filename is deliberately ambiguous; its bank/index still works.
pub(super) fn index<'a>(banks: impl Iterator<Item = &'a Bank>) -> Names {
    let mut names: Names = HashMap::new();
    for bank in banks {
        for url in bank_file_urls(bank) {
            let Some(stem) = local_sample_file_stem(url) else {
                continue;
            };
            let stem = stem.to_lowercase();
            if !playable_stem(&stem) || reserved(&stem) {
                continue;
            }
            let single = Bank::Array(vec![Arc::clone(url)]);
            names
                .entry(stem)
                .and_modify(|found| {
                    if found.as_ref().is_some_and(|old| !same_file(old, &single)) {
                        *found = None;
                    }
                })
                .or_insert(Some(single));
        }
    }
    names
}

fn same_file(a: &Bank, b: &Bank) -> bool {
    matches!((a, b), (Bank::Array(a), Bank::Array(b)) if a == b)
}

pub(super) fn lookup<T>(
    shared: &Shared,
    name: &str,
    take: impl FnOnce(Named<'_>) -> T,
) -> Option<T> {
    let name = name.to_lowercase();
    let custom = shared.custom_file_names.read().expect("custom filenames");
    let global = shared.global_file_names.read().expect("global filenames");
    let bank = match (custom.get(&name), global.get(&name)) {
        (Some(Some(a)), Some(Some(b))) if same_file(a, b) => a,
        (Some(Some(a)), None) | (None, Some(Some(a))) => a,
        _ => return None,
    };
    Some(take(Named::Bank(bank)))
}

fn read_order(folder: &Path) -> Result<Order, String> {
    let path = folder.join(ORDER_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Order::new()),
        Err(error) => return Err(format!("cannot read sample order: {error}")),
    };
    if !meta.file_type().is_file() || meta.len() > MAX_ORDER_BYTES {
        return Err("invalid sample order file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_ORDER_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read sample order: {error}"))?;
    if bytes.len() as u64 > MAX_ORDER_BYTES {
        return Err("sample order is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid sample order: {error}"))
}

// Keep only one directory's metadata: a scan with many folders must not
// accumulate an order file for every directory. Sorting first also makes
// adjacent files normally share that one cached directory.
fn order_key(path: &Path, spelling: &str, cached: &mut Option<(PathBuf, Order)>) -> String {
    let Some(parent) = path.parent() else {
        return spelling.to_owned();
    };
    if cached.as_ref().is_none_or(|(folder, _)| folder != parent) {
        *cached = Some((parent.to_owned(), read_order(parent).unwrap_or_default()));
    }
    let order = &cached.as_ref().unwrap().1;
    let original = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| order.get(name));
    match original
        .filter(|name| Path::new(name).components().count() == 1 && !name.contains(['/', '\\']))
    {
        Some(original) => {
            // Compare the original string spelling, not Path components:
            // `foo.wav` precedes `foo/bar.wav` in the existing bank order.
            let filename = spelling.rfind(['/', '\\']).map_or(0, |at| at + 1);
            format!("{}{original}", &spelling[..filename])
        }
        None => spelling.to_owned(),
    }
}

pub(super) fn order_paths(root: &Path, paths: &mut [String]) {
    let mut cached = None;
    paths.sort_unstable();
    paths.sort_by_cached_key(|path| order_key(&root.join(path), path, &mut cached));
}

pub(super) fn order_urls(urls: &mut [Arc<str>]) {
    let mut cached = None;
    urls.sort_unstable();
    urls.sort_by_cached_key(|url| {
        let path = Path::new(url.strip_prefix("file://").unwrap_or(url));
        order_key(path, url, &mut cached)
    });
}

fn write_order(folder: &Path, order: &Order) -> Result<(), String> {
    let bytes = serde_json::to_vec(order).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_ORDER_BYTES {
        return Err("sample order is full".into());
    }
    let path = folder.join(ORDER_FILE);
    crate::atomic_file::replace_file(&path, ".rustel-order-", &bytes, |pending, target| {
        std::fs::rename(pending, target)
    })
    .map_err(|error| format!("cannot save sample order: {error}"))?;
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, SetFileAttributesW,
        };
        // Apply Hidden to the final metadata file once it is committed.
        if let Ok(meta) = std::fs::metadata(&path) {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: wide is a live, NUL-terminated path for this call.
            unsafe {
                SetFileAttributesW(
                    wide.as_ptr(),
                    (meta.file_attributes() & !FILE_ATTRIBUTE_NORMAL) | FILE_ATTRIBUTE_HIDDEN,
                );
            }
        }
    }
    Ok(())
}

impl SampleLibrary {
    /// The file behind one variant of a host-owned local array bank, the
    /// only files Studio renames or deletes. Callers keep this path so a
    /// refresh while a prompt is open cannot change its target.
    pub fn local_sample_path(&self, bank: &str, variant: usize) -> Result<PathBuf, String> {
        let custom = self.custom.read().expect("custom banks");
        let bank = if let Some(value) = custom.get(bank) {
            if !self
                .shared
                .set_banks
                .read()
                .expect("set banks")
                .contains_key(bank)
            {
                return Err("only imported local samples can be renamed or deleted".into());
            }
            value.clone()
        } else {
            self.global
                .read()
                .expect("global banks")
                .get(bank)
                .cloned()
                .ok_or_else(|| "sample is no longer available".to_owned())?
        };
        let Bank::Array(urls) = bank else {
            return Err("individual files in this bank cannot be renamed or deleted".into());
        };
        urls.get(variant)
            .and_then(|url| url.strip_prefix("file://"))
            .map(PathBuf::from)
            .ok_or_else(|| "only local sample files can be renamed or deleted".to_owned())
    }

    /// Whether `path` lies in this library's download cache. Local sample
    /// paths are canonical with the Windows verbatim prefix stripped, so the
    /// cache root is spelled the same way, even when it is a symlink.
    pub fn is_in_download_cache(&self, path: &Path) -> bool {
        let root = &self.shared.host_cache;
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        path.starts_with(&*without_verbatim_prefix(&root.display().to_string()))
    }

    /// The file behind one variant, if deletion may remove it: a regular
    /// file outside the download cache.
    pub fn local_sample_deletion_path(
        &self,
        bank: &str,
        variant: usize,
    ) -> Result<PathBuf, String> {
        let path = self.local_sample_path(bank, variant)?;
        if self.is_in_download_cache(&path) {
            return Err("downloaded pack samples cannot be deleted here".into());
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot read sample: {error}"))?;
        if !meta.file_type().is_file() {
            return Err("only regular sample files can be deleted".into());
        }
        Ok(path)
    }

    /// Delete the confirmed local file and remove every address for it.
    /// Already-playing audio keeps its decoded PCM until the engine retires
    /// it normally. A folder walk under way walks again, so a listing taken
    /// before the deletion cannot bring the file back.
    pub fn delete_local_sample(
        &self,
        bank: &str,
        variant: usize,
        expected: &Path,
    ) -> Result<PathBuf, String> {
        let path = self.local_sample_deletion_path(bank, variant)?;
        if path != expected {
            return Err("sample changed - reopen Delete".into());
        }
        std::fs::remove_file(&path).map_err(|error| format!("cannot delete sample: {error}"))?;
        // A later file of the same name must not inherit this one's place.
        // The file is already gone, so a failed write only leaves that entry.
        if let (Some(folder), Some(name)) = (
            path.parent(),
            path.file_name().and_then(|name| name.to_str()),
        ) && let Ok(mut order) = read_order(folder)
            && order.remove(name).is_some()
        {
            let _ = write_order(folder, &order);
        }

        let url = local_file_url(&path);
        {
            let mut custom = self.custom.write().expect("custom banks");
            let mut adopted = self.shared.set_banks.write().expect("set banks");
            // A score bank hidden by the set can name the same file. Do not
            // bring that missing file back when the set later closes.
            for displaced in adopted.values_mut() {
                if let Some(bank) = displaced
                    && !remove_url(bank, &url)
                {
                    *displaced = None;
                }
            }
            custom.retain(|name, bank| {
                if remove_url(bank, &url) {
                    return true;
                }
                // Removing a set bank's last file reveals the score bank
                // it had displaced, exactly as leaving that set would.
                if let Some(Some(displaced)) = adopted.remove(name) {
                    *bank = displaced;
                    true
                } else {
                    false
                }
            });
            *self
                .shared
                .custom_file_names
                .write()
                .expect("custom filenames") = index(custom.values());
        }
        {
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            for slot in slots.iter_mut() {
                slot.banks.retain(|_, bank| remove_url(bank, &url));
                match slot.state {
                    GlobalSourceState::Ready { .. } => {
                        slot.state = GlobalSourceState::Ready {
                            banks: slot.banks.len(),
                        };
                    }
                    GlobalSourceState::Loading if slot.kind == GlobalKind::Folder => {
                        slot.rewalk = true;
                    }
                    _ => {}
                }
            }
        }
        rebuild_global(&self.shared, &self.global);
        Ok(path)
    }

    pub fn rename_local_sample(
        &self,
        bank: &str,
        variant: usize,
        expected: &Path,
        stem: &str,
    ) -> Result<PathBuf, String> {
        let old = self.local_sample_path(bank, variant)?;
        if old != expected {
            return Err("sample changed - reopen Rename".into());
        }
        let stem = stem.trim();
        if !playable_stem(stem) {
            return Err("use letters, numbers, _ or -; leave out the extension".into());
        }
        let old_stem = old
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("invalid sample filename")?;
        if stem == old_stem {
            return Ok(old);
        }
        // A filename must not silently take over another sample, bank or synth.
        let folded = stem.to_lowercase();
        let taken = reserved(&folded)
            || self.knows(stem)
            || self
                .shared
                .custom_file_names
                .read()
                .expect("custom filenames")
                .contains_key(&folded)
            || self
                .shared
                .global_file_names
                .read()
                .expect("global filenames")
                .contains_key(&folded);
        if taken {
            return Err(format!("{stem} is already a sound name"));
        }
        let meta = std::fs::symlink_metadata(&old)
            .map_err(|error| format!("cannot read sample: {error}"))?;
        if !meta.file_type().is_file() {
            return Err("only regular sample files can be renamed".into());
        }
        let parent = old.parent().ok_or("sample has no folder")?;
        let extension = old
            .extension()
            .and_then(|ext| ext.to_str())
            .ok_or("sample has no extension")?;
        let new = parent.join(format!("{stem}.{extension}"));
        let original_order = read_order(parent)?;
        let mut order = original_order.clone();
        let old_name = old
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid sample filename")?;
        let sort_name = order
            .remove(old_name)
            .unwrap_or_else(|| old_name.to_owned());
        order.insert(
            new.file_name().unwrap().to_string_lossy().into_owned(),
            sort_name,
        );
        // Creating the new directory entry without replacing a destination is
        // atomic. The original bytes remain until the order is safely saved.
        std::fs::hard_link(&old, &new).map_err(|error| format!("cannot rename sample: {error}"))?;
        if let Err(error) = write_order(parent, &order) {
            let _ = std::fs::remove_file(&new);
            return Err(error);
        }
        if let Err(error) = std::fs::remove_file(&old) {
            let _ = std::fs::remove_file(&new);
            let _ = write_order(parent, &original_order);
            return Err(format!("cannot rename sample: {error}"));
        }
        let old_url = local_file_url(&old);
        let new_url: Arc<str> = Arc::from(local_file_url(&new));
        let replace = |bank: &mut Bank| match bank {
            Bank::Array(urls) => replace_urls(urls, &old_url, &new_url),
            Bank::Notes(notes) => {
                for (_, urls) in notes {
                    replace_urls(urls, &old_url, &new_url);
                }
            }
        };
        {
            let mut custom = self.custom.write().expect("custom banks");
            for value in custom.values_mut() {
                replace(value);
            }
            *self
                .shared
                .custom_file_names
                .write()
                .expect("custom filenames") = index(custom.values());
        }
        {
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            for slot in slots.iter_mut() {
                for value in slot.banks.values_mut() {
                    replace(value);
                }
            }
        }
        for displaced in self
            .shared
            .set_banks
            .write()
            .expect("set banks")
            .values_mut()
            .flatten()
        {
            replace(displaced);
        }
        rebuild_global(&self.shared, &self.global);
        Ok(new)
    }
}

fn replace_urls(urls: &mut [Arc<str>], old: &str, new: &Arc<str>) {
    for url in urls {
        if url.as_ref() == old {
            *url = Arc::clone(new);
        }
    }
}

/// Remove `deleted` from `bank`, dropping note keys it leaves empty.
/// Returns whether the bank still names a file.
fn remove_url(bank: &mut Bank, deleted: &str) -> bool {
    match bank {
        Bank::Array(urls) => {
            urls.retain(|url| url.as_ref() != deleted);
            !urls.is_empty()
        }
        Bank::Notes(notes) => {
            notes.retain_mut(|(_, urls)| {
                urls.retain(|url| url.as_ref() != deleted);
                !urls.is_empty()
            });
            !notes.is_empty()
        }
    }
}

#[cfg(test)]
mod delete_tests {
    //! Deleting a local sample file through the library.

    use super::tests::files;
    use super::*;

    #[test]
    fn deleting_a_local_variant_updates_addresses_without_reclaiming_playing_audio() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "takes", &["a.wav", "m.wav", "z.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 1).unwrap();
        let last = library.local_sample_path("takes", 2).unwrap();
        let url: Arc<str> = Arc::from(local_file_url(&path));
        library.shared.by_url.write().unwrap().insert(
            Arc::clone(&url),
            UrlState::Ready {
                id: SampleId(41),
                duration_secs: 1.0,
            },
        );

        assert_eq!(
            library.delete_local_sample("takes", 1, &path).unwrap(),
            path
        );
        assert!(!path.exists());
        assert!(last.exists());
        assert_eq!(library.variants_of("takes"), Some(2));
        assert_eq!(library.local_sample_path("takes", 1).unwrap(), last);
        assert!(!library.knows("m"));
        assert!(matches!(
            library.shared.by_url.read().unwrap().get(&url),
            Some(UrlState::Ready {
                id: SampleId(41),
                ..
            })
        ));
        // A second confirmation for the old index cannot delete its successor.
        assert!(library.delete_local_sample("takes", 1, &path).is_err());
        assert!(last.exists());
        library.adopt_set_folder(root.path()).unwrap();
        assert_eq!(library.variants_of("takes"), Some(2));
        assert_eq!(library.local_sample_path("takes", 1).unwrap(), last);
    }

    #[test]
    fn deleting_the_last_set_sample_restores_the_displaced_score_bank() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "takes", &["one.wav"]);
        let library = SampleLibrary::empty_without_loading();
        let score_url = "https://example.com/score.wav";
        library
            .custom
            .write()
            .unwrap()
            .insert("takes".into(), Bank::Array(vec![Arc::from(score_url)]));
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 0).unwrap();
        library.delete_local_sample("takes", 0, &path).unwrap();
        assert_eq!(
            library.file_location("takes", None).as_deref(),
            Some(score_url)
        );
        assert!(
            !library
                .shared
                .set_banks
                .read()
                .unwrap()
                .contains_key("takes")
        );
        assert!(!library.knows("one"));
        assert!(library.local_sample_path("takes", 0).is_err());
        let row = library
            .catalogue()
            .into_iter()
            .find(|row| row.name == "takes")
            .unwrap();
        assert_eq!(row.origin, SoundOrigin::Score);
        library.adopt_set_folder(root.path()).unwrap();
        assert_eq!(
            library.file_location("takes", None).as_deref(),
            Some(score_url)
        );
    }

    #[test]
    fn deleting_shared_set_and_global_file_reveals_the_default_bank_immediately() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["one.wav"]);
        let library = SampleLibrary::empty();
        let default_url = "https://example.com/default.wav";
        library
            .banks
            .write()
            .unwrap()
            .insert("takes".into(), Bank::Array(vec![Arc::from(default_url)]));
        let sources = [GlobalSource {
            spec: folder.display().to_string(),
            enabled: true,
        }];
        library.adopt_global_sources_settled(&sources);
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 0).unwrap();
        library.delete_local_sample("takes", 0, &path).unwrap();
        assert_eq!(
            library.file_location("takes", None).as_deref(),
            Some(default_url)
        );
        assert!(!library.knows("one"));
        assert!(
            library.shared.global_slots.lock().unwrap()[0]
                .banks
                .is_empty()
        );
        assert!(library.shared.set_banks.read().unwrap().is_empty());
        library.adopt_global_sources_settled(&sources);
        library.adopt_set_folder(root.path()).unwrap();
        assert_eq!(
            library.file_location("takes", None).as_deref(),
            Some(default_url)
        );
    }

    #[test]
    fn deleting_a_global_sample_resolves_filename_collisions_and_removes_empty_banks() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "first", &["same.wav"]);
        files(root.path(), "second", &["same.wav"]);
        let library = SampleLibrary::empty();
        let sources = [GlobalSource {
            spec: root.path().display().to_string(),
            enabled: true,
        }];
        library.adopt_global_sources_settled(&sources);
        assert!(!library.knows("same"));
        let first = library.local_sample_path("first", 0).unwrap();
        let second = library.local_sample_path("second", 0).unwrap();
        library.delete_local_sample("first", 0, &first).unwrap();
        assert!(!library.knows("first"));
        assert_eq!(
            library.file_location("same", None),
            Some(local_file_url(&second))
        );
        library.delete_local_sample("second", 0, &second).unwrap();
        assert!(!library.knows("second"));
        assert!(!library.knows("same"));
        assert!(
            library
                .catalogue()
                .iter()
                .all(|row| row.origin != SoundOrigin::Global)
        );
        library.adopt_global_sources_settled(&sources);
        assert!(!library.knows("first"));
        assert!(!library.knows("second"));
    }

    #[test]
    fn deletion_never_reveals_a_backup_address_for_the_missing_file() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["one.wav"]);
        let library = SampleLibrary::empty_without_loading();
        let url = local_file_url(&folder.canonicalize().unwrap().join("one.wav"));
        library.custom.write().unwrap().insert(
            "takes".into(),
            Bank::Notes(vec![(60.0, vec![Arc::from(url)])]),
        );
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 0).unwrap();
        library.delete_local_sample("takes", 0, &path).unwrap();
        assert!(!library.knows("takes"));
        library.adopt_set_folder(root.path()).unwrap();
        assert!(!library.knows("takes"));
    }

    #[test]
    fn deletion_rejects_score_imports_remote_samples_and_invalid_targets() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["one.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 0).unwrap();
        library.custom.write().unwrap().insert(
            "score".into(),
            Bank::Array(vec![Arc::from(local_file_url(&path))]),
        );
        library.global.write().unwrap().insert(
            "downloaded".into(),
            Bank::Array(vec![Arc::from("https://example.com/one.wav")]),
        );
        for (bank, variant, expected) in [
            ("score", 0, path.as_path()),
            ("downloaded", 0, path.as_path()),
            ("takes", 1, path.as_path()),
            ("takes", 0, folder.as_path()),
        ] {
            assert!(
                library
                    .delete_local_sample(bank, variant, expected)
                    .is_err()
            );
            assert!(path.is_file());
        }
        // Files that disappeared or became directories leave the catalogue intact.
        std::fs::remove_file(&path).unwrap();
        assert!(library.delete_local_sample("takes", 0, &path).is_err());
        std::fs::create_dir(&path).unwrap();
        assert!(library.delete_local_sample("takes", 0, &path).is_err());
        assert!(path.is_dir());
        assert_eq!(library.variants_of("takes"), Some(1));
    }

    fn assert_imported_cache_samples_cannot_be_deleted(cache_dir: PathBuf, import_dir: &Path) {
        let library = SampleLibrary::with_background_loaders_at(
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            String::new(),
            cache_dir,
            Loading::InBackground,
        )
        .unwrap();
        let sources = [GlobalSource {
            spec: import_dir.display().to_string(),
            enabled: true,
        }];
        library.adopt_global_sources_settled(&sources);
        let path = library.local_sample_path("downloaded", 0).unwrap();
        for set_imported in [false, true] {
            if set_imported {
                library.adopt_set_folder(import_dir).unwrap();
            }
            assert!(
                library
                    .local_sample_deletion_path("downloaded", 0)
                    .unwrap_err()
                    .contains("downloaded pack")
            );
            assert!(library.delete_local_sample("downloaded", 0, &path).is_err());
            assert!(path.is_file());
            assert_eq!(library.variants_of("downloaded"), Some(1));
        }
    }

    #[test]
    fn deletion_rejects_the_download_cache_imported_as_a_local_folder() {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("cache");
        files(&cache_dir, "downloaded", &["one.wav"]);
        assert_imported_cache_samples_cannot_be_deleted(cache_dir.clone(), &cache_dir);
    }

    #[cfg(unix)]
    #[test]
    fn deletion_rejects_an_imported_download_cache_with_a_symlinked_root() {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("cache");
        files(&cache_dir, "downloaded", &["one.wav"]);
        let cache_link = root.path().join("cache-link");
        std::os::unix::fs::symlink(&cache_dir, &cache_link).unwrap();
        assert_imported_cache_samples_cannot_be_deleted(cache_link, &cache_dir);
    }

    #[cfg(unix)]
    #[test]
    fn deletion_rejects_a_sample_replaced_with_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "takes", &["one.wav", "two.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 0).unwrap();
        let other = library.local_sample_path("takes", 1).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(library.delete_local_sample("takes", 0, &path).is_err());
        assert!(path.is_symlink());
        assert!(other.is_file());
        assert_eq!(library.variants_of("takes"), Some(2));
    }

    #[test]
    fn a_folder_walk_listed_before_a_deletion_walks_again_instead_of_landing() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "takes", &["a.wav", "b.wav"]);
        let library = SampleLibrary::empty_without_loading();
        let spec = root.path().display().to_string();
        library.adopt_global_sources_settled(&[GlobalSource {
            spec: spec.clone(),
            enabled: true,
        }]);
        let deleted = library.local_sample_path("takes", 0).unwrap();
        let kept = library.local_sample_path("takes", 1).unwrap();
        // A walk is under way and has listed the folder, but not yet landed.
        let (listed, state) = walk_global_folder(&spec);
        library.shared.global_slots.lock().unwrap()[0].state = GlobalSourceState::Loading;
        let generation = library.shared.global_generation.load(Ordering::Acquire);

        library.delete_local_sample("takes", 0, &deleted).unwrap();
        assert_eq!(
            fill_global_folder_slot(&library.shared, &spec, generation, listed, state),
            FolderFill::Stale
        );
        assert_eq!(library.variants_of("takes"), Some(1));
        library
            .run_folders_work_for_test(
                std::slice::from_ref(&spec),
                generation,
                &sample_fetch::FetchBudget::for_one_fetch(),
            )
            .unwrap();
        assert_eq!(
            library.global_source_reports()[0].state,
            GlobalSourceState::Ready { banks: 1 }
        );
        assert_eq!(library.local_sample_path("takes", 0).unwrap(), kept);
        assert_eq!(library.variants_of("takes"), Some(1));
    }

    #[test]
    fn deleting_a_renamed_sample_forgets_its_place_in_the_folder_order() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["a.wav", "m.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        let path = library.local_sample_path("takes", 0).unwrap();
        let renamed = library
            .rename_local_sample("takes", 0, &path, "vox1")
            .unwrap();
        assert!(read_order(&folder).unwrap().contains_key("vox1.wav"));

        library.delete_local_sample("takes", 0, &renamed).unwrap();
        assert!(read_order(&folder).unwrap().is_empty());
        // A new file under the deleted name sorts by its own name.
        std::fs::write(folder.join("vox1.wav"), b"new").unwrap();
        library.adopt_set_folder(root.path()).unwrap();
        assert_eq!(
            library
                .local_sample_path("takes", 1)
                .unwrap()
                .canonicalize()
                .unwrap(),
            folder.join("vox1.wav").canonicalize().unwrap()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_voice::SampleLookup;

    pub(super) fn files(root: &Path, bank: &str, names: &[&str]) -> PathBuf {
        let folder = root.join(bank);
        std::fs::create_dir_all(&folder).unwrap();
        for name in names {
            std::fs::write(folder.join(name), name.as_bytes()).unwrap();
        }
        folder
    }

    #[test]
    fn saved_rename_order_preserves_the_existing_nested_path_sort() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "foo", &["bar.wav"]);
        std::fs::write(root.path().join("foo.wav"), b"outer").unwrap();
        let mut paths = vec!["foo/bar.wav".to_owned(), "foo.wav".to_owned()];
        order_paths(root.path(), &mut paths);
        assert_eq!(paths, ["foo.wav", "foo/bar.wav"]);

        std::fs::rename(root.path().join("foo.wav"), root.path().join("vox1.wav")).unwrap();
        write_order(
            root.path(),
            &Order::from([("vox1.wav".into(), "foo.wav".into())]),
        )
        .unwrap();
        paths = vec!["foo/bar.wav".into(), "vox1.wav".into()];
        order_paths(root.path(), &mut paths);
        assert_eq!(paths, ["vox1.wav", "foo/bar.wav"]);
        let mut urls: Vec<Arc<str>> = paths
            .iter()
            .rev()
            .map(|path| Arc::from(local_file_url(&root.path().join(path))))
            .collect();
        order_urls(&mut urls);
        assert_eq!(
            urls[0].as_ref(),
            local_file_url(&root.path().join("vox1.wav"))
        );
    }

    #[test]
    fn renaming_a_local_file_preserves_its_index_and_both_playback_addresses() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["a.wav", "m.wav", "z.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        let old = library.local_sample_path("takes", 1).unwrap();
        let new = library
            .rename_local_sample("takes", 1, &old, "vox1")
            .unwrap();
        assert!(!old.exists());
        assert_eq!(std::fs::read(&new).unwrap(), b"m.wav");
        assert_eq!(new.extension().unwrap(), "wav");
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            assert_ne!(
                std::fs::metadata(folder.join(ORDER_FILE))
                    .unwrap()
                    .file_attributes()
                    & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN,
                0
            );
        }
        let url = local_file_url(&new);
        assert_eq!(library.file_location("takes", Some(1)), Some(url.clone()));
        assert_eq!(library.file_location("vox1", None), Some(url.clone()));
        assert!(library.knows_sound("vox1"));
        assert!(library.knows_sound("VOX1"));
        assert!(!library.knows("m"));
        assert_eq!(library.variants_of("vox1"), Some(1));
        library.shared.by_url.write().unwrap().insert(
            Arc::from(url.as_str()),
            UrlState::Ready {
                id: SampleId(41),
                duration_secs: 1.0,
            },
        );
        for (name, n) in [("takes", 1.0), ("vox1", 0.0)] {
            assert!(matches!(
                SampleLookup::resolve(&library, name, n, 36.0),
                SampleResolution::Found {
                    id: SampleId(41),
                    ..
                }
            ));
        }
        let rows: Vec<_> = library
            .catalogue()
            .into_iter()
            .filter(|row| row.origin == SoundOrigin::Set)
            .collect();
        assert_eq!(rows.len(), 1, "filename aliases are not extra bank rows");
        assert_eq!(rows[0].variant_label(1), "takes:1 (vox1)");

        // Disk refresh, a fresh process/library, and a second rename all
        // preserve the address even when alphabetical filename order changes.
        library.adopt_set_folder(root.path()).unwrap();
        assert_eq!(library.file_location("takes", Some(1)), Some(url));
        let reopened = SampleLibrary::empty_without_loading();
        reopened.adopt_set_folder(root.path()).unwrap();
        assert_eq!(reopened.local_sample_path("takes", 1).unwrap(), new);
        let renamed = reopened
            .rename_local_sample("takes", 1, &new, "aa")
            .unwrap();
        reopened.adopt_set_folder(root.path()).unwrap();
        assert_eq!(reopened.local_sample_path("takes", 1).unwrap(), renamed);
        let scanned = scan_sample_folder(&folder).unwrap();
        assert_eq!(scanned["takes"], ["a.wav", "aa.wav", "z.wav"]);
        reopened.set_bank_renames(HashMap::from([("takes".into(), "voices".into())]));
        reopened.adopt_set_folder(root.path()).unwrap();
        assert_eq!(
            reopened.file_location("voices", Some(1)),
            reopened.file_location("aa", None)
        );
    }

    #[test]
    fn filename_collisions_never_replace_a_bank_synth_or_another_file() {
        let root = tempfile::tempdir().unwrap();
        files(root.path(), "takes", &["one.wav", "two.wav"]);
        files(root.path(), "other", &["two.wav", "sine.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        assert!(
            !library.knows("two"),
            "ambiguous stems require a bank/index"
        );
        assert!(!library.knows("sine"), "a filename cannot shadow a synth");
        let old = library.local_sample_path("takes", 0).unwrap();
        for stem in ["two", "sine", "takes", "../escape", "my voice", "bad*4"] {
            assert!(
                library.rename_local_sample("takes", 0, &old, stem).is_err(),
                "{stem}"
            );
            assert_eq!(std::fs::read(&old).unwrap(), b"one.wav");
        }
        let occupied = old.parent().unwrap().join("occupied.wav");
        std::fs::write(&occupied, b"created since the scan").unwrap();
        assert!(
            library
                .rename_local_sample("takes", 0, &old, "occupied")
                .is_err()
        );
        assert_eq!(std::fs::read(occupied).unwrap(), b"created since the scan");
        assert!(old.exists());
        assert!(!old.parent().unwrap().join(ORDER_FILE).exists());
        assert!(
            library
                .rename_local_sample("takes", 0, &old.with_file_name("two.wav"), "vox1")
                .is_err()
        );
    }

    #[test]
    fn filename_addresses_refresh_and_disappear_with_their_sources() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["first.wav", "last.wav"]);
        let library = SampleLibrary::empty();
        let sources = [GlobalSource {
            spec: folder.display().to_string(),
            enabled: true,
        }];
        library.adopt_global_sources_settled(&sources);
        assert!(library.knows("first"));
        let old = library.local_sample_path("takes", 0).unwrap();
        let new = library
            .rename_local_sample("takes", 0, &old, "vox1")
            .unwrap();
        library.adopt_global_sources_settled(&sources);
        assert_eq!(library.local_sample_path("takes", 0).unwrap(), new);
        assert!(library.knows("vox1"));
        assert!(!library.knows("first"));
        let fresh = SampleLibrary::empty();
        fresh.adopt_global_sources_settled(&sources);
        assert_eq!(
            fresh.file_location("takes", Some(0)),
            fresh.file_location("vox1", None)
        );
        assert_eq!(
            fresh
                .catalogue()
                .iter()
                .filter(|row| row.origin == SoundOrigin::Global)
                .count(),
            1
        );
        fresh.adopt_global_sources_settled(&[]);
        assert!(!fresh.knows("vox1"));

        // Unloading a set or replacing its bank must also remove its names.
        library.adopt_set_folder(root.path()).unwrap();
        library.adopt_global_sources_settled(&[]);
        assert!(library.knows("vox1"));
        let empty = tempfile::tempdir().unwrap();
        library.adopt_set_folder(empty.path()).unwrap();
        assert!(!library.knows("vox1"));
    }

    #[test]
    fn a_failed_order_write_leaves_the_original_sample_untouched() {
        let root = tempfile::tempdir().unwrap();
        let folder = files(root.path(), "takes", &["take.wav"]);
        let library = SampleLibrary::empty_without_loading();
        library.adopt_set_folder(root.path()).unwrap();
        let old = library.local_sample_path("takes", 0).unwrap();
        std::fs::write(folder.join(ORDER_FILE), b"not valid metadata").unwrap();
        assert!(
            library
                .rename_local_sample("takes", 0, &old, "vox1")
                .is_err()
        );
        assert_eq!(std::fs::read(&old).unwrap(), b"take.wav");
        assert!(!folder.join("vox1.wav").exists());
    }
}
