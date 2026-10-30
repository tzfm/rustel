use super::*;

/// A pack's base as a prefix its files start with: with the slash, so
/// `…/piano` does not claim `…/piano-two/`.
pub(super) fn source_prefix(base: &str) -> std::borrow::Cow<'_, str> {
    if base.ends_with('/') {
        std::borrow::Cow::Borrowed(base)
    } else {
        std::borrow::Cow::Owned(format!("{base}/"))
    }
}

pub(super) fn default_categories() -> &'static DefaultCategories {
    static CATEGORIES: std::sync::OnceLock<DefaultCategories> = std::sync::OnceLock::new();
    CATEGORIES.get_or_init(|| {
        let Ok(pinned) = serde_json::from_str::<PinnedFile>(PINNED_BANKS) else {
            return Vec::new();
        };
        let mut declared: Vec<(String, SoundCategory)> = pinned
            .sources
            .iter()
            .filter_map(|source| {
                let base = source.base.as_deref()?;
                let category = source.category?;
                Some((source_prefix(base).to_lowercase(), category))
            })
            .chain(
                pinned
                    .inline
                    .category
                    .map(|category| (source_prefix(&pinned.inline.base).to_lowercase(), category)),
            )
            .collect();
        declared.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| b.0.cmp(&a.0)));
        declared
    })
}

/// The shipped packs a pin file and a font map describe. Only the packs
/// that bring files: an alias-only pin has no row's worth of anything.
pub(super) fn default_sources_from(pinned_banks: &str, pinned_fonts: &str) -> Vec<DefaultSource> {
    let mut sources = Vec::new();
    if let Ok(pinned) = serde_json::from_str::<PinnedFile>(pinned_banks) {
        for source in pinned.sources {
            if let Some(base) = source.base {
                sources.push(DefaultSource {
                    name: source.name,
                    url: source.url,
                    base,
                });
            }
        }
        if !pinned.inline.banks.is_empty() {
            let base = pinned.inline.base;
            sources.push(DefaultSource {
                name: base
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("built-in")
                    .to_owned(),
                url: base.clone(),
                base,
            });
        }
    }
    if let Ok(fonts) = serde_json::from_str::<PinnedFonts>(pinned_fonts)
        && !fonts.fonts.is_empty()
    {
        sources.push(DefaultSource {
            name: "gm soundfonts".to_owned(),
            url: fonts.base.clone(),
            base: fonts.base,
        });
    }
    sources
}

/// Banks baked into the pin file - Dirt-Samples and friends - not fetched
/// from a remote manifest.
pub(super) fn inline_banks_from(pinned: &PinnedFile) -> HashMap<String, Bank> {
    pinned
        .inline
        .banks
        .iter()
        .map(|(name, files)| {
            let urls = files
                .iter()
                .map(|file| Arc::from(join_url(&pinned.inline.base, file)))
                .collect();
            (name.clone(), Bank::Array(urls))
        })
        .collect()
}

/// What the pin file says an inline pack holds, when its banks are not
/// in the live map yet - after a refresh cleared them, or before the
/// first poll.
fn pinned_inline_files(source: &DefaultSource) -> Option<(usize, HashSet<Arc<str>>)> {
    let pinned: PinnedFile = serde_json::from_str(PINNED_BANKS).ok()?;
    if pinned.inline.banks.is_empty()
        || source_prefix(&source.base) != source_prefix(&pinned.inline.base)
    {
        return None;
    }
    let mut files = HashSet::new();
    for file_list in pinned.inline.banks.values() {
        for file in file_list {
            files.insert(Arc::from(join_url(&pinned.inline.base, file)));
        }
    }
    Some((pinned.inline.banks.len(), files))
}

impl SoundEntry {
    pub fn variant_label(&self, variant: usize) -> String {
        let indexed = format!("{}:{variant}", self.name);
        self.variant_names
            .get(variant)
            .filter(|name| !name.is_empty())
            .map_or(indexed.clone(), |name| format!("{indexed} ({name})"))
    }
}

pub(super) fn local_sample_file_stem(location: &str) -> Option<String> {
    let path = location.strip_prefix("file://")?;
    std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
}

pub(super) fn local_variant_names(bank: &Bank) -> Vec<String> {
    let names: Vec<String> = match bank {
        Bank::Array(urls) => urls
            .iter()
            .map(|url| local_sample_file_stem(url).unwrap_or_default())
            .collect(),
        Bank::Notes(notes) => {
            let variants = notes.iter().map(|(_, urls)| urls.len()).max().unwrap_or(0);
            (0..variants)
                .map(|index| {
                    notes
                        .iter()
                        .find_map(|(_, urls)| urls.get(index))
                        .and_then(|url| local_sample_file_stem(url))
                        .unwrap_or_default()
                })
                .collect()
        }
    };
    if names.iter().any(|name| !name.is_empty()) {
        names
    } else {
        Vec::new()
    }
}

/// Whether a Settings source is a folder already on this machine, so
/// there is nothing to download: a path, or a `local:` folder.
pub fn source_is_local(spec: &str) -> bool {
    folder_of_spec(spec).is_some()
}

/// Keep the least-specific enabled local folders. An enabled parent owns all
/// banks below it, so retaining a child would only scan and list the same
/// files twice. Remote sources and disabled roots that no enabled parent
/// owns are preserved in their order.
pub fn deduplicated_global_sources(sources: &[GlobalSource]) -> Vec<GlobalSource> {
    deduplicated_global_sources_with_owners(sources).0
}

/// Deduplicate sources and report each removed source's surviving owner.
/// Preferences use the ownership pairs to move source-scoped bank aliases
/// before the redundant child row disappears.
pub fn deduplicated_global_sources_with_owners(
    sources: &[GlobalSource],
) -> (Vec<GlobalSource>, Vec<(String, String)>) {
    let local_paths: Vec<(usize, String, bool)> = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let folder = folder_of_spec(&source.spec)?;
            let path = Path::new(&folder).canonicalize().ok()?;
            // Not folded to one case: on a filesystem that folds, the
            // canonical form already spells the folder the one way it is
            // written, and on one that does not, `Kit` and `kit` are two
            // folders whose sounds must both survive.
            let key = path
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_owned();
            Some((index, key, source.enabled))
        })
        .collect();
    let mut enabled: Vec<(usize, String)> = local_paths
        .iter()
        .filter(|(_, _, enabled)| *enabled)
        .map(|(index, path, _)| (*index, path.clone()))
        .collect();
    enabled.sort_by_key(|(_, path)| (path.len(), path.clone()));
    let mut accepted: Vec<(usize, String)> = Vec::new();
    for (index, path) in enabled {
        let is_covered = accepted.iter().any(|(_, parent)| {
            path == *parent
                || path
                    .strip_prefix(parent)
                    .is_some_and(|rest| rest.starts_with(['/', '\\']))
        });
        if !is_covered {
            accepted.push((index, path));
        }
    }
    let owners: HashMap<usize, usize> = local_paths
        .iter()
        .filter_map(|(index, path, _)| {
            accepted
                .iter()
                .find(|(parent_index, parent)| {
                    index != parent_index
                        && (path == parent
                            || path
                                .strip_prefix(parent)
                                .is_some_and(|rest| rest.starts_with(['/', '\\'])))
                })
                .map(|(parent_index, _)| (*index, *parent_index))
        })
        .collect();
    let kept = sources
        .iter()
        .enumerate()
        .filter(|(index, _)| !owners.contains_key(index))
        .map(|(_, source)| source.clone())
        .collect();
    let mut ownership_indices: Vec<(usize, usize)> = owners.into_iter().collect();
    ownership_indices.sort_by_key(|(child, _)| *child);
    let ownership = ownership_indices
        .into_iter()
        .filter_map(|(child, parent)| {
            let child = sources.get(child)?.spec.clone();
            let parent = sources.get(parent)?.spec.clone();
            (child != parent).then_some((child, parent))
        })
        .collect();
    (kept, ownership)
}

/// The distinct file URLs a bank names.
pub(super) fn bank_file_urls(bank: &Bank) -> Vec<&Arc<str>> {
    match bank {
        Bank::Array(urls) => urls.iter().collect(),
        Bank::Notes(notes) => notes.iter().flat_map(|(_, urls)| urls.iter()).collect(),
    }
}

fn source_sort_key(slot: &GlobalSlot) -> String {
    folder_of_spec(&slot.spec)
        .and_then(|folder| Path::new(&folder).canonicalize().ok())
        .map(|path| path.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| slot.spec.to_ascii_lowercase())
}

/// Rebuild the flat imported layer from the rows. Banks with the same name
/// get stable source-scoped aliases rather than silently replacing one another.
pub(super) fn rebuild_global(shared: &Shared, global: &RwLock<HashMap<String, Bank>>) {
    let slots = shared.global_slots.lock().expect("global source slots");
    let renames = shared.renames.read().expect("bank renames");
    let source_renames = shared.source_renames.read().expect("source bank renames");
    let mut auto_aliases = shared.auto_aliases.write().expect("automatic bank aliases");
    let mut flat: HashMap<String, Bank> = HashMap::new();
    let mut source_of: HashMap<String, Arc<str>> = HashMap::new();
    let mut rows: Vec<&GlobalSlot> = slots.iter().collect();
    rows.sort_by_key(|slot| (source_sort_key(slot), slot.row));
    for slot in rows {
        // Off and unreadable rows bring nothing; a pack still fetching keeps
        // what it brought last time.
        if matches!(
            slot.state,
            GlobalSourceState::Off | GlobalSourceState::Missing(_) | GlobalSourceState::Failed(_)
        ) && slot.banks.is_empty()
        {
            continue;
        }
        if matches!(slot.state, GlobalSourceState::Off) {
            continue;
        }
        let mut banks: Vec<_> = slot.banks.iter().collect();
        banks.sort_by_key(|(name, _)| *name);
        for (original, bank) in banks {
            let identity = (slot.spec.clone(), original.clone());
            let manual = source_renames
                .get(&identity)
                .or_else(|| renames.get(original));
            let requested = manual.cloned().unwrap_or_else(|| original.clone());
            let remembered = manual
                .is_none()
                .then(|| auto_aliases.get(&identity).cloned())
                .flatten();
            let name = if remembered
                .as_ref()
                .is_some_and(|name| !flat.contains_key(name))
            {
                remembered.expect("checked above")
            } else if !flat.contains_key(&requested) {
                requested.clone()
            } else {
                let mut suffix = 1usize;
                loop {
                    let candidate = format!("{requested}_{suffix}");
                    if !flat.contains_key(&candidate)
                        && !auto_aliases.values().any(|alias| alias == &candidate)
                    {
                        break candidate;
                    }
                    suffix += 1;
                }
            };
            if name != requested {
                auto_aliases.insert(identity, name.clone());
            }
            source_of.insert(name.clone(), Arc::from(slot.spec.as_str()));
            flat.insert(name, bank.clone());
        }
    }
    drop(auto_aliases);
    drop(source_renames);
    drop(renames);
    drop(slots);
    // One write each, so the browser never sees a moment with the old
    // sources gone and the new ones not yet in.
    let mut global = global.write().expect("global banks");
    *shared.global_file_names.write().expect("global filenames") =
        local_names::index(flat.values());
    *global = flat;
    drop(global);
    *shared.global_source_of.write().expect("global sources") = source_of;
}

/// A pack's list has landed: fill its row, unless the row has been
/// replaced since the fetch was asked for.
pub(super) fn fill_global_slot(
    shared: &Shared,
    spec: Option<&str>,
    generation: u64,
    banks: Vec<(String, Bank)>,
) -> bool {
    if shared.global_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    let Some(spec) = spec else {
        return false;
    };
    let mut slots = shared.global_slots.lock().expect("global source slots");
    let Some(slot) = slots
        .iter_mut()
        .find(|slot| slot.kind == GlobalKind::Pack && slot.spec == spec)
    else {
        return false;
    };
    slot.state = if banks.is_empty() {
        GlobalSourceState::Failed("brought no sounds".to_owned())
    } else {
        GlobalSourceState::Ready { banks: banks.len() }
    };
    slot.banks = banks.into_iter().collect();
    true
}

/// A folder's walk has finished: fill its row with what it holds and how
/// it stands, unless the row has been replaced since the walk was asked
/// for or a file was deleted from it meanwhile.
pub(super) fn fill_global_folder_slot(
    shared: &Shared,
    spec: &str,
    generation: u64,
    banks: Vec<(String, Bank)>,
    state: GlobalSourceState,
) -> FolderFill {
    if shared.global_generation.load(Ordering::Acquire) != generation {
        return FolderFill::Superseded;
    }
    let mut slots = shared.global_slots.lock().expect("global source slots");
    let Some(slot) = slots
        .iter_mut()
        .find(|slot| slot.kind == GlobalKind::Folder && slot.spec == spec)
    else {
        return FolderFill::Superseded;
    };
    if std::mem::take(&mut slot.rewalk) {
        return FolderFill::Stale;
    }
    slot.state = state;
    slot.banks = banks.into_iter().collect();
    FolderFill::Filled
}

/// What one imported folder holds, and how its row should read: missing
/// when the folder is not there, failed when it is there and will not
/// read.
pub(super) fn walk_global_folder(folder: &str) -> (Vec<(String, Bank)>, GlobalSourceState) {
    let root = Path::new(folder.trim());
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            let why = format!("cannot read {}: {error}", root.display());
            let state = if error.kind() == std::io::ErrorKind::NotFound {
                GlobalSourceState::Missing(why)
            } else {
                GlobalSourceState::Failed(why)
            };
            return (Vec::new(), state);
        }
    };
    match folder_banks(&root, &[]) {
        Ok(banks) => {
            let count = banks.len();
            (banks, GlobalSourceState::Ready { banks: count })
        }
        Err(error) => (Vec::new(), GlobalSourceState::Failed(error)),
    }
}

/// Whether a source spelling is a folder on this machine: a bare absolute
/// path, or the `local:` spelling a score would use. A drop and the
/// picker hand over bare paths; nothing tells anyone to type `local:`.
pub fn folder_of_spec(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if Path::new(spec).is_absolute() {
        return Some(spec.to_owned());
    }
    match read_source(spec, "") {
        SampleSource::LocalFolder(folder) => Some(folder),
        SampleSource::Url(_) => None,
    }
}

impl SampleLibrary {
    /// Take the open set's own folder up as sample banks.
    ///
    /// A set is a folder of scores, and audio a player drops beside them is
    /// theirs: it belongs in the browser and in `s(...)` with nothing
    /// written and no flag passed. `samples('local:...')` asks a host grant
    /// to do exactly this, which is why the grant is the studio's to give
    /// rather than the score's to ask for.
    ///
    /// A file in a subfolder is a variant of that folder's bank, so
    /// `kicks/1.wav` and `kicks/2.wav` are `s("kicks")` and `s("kicks:1")`.
    /// Files directly inside the set folder form one bank named after that
    /// folder (see `folder_banks`). A local file also has a filename
    /// address when `local_names::index` accepts its stem, so
    /// `deep_bass.wav` is `s("deep_bass")`.
    ///
    /// `sessions/` and `exports/` are skipped: those are the set's takes,
    /// its tapes and its bounces, and last night's recording is not an
    /// instrument.
    ///
    /// A bank takes the name it is given, so a set holding a `bd` folder plays
    /// its own `bd` - the precedence a score's `samples()` already has over
    /// the pinned banks. The browser files it under `set`, so which one is
    /// sounding is on screen rather than a surprise. Answering again
    /// replaces the last set's banks: they belong to that set, and leaving
    /// them behind would let one set's `kicks` play in another. What each of
    /// them displaced goes back on the way out, so a `bd` this set took over
    /// from a score's import is the score's again at the next one.
    pub fn adopt_set_folder(&self, folder: &Path) -> Result<usize, String> {
        self.adopt_set_folder_inner(folder, false)
    }

    /// Re-read the open set's folder, so the set holds what is on disk now.
    /// Unlike switching sets, a refresh that fails leaves the current banks
    /// in place, and a name a score or prebake redefined stays theirs.
    pub fn refresh_set_folder(&self, folder: &Path) -> Result<usize, String> {
        self.adopt_set_folder_inner(folder, true)
    }

    /// The open set's folder, canonical, as the last set change read it.
    pub fn set_folder(&self) -> Option<PathBuf> {
        self.shared.set_folder.read().expect("set folder").clone()
    }

    fn adopt_set_folder_inner(&self, folder: &Path, refresh: bool) -> Result<usize, String> {
        // Read before either lock is taken: the folder is walked on the
        // thread that opened the set, and the browser reads these two at
        // the other end of them.
        let root = folder
            .canonicalize()
            .map_err(|error| format!("set samples: cannot read {}: {error}", folder.display()));
        let staged: Result<Vec<(String, Bank)>, String> = root
            .clone()
            .and_then(|root| set_folder_banks(&root))
            .map(|banks| {
                banks
                    .into_iter()
                    .map(|(name, bank)| (self.renamed(name), bank))
                    .collect()
            });
        if refresh && let Err(error) = &staged {
            return Err(error.clone());
        }
        let mut custom = self.custom.write().expect("custom banks");
        let mut adopted = self.shared.set_banks.write().expect("set banks");
        // A refresh leaves alone a name the session holds outside this set:
        // a score or prebake registered it, and only opening a set takes a
        // name over (see `publish_score_custom`).
        let staged = staged.map(|banks| {
            banks
                .into_iter()
                .filter(|(name, _)| {
                    !refresh || adopted.contains_key(name) || !custom.contains_key(name)
                })
                .collect::<Vec<_>>()
        });
        let limit = SAMPLE_BANK_CAPACITY - 1;
        // The session's size once this set's banks are put down and the
        // folder's are taken up, counted the way `publish_score_custom`
        // counts it: a name already held is replaced rather than added, and
        // the session's total is what the limit is on - not anything the
        // set's folder is responsible for. It is counted before either map
        // changes, so a refresh that does not fit changes nothing.
        let full = staged
            .as_ref()
            .is_ok_and(|banks| {
                let held = |name: &str| {
                    custom.contains_key(name) && !matches!(adopted.get(name), Some(None))
                };
                let kept = custom.keys().filter(|name| held(name)).count();
                let additional = banks.iter().filter(|(name, _)| !held(name)).count();
                kept.saturating_add(additional) > limit
            })
            .then(|| format!("the session holds more than the {limit}-bank limit"));
        if refresh && let Some(error) = full {
            return Err(error);
        }
        // Switching sets closes the previous one even when the new folder
        // does not read or does not fit: one set's `kicks` never plays in
        // the next.
        for (name, displaced) in std::mem::take(&mut *adopted) {
            match displaced {
                Some(bank) => {
                    custom.insert(name, bank);
                }
                None => {
                    custom.remove(&name);
                }
            }
        }
        *self
            .shared
            .custom_file_names
            .write()
            .expect("custom filenames") = local_names::index(custom.values());
        // The folder belongs to the open set even when its samples do not
        // fit, so its images and videos stay readable.
        *self.shared.set_folder.write().expect("set folder") = root.ok();
        let staged = staged?;
        if let Some(error) = full {
            return Err(error);
        }
        for (name, bank) in staged {
            adopted.insert(name.clone(), custom.insert(name, bank));
        }
        *self
            .shared
            .custom_file_names
            .write()
            .expect("custom filenames") = local_names::index(custom.values());
        Ok(adopted.len())
    }

    /// Rename imported banks, by the name they arrived under.
    ///
    /// Only the imported layers answer to this - the set's own folder and
    /// the sources in Settings. The pinned banks are shared vocabulary: a
    /// score that says `bd` means the same thing on anybody's machine, and
    /// a local rename of one would quietly stop being true the moment the
    /// set was handed over.
    pub fn set_bank_renames(&self, renames: HashMap<String, String>) {
        *self.shared.renames.write().expect("bank renames") = renames;
        // The imported layer answers to the overlay too, and its rows are
        // still here to be read through it again.
        rebuild_global(&self.shared, &self.global);
    }

    /// Apply aliases using the source plus original name as the identity.
    pub fn set_source_bank_renames(&self, renames: HashMap<(String, String), String>) {
        *self
            .shared
            .source_renames
            .write()
            .expect("source bank renames") = renames;
        rebuild_global(&self.shared, &self.global);
    }

    pub fn alias_for_import(&self, spec: &str, original: &str) -> String {
        self.shared
            .source_renames
            .read()
            .expect("source bank renames")
            .get(&(spec.to_owned(), original.to_owned()))
            .cloned()
            .or_else(|| {
                self.shared
                    .auto_aliases
                    .read()
                    .expect("automatic bank aliases")
                    .get(&(spec.to_owned(), original.to_owned()))
                    .cloned()
            })
            .or_else(|| {
                self.shared
                    .renames
                    .read()
                    .expect("bank renames")
                    .get(original)
                    .cloned()
            })
            .unwrap_or_else(|| original.to_owned())
    }

    pub fn original_for_import_alias(&self, spec: &str, alias: &str) -> Option<String> {
        self.banks_for_import(spec)
            .into_iter()
            .find(|original| self.alias_for_import(spec, original) == alias)
    }

    /// The name an imported bank goes by, after the player's renames.
    pub(super) fn renamed(&self, name: String) -> String {
        self.shared
            .renames
            .read()
            .expect("bank renames")
            .get(&name)
            .cloned()
            .unwrap_or(name)
    }

    /// Register the sources the player added in Settings, replacing the
    /// ones registered before.
    ///
    /// A folder is walked on the manifest worker, and a URL pack is a
    /// manifest the loader fetches. Both arrive later: the state is
    /// `Loading` until then, and the source keeps the banks of its previous
    /// adoption in the meantime. A folder that does not exist is reported
    /// as `Missing` at once. All names go in the `global` layer, below the
    /// open set's folder and above the pinned banks.
    ///
    /// A source that cannot be read is reported and skipped. It is never
    /// fatal: a folder on an unplugged drive must not stop the studio from
    /// opening, and the sound browser shows the source as missing.
    pub fn adopt_global_sources(&self, sources: &[GlobalSource]) -> Vec<GlobalSourceReport> {
        let sources = deduplicated_global_sources(sources);
        let generation = self
            .shared
            .global_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let previous = std::mem::take(
            &mut *self
                .shared
                .global_slots
                .lock()
                .expect("global source slots"),
        );
        let mut slots: Vec<GlobalSlot> = Vec::with_capacity(sources.len());
        let mut packs: Vec<(usize, String)> = Vec::new();
        let mut folders: Vec<String> = Vec::new();
        for (row, source) in sources.iter().enumerate() {
            let spec = source.spec.trim();
            if spec.is_empty() {
                continue;
            }
            if !source.enabled {
                slots.push(GlobalSlot {
                    row,
                    spec: spec.to_owned(),
                    kind: if folder_of_spec(spec).is_some() {
                        GlobalKind::Folder
                    } else {
                        GlobalKind::Pack
                    },
                    banks: HashMap::new(),
                    state: GlobalSourceState::Off,
                    rewalk: false,
                });
                continue;
            }
            if let Some(folder) = folder_of_spec(spec) {
                // A folder that is not there has nothing to walk: its row
                // says so at once, and nothing waits in line for it.
                if matches!(Path::new(folder.trim()).try_exists(), Ok(false)) {
                    slots.push(GlobalSlot {
                        row,
                        spec: spec.to_owned(),
                        kind: GlobalKind::Folder,
                        banks: HashMap::new(),
                        state: GlobalSourceState::Missing("no such folder".to_owned()),
                        rewalk: false,
                    });
                    continue;
                }
                // The walk happens on the manifest worker, exactly as a
                // pack's fetch does. A folder keeps what it brought last
                // time until the new walk lands, so re-adopting - which
                // every add, remove and reorder does - never silences a
                // library for the length of a directory scan.
                let carried = previous
                    .iter()
                    .find(|slot| slot.kind == GlobalKind::Folder && slot.spec == spec)
                    .map(|slot| slot.banks.clone())
                    .unwrap_or_default();
                slots.push(GlobalSlot {
                    row,
                    spec: spec.to_owned(),
                    kind: GlobalKind::Folder,
                    banks: carried,
                    state: GlobalSourceState::Loading,
                    rewalk: false,
                });
                folders.push(spec.to_owned());
                continue;
            }
            // A pack keeps what it brought last time until its new list
            // lands: a refetch must not silence it in the meantime.
            let carried = previous
                .iter()
                .find(|slot| slot.kind == GlobalKind::Pack && slot.spec == spec)
                .map(|slot| slot.banks.clone())
                .unwrap_or_default();
            slots.push(GlobalSlot {
                row,
                spec: spec.to_owned(),
                kind: GlobalKind::Pack,
                banks: carried,
                state: GlobalSourceState::Loading,
                rewalk: false,
            });
            packs.push((row, spec.to_owned()));
        }
        *self
            .shared
            .global_slots
            .lock()
            .expect("global source slots") = slots;
        rebuild_global(&self.shared, &self.global);
        if !folders.is_empty()
            && let Err(error) = self.enqueue_manifest_work_async_locked(
                ManifestWork::Folders {
                    specs: folders.clone(),
                    generation,
                },
                MANIFEST_BATCH_TIMEOUT,
            )
        {
            // The queue refused the walk, so nothing will ever fill these
            // rows: say so on them rather than leaving `fetching…` up for
            // the rest of the session.
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            for slot in slots.iter_mut() {
                if slot.kind == GlobalKind::Folder && folders.contains(&slot.spec) {
                    slot.state = GlobalSourceState::Failed(error.clone());
                }
            }
        }
        if !packs.is_empty()
            && let Err(error) = self.register_global_packs(&packs, generation)
        {
            let mut slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            for slot in slots.iter_mut() {
                if slot.kind == GlobalKind::Pack && slot.state == GlobalSourceState::Loading {
                    slot.state = GlobalSourceState::Failed(error.clone());
                }
            }
        }
        self.global_source_reports()
    }

    /// Adopt, and wait for every folder walk to land.
    ///
    /// For tests that want the settled reading rather than the one on the
    /// way to it. A studio never waits: its rows say `fetching…` and the
    /// half-second poll picks them up.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn adopt_global_sources_settled(
        &self,
        sources: &[GlobalSource],
    ) -> Vec<GlobalSourceReport> {
        let _ = self.adopt_global_sources(sources);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.shared.manifest_pending.load(Ordering::Acquire) != 0 && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.global_source_reports()
    }

    /// How every imported source stands right now, in row order - for the
    /// sources page, which asks again as packs land.
    pub fn global_source_reports(&self) -> Vec<GlobalSourceReport> {
        let slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        let mut rows: Vec<&GlobalSlot> = slots.iter().collect();
        rows.sort_by_key(|slot| slot.row);
        rows.iter()
            .map(|slot| GlobalSourceReport {
                spec: slot.spec.clone(),
                state: slot.state.clone(),
            })
            .collect()
    }

    /// The fetch policy the host gives a score, so an imported pack is
    /// granted no less: a pack on one host whose samples live on another is
    /// as ordinary as a score that does the same.
    pub fn set_import_policy(&self, access: ScoreSampleAccess) {
        *self.shared.import_policy.lock().expect("import policy") = Some(access);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn import_policy_for_test(&self) -> Option<ScoreSampleAccess> {
        self.shared
            .import_policy
            .lock()
            .expect("import policy")
            .clone()
    }

    /// Ask the loader for the packs the player named in Settings, in one
    /// job.
    ///
    /// A URL that the player types in Settings is a grant, like
    /// `--allow-sample-origin` for a score from another author. Each pack's
    /// origin is permitted exactly, in addition to the policy a score gets.
    /// A pack's `_base` can then point at a CDN, as a score's imports can.
    fn register_global_packs(
        &self,
        packs: &[(usize, String)],
        generation: u64,
    ) -> Result<(), String> {
        let mut access = self
            .shared
            .import_policy
            .lock()
            .expect("import policy")
            .clone()
            .unwrap_or_else(|| {
                let mut access = ScoreSampleAccess::denied();
                access.permit_public_cors_origins();
                access
            });
        let mut effects = Vec::with_capacity(packs.len());
        for (_, spec) in packs {
            let SampleSource::Url(url) = read_source(spec, "") else {
                return Err(format!("{spec}: not a URL source"));
            };
            let origin = Url::parse(&url)
                .map_err(|error| format!("{spec}: invalid sample source: {error}"))?
                .origin()
                .ascii_serialization();
            access.permit_origin(&origin)?;
            effects.push((
                serde_json::to_string(spec).map_err(|error| error.to_string())?,
                None,
            ));
        }
        self.register_batch_async_into(
            BankLayer::Global { generation },
            effects,
            &[],
            ManifestAccess::Score(access),
        )
        .map(|_| ())
    }

    /// Forget a pack's cached list, so the next adoption asks for it again
    /// rather than answering out of the cache: Enter on the row means "go
    /// and get it", and a `shabda:` source means a fresh draw.
    pub fn forget_manifest(&self, spec: &str) {
        let SampleSource::Url(url) = read_source(spec.trim(), "") else {
            return;
        };
        self.forget_manifest_url(&url);
        let map = serde_json::to_string(spec.trim()).unwrap_or_default();
        self.shared
            .source_tables
            .lock()
            .expect("samples source tables")
            .forget_state(&map);
    }

    /// Drop a pinned default pack's cached manifest so the next load asks
    /// the network again. Sample files already on disk are kept.
    pub fn forget_default_manifest(&self, url: &str) {
        self.forget_manifest_url(url);
    }

    /// Refetch every shipped pack's list from the network and merge what
    /// changed. Banks under those packs are cleared first so a refresh
    /// does not stack duplicate names.
    pub fn refresh_default_sources(&self) -> Result<(), String> {
        let pinned: PinnedFile = serde_json::from_str(PINNED_BANKS)
            .map_err(|error| format!("pinned sample-banks.json: {error}"))?;
        for source in &pinned.sources {
            self.forget_default_manifest(&source.url);
        }
        let bases: Vec<String> = Self::default_sources()
            .iter()
            .filter(|source| !self.is_font_source(source))
            .map(|source| source.base.clone())
            .collect();
        {
            let mut banks = self.banks.write().expect("default banks");
            banks.retain(|_, bank| {
                bank_file_urls(bank).first().is_none_or(|url| {
                    !bases
                        .iter()
                        .any(|base| url.starts_with(&*source_prefix(base)))
                })
            });
            banks.extend(inline_banks_from(&pinned));
        }
        self.enqueue_manifest_work_async(ManifestWork::Defaults {
            sources: pinned.sources,
            cache_dir: self.shared.host_cache.clone(),
        })
    }

    /// How many of these urls are on disk, and the bytes they take.
    pub fn count_cached_urls<'a>(
        &self,
        urls: impl IntoIterator<Item = &'a Arc<str>>,
    ) -> (usize, u64) {
        let mut held = 0usize;
        let mut bytes = 0u64;
        for url in urls {
            if let Some(len) = cache_entry_bytes(&self.shared.host_cache, url) {
                held += 1;
                bytes = bytes.saturating_add(len);
            }
        }
        (held, bytes)
    }

    fn forget_manifest_url(&self, url: &str) {
        let path = cache_path(&self.shared.host_cache, url);
        let _ = std::fs::remove_file(&path);
        let cache = &self.shared.score_cache;
        for trust in [ScoreCacheTrust::Grant, ScoreCacheTrust::Cors] {
            let path = cache.path(url, ScoreCacheKind::Manifest, trust);
            let _ = with_score_cache_lock(&cache.base, || match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!("forget {}: {error}", path.display())),
            });
        }
    }

    /// Whether the set's own folder already answers `name`.
    ///
    /// Consolidating must not write over audio the player put in the set: a
    /// `bd` folder beside the scores is the set's `bd`, and the pinned bank
    /// must not replace it.
    pub fn set_folder_holds(&self, root: &Path, name: &str) -> bool {
        // The prefix is spelled the way `folder_banks` writes its URLs: the
        // raw path, not a percent-encoded URL, so a set path with a space
        // still matches. The separator is this platform's, because
        // `root.join(...)` builds those URLs and joins with a backslash on
        // Windows.
        let spelled =
            |root: &Path| format!("{}{}", local_file_url(root), std::path::MAIN_SEPARATOR);
        // Both spellings are tried, because the URLs carry whatever root the
        // caller handed `folder_banks` and this is handed one that may not be
        // the same text. Canonicalising is what reconciles them on Unix, where
        // the usual difference is a symlinked temporary directory. On Windows
        // `canonicalize` answers in the `\\?\` verbatim form, which
        // `local_file_url` strips from both sides, so the two spellings meet
        // in the middle. Accepting either is what makes "does this folder
        // hold that bank" mean the same thing on both.
        let mut prefixes = vec![spelled(root)];
        if let Ok(canonical) = root.canonicalize() {
            let canonical = spelled(&canonical);
            if !prefixes.contains(&canonical) {
                prefixes.push(canonical);
            }
        }
        let under = |url: &str| prefixes.iter().any(|prefix| url.starts_with(prefix));
        let holds = |bank: &Bank| match bank {
            Bank::Array(urls) => urls.iter().any(|url| under(url)),
            Bank::Notes(notes) => notes
                .iter()
                .any(|(_, urls)| urls.iter().any(|url| under(url))),
        };
        // Only the set's own layer is asked, not the whole chain: a name
        // the score's `samples()` took over is not the folder's.
        self.custom
            .read()
            .expect("custom banks")
            .get(name)
            .is_some_and(holds)
    }

    /// Copy every file behind `name` into `root`, under a folder of its
    /// own, and report how many landed.
    ///
    /// The naming is the scan's own rule read backwards: files inside a
    /// folder become a bank called after the folder, so `name/0.wav`,
    /// `name/1.wav` is what makes the copied bank answer to `name` - and
    /// `n` still picks between them, in the order the bank had.
    ///
    /// The copy is refused if `name` is not one folder name or the destination
    /// escapes `root` through a path or link.
    ///
    /// Only what is already downloaded is copied. A sound whose files have
    /// never been fetched is reported as nothing copied rather than
    /// silently reaching the network in the middle of a file operation.
    pub fn copy_bank_into(&self, name: &str, root: &Path) -> Result<usize, String> {
        enum Copyable {
            Files(Vec<Arc<str>>),
            Keyed,
            Font,
        }
        let Some(found) = self.look_up(name, |named| match named {
            Named::Bank(Bank::Array(urls)) => Copyable::Files(urls.to_vec()),
            // A keyed bank picks its file by the note nearest the key it is
            // filed under; laid out as a row the scan would read it back
            // by `n`, transposed from C3 - the same name, a different
            // instrument. It is left where it is rather than copied wrong.
            Named::Bank(Bank::Notes(_)) => Copyable::Keyed,
            // A soundfont is a font file and a decode of it, not a set of
            // samples with names: there is nothing to lay in a folder that
            // would play the same way.
            Named::Font(_) => Copyable::Font,
        }) else {
            return Ok(0);
        };
        let urls = match found {
            Copyable::Files(urls) => urls,
            Copyable::Keyed => {
                return Err(format!(
                    "{name} is a keyed bank and cannot be laid out as a row; it stays where it is"
                ));
            }
            Copyable::Font => return Ok(0),
        };
        if urls.is_empty() {
            return Ok(0);
        }
        // All or nothing: a bank copied with gaps is re-read by the scan
        // with the gaps closed, and `s("bd:7")` plays what used to be
        // `bd:9`. Better to say what is missing than to change the score's
        // meaning in the act of making it portable.
        let mut files = Vec::with_capacity(urls.len());
        let mut missing = 0usize;
        for url in &urls {
            match self.cached_bytes_for(url) {
                Some(bytes) => files.push((url.clone(), bytes)),
                None => missing += 1,
            }
        }
        if missing > 0 {
            return Err(format!(
                "{missing} of {} files are not downloaded yet - play it once, or turn on fetch imports",
                urls.len()
            ));
        }
        let mut components = Path::new(name).components();
        if !matches!(
            components.next(),
            Some(std::path::Component::Normal(component))
                if component == std::ffi::OsStr::new(name)
        ) || components.next().is_some()
        {
            return Err(format!(
                "cannot copy {name}: a bank name must be one folder name"
            ));
        }
        std::fs::create_dir_all(root)
            .map_err(|error| format!("create {}: {error}", root.display()))?;
        let root = root
            .canonicalize()
            .map_err(|error| format!("read {}: {error}", root.display()))?;
        let folder = root.join(name);
        if std::fs::symlink_metadata(&folder)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(format!(
                "cannot copy {name}: the bank folder is a symbolic link"
            ));
        }
        std::fs::create_dir_all(&folder)
            .map_err(|error| format!("create {}: {error}", folder.display()))?;
        let folder = folder
            .canonicalize()
            .map_err(|error| format!("read {}: {error}", folder.display()))?;
        if folder.parent() != Some(root.as_path()) {
            return Err(format!(
                "cannot copy {name}: the bank folder leaves the destination"
            ));
        }
        // Zero-padded, because the scan orders a bank's files by name and
        // `10.wav` sorts before `2.wav`.
        let width = urls.len().to_string().len();
        let mut copied = 0usize;
        for (index, (url, bytes)) in files.iter().enumerate() {
            // The extension the scan will accept, read the way the decoder
            // reads it: from the path, not from a query or a fragment.
            let extension = match codec_for(url) {
                Codec::Mp3 => "mp3",
                Codec::Ogg => "ogg",
                _ => "wav",
            };
            let into = folder.join(format!("{index:0width$}.{extension}"));
            if std::fs::symlink_metadata(&into)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(format!(
                    "cannot copy {name}: {} is a symbolic link",
                    into.display()
                ));
            }
            std::fs::write(&into, bytes)
                .map_err(|error| format!("write {}: {error}", into.display()))?;
            copied += 1;
        }
        Ok(copied)
    }

    /// The bytes behind a url, if this machine already has them: a local
    /// file read straight, a downloaded one out of the cache.
    ///
    /// Never a fetch. Consolidating is a file operation the player asked
    /// for, and one that quietly went to the network in the middle would
    /// be a very different thing from the one they asked for.
    fn cached_bytes_for(&self, url: &str) -> Option<Vec<u8>> {
        if url.starts_with("file://") {
            return fetch_located(url).ok();
        }
        // A url a score or a Settings pack named was fetched under a grant
        // and lives in the score cache, keyed by the trust its grant
        // carried. The pinned banks live in the plain cache beside it.
        let granted = self
            .shared
            .score_sources
            .read()
            .expect("score sample sources")
            .contains_key(url);
        if granted {
            let cache = &self.shared.score_cache;
            return [ScoreCacheTrust::Grant, ScoreCacheTrust::Cors]
                .into_iter()
                .find_map(|trust| {
                    read_sample_cache(&cache.path(url, ScoreCacheKind::Audio, trust)).ok()
                });
        }
        read_sample_cache(&cache_path(&cache_dir(), url)).ok()
    }

    /// Every sound a score can name right now, for a browser: banks from
    /// the pinned manifests, banks a score registered, and the General MIDI
    /// fonts, each with how many numbered variants `n` can pick from.
    /// Banks a Settings import brought, under the names they play as.
    pub fn banks_for_import(&self, spec: &str) -> Vec<String> {
        // Read original names from the source row. An alias must not erase
        // the identity used to edit that same alias again.
        let slots = self
            .shared
            .global_slots
            .lock()
            .expect("global source slots");
        let mut names: Vec<String> = slots
            .iter()
            .filter(|slot| slot.spec == spec)
            .flat_map(|slot| slot.banks.keys().cloned())
            .collect();
        drop(slots);
        // Score-shaped imports may exist outside a Settings source slot.
        let imports = self.shared.bank_imports.read().expect("bank imports");
        names.extend(
            imports
                .iter()
                .filter(|(_, import)| import.as_ref() == spec)
                .map(|(name, _)| name.clone()),
        );
        drop(imports);
        names.sort();
        names.dedup();
        names
    }

    /// Automatic collision aliases currently assigned to one Settings source.
    pub fn automatic_aliases_for_import(&self, spec: &str) -> Vec<(String, String)> {
        let aliases = self
            .shared
            .auto_aliases
            .read()
            .expect("automatic bank aliases");
        let mut found: Vec<_> = aliases
            .iter()
            .filter(|((source, _), _)| source == spec)
            .map(|((_, original), alias)| (original.clone(), alias.clone()))
            .collect();
        found.sort();
        found
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_catalogue_lock_for_tests<T>(&self, f: impl FnOnce() -> T) -> T {
        let _banks = self.custom.write().expect("custom banks");
        f()
    }

    pub fn catalogue(&self) -> Vec<SoundEntry> {
        let mut entries = Vec::new();
        let variants = |bank: &Bank| match bank {
            Bank::Array(urls) => urls.len(),
            Bank::Notes(notes) => notes.iter().map(|(_, urls)| urls.len()).max().unwrap_or(1),
        };
        let location = |bank: &Bank| match bank {
            Bank::Array(urls) => urls.first().map(|url| url.to_string()),
            Bank::Notes(notes) => notes
                .iter()
                .find_map(|(_, urls)| urls.first())
                .map(|url| url.to_string()),
        };
        // One order for the three, and the same one `publish_score_custom`
        // and `adopt_set_folder` take them in: `custom`, then the imports,
        // then the set's. The browser reads them on the studio's thread
        // while the loader writes them on the worker's, and a pair taken in
        // both orders is a pair that can wait on itself.
        let custom = self.custom.read().expect("custom banks");
        let imports = self.shared.bank_imports.read().expect("bank imports");
        let of_set = self.shared.set_banks.read().expect("set banks");
        for (name, bank) in custom.iter() {
            // The set's own folder brought this one, so say so: a `bd` that
            // is the set's rather than the pinned one is the difference
            // between a surprise and a choice.
            let set = of_set.contains_key(name);
            entries.push(SoundEntry {
                name: name.clone(),
                variants: variants(bank),
                variant_names: local_variant_names(bank),
                origin: if set {
                    SoundOrigin::Set
                } else {
                    SoundOrigin::Score
                },
                category: if set {
                    SoundCategory::Set
                } else {
                    SoundCategory::Score
                },
                location: location(bank),
                import: imports.get(name).map(|import| import.to_string()),
            });
        }
        drop(of_set);
        drop(imports);
        drop(custom);
        let mut names: HashSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        let source_of = self.shared.global_source_of.read().expect("global sources");
        for (name, bank) in self.global.read().expect("global banks").iter() {
            if !names.insert(name.clone()) {
                continue;
            }
            entries.push(SoundEntry {
                name: name.clone(),
                variants: variants(bank),
                variant_names: local_variant_names(bank),
                origin: SoundOrigin::Global,
                category: SoundCategory::Mine,
                location: location(bank),
                import: source_of.get(name).map(|spec| spec.to_string()),
            });
        }
        drop(source_of);
        for (name, bank) in self.banks.read().expect("default banks").iter() {
            if !names.insert(name.clone()) {
                continue;
            }
            let location = location(bank);
            entries.push(SoundEntry {
                name: name.clone(),
                variants: variants(bank),
                variant_names: local_variant_names(bank),
                origin: SoundOrigin::Default,
                category: SoundCategory::of_location(location.as_deref()),
                location,
                import: None,
            });
        }
        for (name, fonts) in self.gm.iter() {
            if !names.insert(name.clone()) {
                continue;
            }
            entries.push(SoundEntry {
                name: name.clone(),
                variants: fonts.len(),
                variant_names: Vec::new(),
                origin: SoundOrigin::Font,
                category: SoundCategory::Font,
                location: None,
                import: None,
            });
        }
        // The synths, unless a bank has taken the name - a bank wins at
        // play time, so the browser says the same.
        for name in rustel_voice::NATIVE_SYNTH_SOUNDS {
            if !names.insert((*name).to_owned()) {
                continue;
            }
            entries.push(SoundEntry {
                name: (*name).to_owned(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Synth,
                category: SoundCategory::Synth,
                location: None,
                import: None,
            });
        }
        // The audio input stands with the synths: what `s("…")` plays
        // without a sample. Its channels are its variants, and how many
        // there are is the device's to say - a browser with a device sets
        // the count; here it is one, the channel `in` alone plays.
        if names.insert("in".to_owned()) {
            entries.push(SoundEntry {
                name: "in".to_owned(),
                variants: 1,
                variant_names: Vec::new(),
                origin: SoundOrigin::Input,
                category: SoundCategory::Synth,
                location: None,
                import: None,
            });
        }
        // Case-insensitive lookup registers a lower-case key beside a
        // canonical mixed-case bank. They are two spellings of the same
        // sound, not two browser rows. Keep the canonical spelling while
        // leaving genuinely distinct banks that happen to differ by case.
        let mut visible: Vec<SoundEntry> = Vec::with_capacity(entries.len());
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for entry in entries {
            let candidates = by_name.entry(entry.name.to_ascii_lowercase()).or_default();
            let same = candidates.iter().copied().find(|&index| {
                let shown = &visible[index];
                shown.variants == entry.variants
                    && shown.origin == entry.origin
                    && shown.category == entry.category
                    && shown.location == entry.location
                    && shown.import == entry.import
            });
            if let Some(index) = same {
                let shown = &mut visible[index];
                let shown_has_case = shown.name.chars().any(char::is_uppercase);
                let entry_has_case = entry.name.chars().any(char::is_uppercase);
                if entry_has_case && !shown_has_case {
                    *shown = entry;
                }
            } else {
                candidates.push(visible.len());
                visible.push(entry);
            }
        }
        let mut entries = visible;
        // Synths first, then the banks by name: the browser groups the
        // synths under one heading at the top.
        entries.sort_by_key(|entry| {
            (
                !matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input),
                entry.name.to_lowercase(),
            )
        });
        entries
    }

    /// Where one sound stands, and - for a name that is known but not yet
    /// fetched - the request that fetches it. Asking is what starts the
    /// download, so a score can be warmed as its names are typed. Asked as
    /// a bet: it queues behind what plays, and does not retry a failure.
    pub fn readiness(&self, name: &str, n: f64) -> SoundReadiness {
        self.readiness_at(name, n, 36.0)
    }

    /// [`Self::readiness`] for the file a note picks: in a bank keyed by
    /// note, `midi` chooses the key.
    pub fn readiness_at(&self, name: &str, n: f64, midi: f64) -> SoundReadiness {
        if rustel_voice::is_native_synth_sound(name) {
            return SoundReadiness::Ready;
        }
        match self.resolve_with_priority(name, n, midi, LoadPriority::Bet) {
            SampleResolution::Found { .. } => SoundReadiness::Ready,
            SampleResolution::Loading => SoundReadiness::Loading,
            SampleResolution::Failed => SoundReadiness::Failed,
            SampleResolution::Unknown => SoundReadiness::Unknown,
        }
    }

    /// The keys of a bank whose files are picked by note, as MIDI numbers;
    /// `None` for any other kind of sound, or a name not known yet.
    pub fn note_keys(&self, name: &str) -> Option<Vec<f64>> {
        self.look_up(name, |named| match named {
            Named::Bank(Bank::Notes(notes)) => Some(notes.iter().map(|(key, _)| *key).collect()),
            _ => None,
        })
        .flatten()
    }

    /// The shape of the sample `s("name")` would play, and how long it
    /// lasts.
    ///
    /// `name` is spelled the way a browser spells it: `bd`, or `bd:3` for
    /// a numbered variant. `None` until the sample has been decoded once,
    /// because the shape is measured at decode - the only moment the
    /// library holds the audio. A soundfont has no one shape and answers
    /// nothing.
    pub fn sound_shape(&self, name: &str) -> Option<(Arc<[u8]>, f64)> {
        let (bank, n) = match name.rsplit_once(':') {
            Some((head, tail)) => match tail.parse::<f64>() {
                Ok(n) if !head.is_empty() => (head, n),
                _ => (name, 0.0),
            },
            None => (name, 0.0),
        };
        let url = self.look_up(bank, |named| match named {
            // A browser preview carries no `note`, so the sampler uses its
            // ordinary MIDI 36 default. This must choose the same key as the
            // voice: MIDI 60 can be another file in a note-keyed bank, whose
            // shape was never decoded by the preview we just heard.
            Named::Bank(bank) => pick_from_bank(bank, n, 36.0).map(|(url, _)| url),
            Named::Font(_) => None,
        })??;
        let shape = self
            .shared
            .shapes
            .lock()
            .expect("sample shapes")
            .by_url
            .get(&url)
            .cloned()?;
        let duration = match self
            .shared
            .by_url
            .read()
            .expect("sample url table")
            .get(&url)
        {
            Some(UrlState::Ready { duration_secs, .. }) => *duration_secs,
            _ => return None,
        };
        Some((shape, duration))
    }

    /// Whether a `samples(...)` the score wrote - its string argument, as
    /// written - has been registered and published. A string argument
    /// reaches the library as the JSON text it stringifies to, so both
    /// spellings are tried.
    pub fn knows_samples_source(&self, spec: &str) -> bool {
        let tables = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        let sources = &tables.sources;
        sources.contains(spec)
            || serde_json::to_string(spec).is_ok_and(|quoted| sources.contains(&quoted))
    }

    /// Source status for the Studio catalogue worker's next snapshot.
    pub fn browser_source_states(&self) -> HashMap<String, SourceState> {
        self.shared
            .source_tables
            .lock()
            .expect("samples source tables")
            .states
            .iter()
            .map(|(map, standing)| (map.clone(), standing.state.clone()))
            .collect()
    }

    /// Where a `samples("…")` the score wrote stands - its string argument,
    /// as written - or None for one nobody has asked the library for.
    pub fn samples_source_state(&self, spec: &str) -> Option<SourceState> {
        let tables = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        let states = &tables.states;
        states
            .get(spec)
            .or_else(|| {
                serde_json::to_string(spec)
                    .ok()
                    .and_then(|quoted| states.get(&quoted))
            })
            .map(|standing| standing.state.clone())
    }

    /// Whether a `samples("…")` whose map failed may be asked for again:
    /// the failure has rested, and the loader has nothing else in hand, so
    /// a retry never waits in line behind another. An ask rests
    /// [`FAILED_RETRY_AFTER`]; a `background` one also waits out a rest that
    /// doubles with each failure in a row, up to [`FAILED_RETRY_LONGEST`].
    pub fn samples_source_rested(&self, spec: &str, background: bool) -> bool {
        let Ok(map) = serde_json::to_string(spec) else {
            return false;
        };
        self.manifests_pending() == 0
            && self
                .shared
                .source_tables
                .lock()
                .expect("samples source tables")
                .states
                .get(&map)
                .is_some_and(|standing| {
                    let rest = if background {
                        standing.rest()
                    } else {
                        FAILED_RETRY_AFTER
                    };
                    standing.failed() && standing.since.elapsed() >= rest
                })
    }

    /// Mark `map` as on its way. A failure keeps its reason until the job's
    /// verdict replaces it, so asking again does not lift the refusal the
    /// failure explains.
    pub(super) fn mark_source_loading(&self, map: &str) {
        let mut tables = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables");
        if !tables.states.get(map).is_some_and(Standing::failed) {
            tables.note(map, SourceState::Loading);
        }
    }

    /// Ask for a `samples("…")` a score names before the score is
    /// evaluated, as the checker sees it: its map is fetched under the same
    /// grant an evaluation would use, so its banks are in the browser to
    /// look at, and its names judged, while the score is still being
    /// typed. A spec nobody has asked for is asked, and one whose failure
    /// has rested is asked again ([`Self::samples_source_rested`]); one on
    /// its way or in is left as it stands. A refusal is recorded as the
    /// spec's failure, so a checker can show it.
    pub fn look_up_samples_source(
        &self,
        spec: &str,
        access: &ScoreSampleAccess,
    ) -> Result<(), String> {
        let map = serde_json::to_string(spec).map_err(|error| error.to_string())?;
        let failures = || {
            self.shared
                .source_tables
                .lock()
                .expect("samples source tables")
                .states
                .get(&map)
                .map(|standing| standing.failures)
        };
        let asked = failures();
        if asked.is_some() && !self.samples_source_rested(spec, false) {
            return Ok(());
        }
        self.register_batch_async_into_with_intent(
            BankLayer::Score,
            vec![(map.clone(), None)],
            &[],
            ManifestAccess::Score(access.clone()),
            PrefetchIntent::Explicit,
        )
        .map(|_| ())
        .inspect_err(|error| {
            // A job the line refused has already counted as the map's
            // failure; an ask turned away before that counts here.
            if failures() == asked {
                self.shared
                    .note_source_standing(&map, SourceState::Failed(error.clone()));
            }
        })
    }

    /// What the manifest worker notes for a score's `samples("…")` once its
    /// map is in - the argument as JSON.stringify writes it - for tests
    /// that cannot fetch.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn note_samples_source_for_tests(&self, spec: &str) {
        let map = serde_json::to_string(spec).expect("a string");
        self.shared.note_source_standing(&map, SourceState::Ready);
    }

    /// Where a `samples("…")` stands, for tests that cannot fetch.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn note_samples_source_state_for_tests(&self, spec: &str, state: SourceState) {
        let map = serde_json::to_string(spec).expect("a string");
        if state == SourceState::Ready {
            self.note_samples_source_for_tests(spec);
        } else {
            self.shared.note_source_standing(&map, state);
        }
    }

    /// Age a `samples("…")`'s standing by [`FAILED_RETRY_AFTER`], as if
    /// that long had passed.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn rest_samples_source_for_tests(&self, spec: &str) {
        let map = serde_json::to_string(spec).expect("a string");
        if let Some(standing) = self
            .shared
            .source_tables
            .lock()
            .expect("samples source tables")
            .states
            .get_mut(&map)
        {
            standing.since -= FAILED_RETRY_AFTER;
        }
    }

    /// Whether any bank has been registered yet. A library with none knows
    /// too little to say a sound does not exist.
    pub fn has_banks(&self) -> bool {
        !self.banks.read().expect("default banks").is_empty()
            || !self.custom.read().expect("custom banks").is_empty()
            || !self.global.read().expect("global banks").is_empty()
    }

    /// The packs the studio ships with, in the order the pin file lists
    /// them: each pinned manifest that brings files, the inline banks, and
    /// the General MIDI soundfonts last.
    ///
    /// A pin that only maps aliases onto another pack's banks brings no
    /// files of its own, so it is not a row: there would be nothing to
    /// count and nothing to cache under it.
    pub fn default_sources() -> &'static [DefaultSource] {
        static SOURCES: std::sync::OnceLock<Vec<DefaultSource>> = std::sync::OnceLock::new();
        SOURCES.get_or_init(|| default_sources_from(PINNED_BANKS, PINNED_GM_FONTS))
    }

    /// What a shipped pack holds right now: how many sounds this library
    /// knows under it, and how many distinct files they name. Zero of
    /// both until its manifest has landed.
    pub fn default_source_holds(&self, source: &DefaultSource) -> (usize, usize) {
        let (sounds, files) = self.default_source_files(source);
        (sounds, files.len())
    }

    /// Fetch every file of one shipped pack onto disk, skipping what is
    /// there already. `Enter` on its row, and one pass of `c` for the
    /// whole library: what the row then counts down is
    /// [`Self::pending_cache_under`] its base.
    pub fn cache_default_source(&self, source: &DefaultSource) -> CacheRequest {
        if self.is_font_source(source) {
            let fonts = self.default_font_files();
            let queued = fonts
                .iter()
                .filter(|font| queue_font_cache(&self.shared, font))
                .count();
            return CacheRequest {
                sounds: self.gm.len(),
                files: fonts.len(),
                queued,
            };
        }
        let (sounds, files) = self.default_source_files(source);
        let queued = files
            .iter()
            .filter(|url| ensure_cached_shared(&self.shared, url))
            .count();
        CacheRequest {
            sounds,
            files: files.len(),
            queued,
        }
    }

    /// Disk-cache fetches still in the line for files under `base`: the
    /// number a pack's row counts down while the library is being cached.
    ///
    /// Only claims that still have a job, or a file in a loader's hand: a
    /// claim left behind with no job would pin the last file of a pack on
    /// the row forever.
    pub fn pending_cache_under(&self, base: &str) -> usize {
        let prefix = source_prefix(base);
        let mut caching = self.shared.caching.lock().expect("disk-cache claims");
        let state = self.shared.jobs.state.lock().expect("sample load queue");
        let loading = self.shared.loading.lock().expect("loading sample");
        let live = |url: &str| {
            state.now.iter().any(|job| &*job.url == url)
                || state.bets.iter().any(|job| &*job.url == url)
                || loading.iter().any(|hand| hand.as_ref() == url)
        };
        // Drop orphan claims so a later ask can try again, and so the row
        // reaches "done" instead of counting ghosts.
        caching.retain(|url| !url.starts_with(&*prefix) || live(url));
        caching
            .iter()
            .filter(|url| url.starts_with(&*prefix))
            .count()
    }

    /// How much of a shipped pack is already on disk: files held, files
    /// named, and the bytes those held files take in the host cache.
    pub fn default_source_cached(&self, source: &DefaultSource) -> (usize, usize, u64) {
        if self.is_font_source(source) {
            let fonts = self.default_font_files();
            let total = fonts.len();
            let mut held = 0usize;
            let mut bytes = 0u64;
            for font in fonts {
                let url = font_file_url(&self.shared.font_base, &font);
                if let Some(len) = cache_entry_bytes(&self.shared.host_cache, &url) {
                    held += 1;
                    bytes = bytes.saturating_add(len);
                }
            }
            return (held, total, bytes);
        }
        let (_, files) = self.default_source_files(source);
        let total = files.len();
        let mut held = 0usize;
        let mut bytes = 0u64;
        for url in files {
            if let Some(len) = cache_entry_bytes(&self.shared.host_cache, &url) {
                held += 1;
                bytes = bytes.saturating_add(len);
            }
        }
        (held, total, bytes)
    }

    /// Whether a file in a loader's hand belongs to the pack at `base`.
    pub fn loading_under(&self, base: &str) -> Option<String> {
        let prefix = source_prefix(base);
        let loading = self.shared.loading.lock().expect("loading sample");
        loading
            .iter()
            .find(|url| url.starts_with(&*prefix))
            .map(|url| file_name_of_url(url))
    }

    fn is_font_source(&self, source: &DefaultSource) -> bool {
        !self.shared.font_base.is_empty()
            && source_prefix(&source.base) == source_prefix(&self.shared.font_base)
    }

    /// The sounds under a shipped pack and the distinct files they name.
    /// Distinct, because the case-insensitive and alias copies of a bank
    /// point at the same files, and a count that took each copy as its own
    /// download would have a bar reach the end at half way.
    pub fn default_source_files(&self, source: &DefaultSource) -> (usize, HashSet<Arc<str>>) {
        if self.is_font_source(source) {
            let files = self
                .default_font_files()
                .into_iter()
                .map(|font| Arc::from(font_file_url(&self.shared.font_base, &font)))
                .collect();
            return (self.gm.len(), files);
        }
        let prefix = source_prefix(&source.base);
        let mut files = HashSet::new();
        let mut sounds = 0;
        // Snapshot only this pack so catalogue and manifest work can keep
        // using the table while its files are collected. Matching lowercase
        // copies share the first URL and therefore stay in the same snapshot.
        let banks: Vec<_> = self
            .banks
            .read()
            .expect("default banks")
            .iter()
            .filter_map(|(name, bank)| {
                let first = match bank {
                    Bank::Array(urls) => urls.first(),
                    Bank::Notes(notes) => notes.iter().find_map(|(_, urls)| urls.first()),
                };
                first
                    .is_some_and(|url| url.starts_with(&*prefix))
                    .then(|| (name.clone(), bank.clone()))
            })
            .collect();
        let mut lookup_copies: HashMap<String, HashSet<Vec<&Arc<str>>>> = HashMap::new();
        for (name, bank) in &banks {
            let lowercase = name.to_lowercase();
            if lowercase != *name {
                lookup_copies
                    .entry(lowercase)
                    .or_default()
                    .insert(bank_file_urls(bank));
            }
        }
        for (name, bank) in banks.iter() {
            let urls = bank_file_urls(bank);
            let lowercase_lookup_copy = name == &name.to_lowercase()
                && lookup_copies
                    .get(name)
                    .is_some_and(|copies| copies.contains(&urls));
            if !lowercase_lookup_copy {
                sounds += 1;
            }
            files.extend(urls.into_iter().cloned());
        }
        if sounds == 0
            && let Some((pinned_sounds, pinned_files)) = pinned_inline_files(source)
        {
            return (pinned_sounds, pinned_files);
        }
        (sounds, files)
    }

    /// What a user import holds right now: sounds this library knows under
    /// it, and how many distinct files they name. Zero of both until its
    /// list has landed.
    pub fn import_source_holds(&self, spec: &str) -> (usize, usize) {
        let (sounds, files) = self.import_source_files(spec);
        (sounds, files.len())
    }

    /// Fetch every remote file of one user import onto disk, skipping what
    /// is there already. Local folders queue nothing: they are the cache.
    pub fn cache_import_source(&self, spec: &str) -> CacheRequest {
        if source_is_local(spec) {
            let (sounds, files) = self.import_source_files(spec);
            return CacheRequest {
                sounds,
                files: files.len(),
                queued: 0,
            };
        }
        let (sounds, files) = self.import_source_files(spec);
        let queued = files
            .iter()
            .filter(|url| ensure_cached_shared(&self.shared, url))
            .count();
        CacheRequest {
            sounds,
            files: files.len(),
            queued,
        }
    }

    /// How much of a user import is already on disk: files held, files
    /// named, and the bytes those held files take in the host cache.
    /// Local folders report every file as held, sized from the folder.
    pub fn import_source_cached(&self, spec: &str) -> (usize, usize, u64) {
        let (_, files) = self.import_source_files(spec);
        let total = files.len();
        let mut held = 0usize;
        let mut bytes = 0u64;
        for url in files {
            if let Some(len) = cache_entry_bytes(&self.shared.host_cache, &url) {
                held += 1;
                bytes = bytes.saturating_add(len);
            }
        }
        (held, total, bytes)
    }

    /// Disk-cache fetches still in the line for this exact set of files.
    /// User packs do not share a CDN prefix the way shipped packs do, so
    /// the row counts the URLs it named rather than a base string.
    pub fn pending_cache_of(&self, files: &HashSet<Arc<str>>) -> usize {
        let mut caching = self.shared.caching.lock().expect("disk-cache claims");
        let state = self.shared.jobs.state.lock().expect("sample load queue");
        let loading = self.shared.loading.lock().expect("loading sample");
        let live = |url: &str| {
            state.now.iter().any(|job| &*job.url == url)
                || state.bets.iter().any(|job| &*job.url == url)
                || loading.iter().any(|hand| hand.as_ref() == url)
        };
        caching.retain(|url| !files.contains(url) || live(url));
        caching.iter().filter(|url| files.contains(*url)).count()
    }

    /// Disk-cache fetches still in the line for a user import, counted
    /// from the files that pack currently names.
    pub fn pending_cache_for_import(&self, spec: &str) -> usize {
        let (_, files) = self.import_source_files(spec);
        self.pending_cache_of(&files)
    }

    /// Whether a file in a loader's hand belongs to this set of URLs.
    pub fn loading_among(&self, files: &HashSet<Arc<str>>) -> Option<String> {
        let loading = self.shared.loading.lock().expect("loading sample");
        loading
            .iter()
            .find(|url| files.contains(*url))
            .map(|url| file_name_of_url(url))
    }

    /// The file in hand for a user import, when the loader has one.
    pub fn loading_for_import(&self, spec: &str) -> Option<String> {
        let (_, files) = self.import_source_files(spec);
        self.loading_among(&files)
    }

    /// True when `name` is a bank a score or Settings import brought -
    /// not a shipped default, a font, or a synth. Fetch imports caches
    /// these when the score names them; `c` on a pack caches the rest.
    pub fn is_imported_sound(&self, name: &str) -> bool {
        self.custom.read().expect("custom banks").contains_key(name)
            || self.global.read().expect("global banks").contains_key(name)
    }

    /// The sounds under a user import and the distinct files they name.
    pub fn import_source_files(&self, spec: &str) -> (usize, HashSet<Arc<str>>) {
        let spec = spec.trim();
        let banks: Vec<Bank> = {
            let slots = self
                .shared
                .global_slots
                .lock()
                .expect("global source slots");
            let Some(slot) = slots.iter().find(|slot| slot.spec == spec) else {
                return (0, HashSet::new());
            };
            slot.banks.values().cloned().collect()
        };
        let mut files = HashSet::new();
        let mut sounds = 0;
        for bank in &banks {
            let urls = bank_file_urls(bank);
            if urls.is_empty() {
                continue;
            }
            sounds += 1;
            files.extend(urls.into_iter().cloned());
        }
        (sounds, files)
    }

    /// Every soundfont file the General MIDI names reach, once each: the
    /// fonts share files across instruments.
    fn default_font_files(&self) -> HashSet<Arc<str>> {
        self.gm.values().flatten().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dedup_keeps_case_distinct_folders_and_folds_same_folder_spellings() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join("Kit")).expect("first folder");
        // On a filesystem that folds case, the second folder IS the first
        // one and the create fails; on one that does not, both exist.
        let folded = std::fs::create_dir(root.path().join("kit")).is_err();
        let sources = ["Kit", "kit"]
            .into_iter()
            .map(|name| GlobalSource {
                spec: root.path().join(name).to_string_lossy().into_owned(),
                enabled: true,
            })
            .collect::<Vec<_>>();
        let kept = deduplicated_global_sources(&sources);
        if folded {
            assert_eq!(kept.len(), 1, "two spellings of one folder stay one source");
        } else {
            assert_eq!(kept.len(), 2, "Kit and kit are two folders, not one");
        }
    }

    #[cfg(test)]
    mod loading_name_tests {
        use super::file_name_of_url;

        #[test]
        fn the_file_in_hand_is_named_by_its_last_segment() {
            assert_eq!(
                file_name_of_url("https://example.org/packs/drums/kick%20one.wav?raw=1"),
                "kick one.wav"
            );
            assert_eq!(
                file_name_of_url("file:///tmp/my%20drums/snare.wav"),
                "snare.wav"
            );
            assert_eq!(file_name_of_url("snare.wav"), "snare.wav");
            assert_eq!(file_name_of_url("bad%zz.wav"), "bad%zz.wav");
            assert_eq!(file_name_of_url("dir/"), "");
        }
    }
}
