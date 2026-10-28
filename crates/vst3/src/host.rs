//! The plugin list, the plugin thread, and the names a score uses.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use rustel_audio::{
    InsertControls, InsertKey, InsertNote, InsertParam, InsertProvider, InsertSlot,
    MAX_INSERT_PARAMS, OrbitInsert,
};

use crate::cache::{self, Cache, Scanned};
use crate::instance::{CopyState, Instance, Loaded, Place};
use crate::module::{ClassInfo, Module};
use crate::watch::Word;
use crate::{Job, ParamInfo, canonical, scan};

/// The program that loads one bundle in a process of its own for the scan.
/// A bundle with a fault at its load then ends that process and not the
/// host. The host runs `program`, `args`, then the bundle path, and the
/// program calls [`crate::serve`] with the path.
///
/// With no worker program, the host runs no scan.
#[derive(Clone)]
pub struct WorkerProgram {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// The plugins the host keeps ready for a slot that did not take them yet.
/// One score needs one for each slot at most, so a start that waits for
/// its plugins finds all of them. A new build at the bound ends a failed
/// build first, then a copy of a chain no caller names, then a copy a
/// caller keeps at rest. A copy of a chain a caller names stays, unless
/// only copies at rest ask for it: see [`Host::keep`] and
/// [`State::make_room`]. A render ends no copy at rest: with no other build
/// to end, it builds past the bound. The bound does not count the build of
/// a render and does not end it before the render takes its copy. A copy
/// that comes home from an engine ends a build of a lower rank only: see
/// [`State::park`].
const MAX_PARKED: usize = rustel_audio::INSERT_SLOTS;
/// The copies one job of [`Host::keep`] ends at most. A job at the end of
/// the line ends the others, so a build or a park that joins the line
/// meanwhile waits for these ends only. One end job at most is in the line,
/// also after many calls.
const ENDS_PER_JOB: usize = 4;
/// A prepared plugin waits behind the read of the folders and the load:
/// its build goes to the end of the line this many times at most.
const PREPARE_TRIES: u32 = 4;

/// What [`Host::prepare`] builds: the plugin and the preset by name, and
/// the slot.
#[derive(Clone, PartialEq)]
struct Prepare {
    name: String,
    preset: Option<String>,
    instrument: bool,
    sample_rate: u32,
    slot: InsertSlot,
}

/// A caller waits this long at most for the plugin thread. A plugin that
/// hangs in a call keeps the thread, and the caller goes on with no plugin.
const WAIT_LIMIT: Duration = Duration::from_secs(120);
/// The host looks again for a preset file it did not find after this time.
const PRESET_RETRY: Duration = Duration::from_secs(2);
/// The preset that plays when a score names no preset: the file
/// `default.vstpreset` in the preset folder of the plugin.
const DEFAULT_PRESET: &str = "default";

/// A loaded plugin: its class and its parameters.
pub struct Plugin {
    module: Arc<Module>,
    info: ClassInfo,
    class: Scanned,
    /// Shared with each copy of the plugin list, so a list costs no copy
    /// of the parameters.
    params: Arc<[ParamInfo]>,
    keys: HashMap<String, u32>,
    /// The copies of the plugin that are prepared or run now.
    running: Arc<AtomicUsize>,
    /// The time the first load took, the load test in a child process
    /// included.
    load_time: Duration,
}

impl Plugin {
    fn new(module: Arc<Module>, info: ClassInfo, params: Vec<ParamInfo>) -> Self {
        let keys = params
            .iter()
            .map(|param| (param.key.clone(), param.id))
            .collect();
        let class = scanned(&info, None);
        Self {
            module,
            info,
            class,
            params: params.into(),
            keys,
            running: Arc::default(),
            load_time: Duration::ZERO,
        }
    }

    pub fn name(&self) -> &str {
        &self.class.name
    }

    /// The plugin as the scan cache keeps the plugin.
    pub(crate) fn scanned(&self) -> Scanned {
        self.class.clone()
    }

    pub fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    /// The number of a parameter from the name a score writes. A name of
    /// digits only is the number itself.
    pub fn param(&self, name: &str) -> Option<u32> {
        let key = canonical(name);
        self.keys.get(&key).copied().or_else(|| {
            let id = key.parse().ok()?;
            self.params.iter().any(|param| param.id == id).then_some(id)
        })
    }

    /// The values of a note for the audio engine. `values` has each value
    /// with the name a score writes. A note carries [`MAX_INSERT_PARAMS`]
    /// values at most. The note drops each value after these, and each
    /// value with a name the plugin does not have. The list has the reason
    /// for each dropped value, for the user.
    pub fn controls(
        &self,
        key: InsertKey,
        values: &[(&str, f64)],
    ) -> (InsertControls, Vec<String>) {
        let name = self.name();
        let mut controls = InsertControls::new(key);
        let mut dropped = Vec::new();
        for (param, value) in values {
            let Some(id) = self.param(param) else {
                dropped.push(format!("{name} has no parameter with the name '{param}'"));
                continue;
            };
            let value = *value as f32;
            if !controls.push(InsertParam { id, value }) {
                let most = MAX_INSERT_PARAMS;
                dropped.push(format!(
                    "a note carries {most} values for {name} at most, and '{param}' is one more"
                ));
            }
        }
        (controls, dropped)
    }

    /// True for a plugin that makes sound from notes.
    pub fn is_instrument(&self) -> bool {
        self.class.categories.contains("Instrument")
    }

    /// The call a score writes for the plugin: `vsti` for an instrument,
    /// `vst` for an effect.
    fn call(&self) -> &'static str {
        if self.is_instrument() { "vsti" } else { "vst" }
    }
}

/// How far a plugin is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The bundle is in a folder. The host did not load the plugin yet.
    Found,
    Loading,
    Ready,
    Failed(String),
}

/// One row of the plugin list.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInfo {
    /// The name of the plugin, or the name of the bundle before its load
    /// or its test.
    pub name: String,
    pub bundle: PathBuf,
    pub vendor: String,
    /// For example "Fx|Reverb". Empty before the load or the test of the
    /// bundle: the kind of the plugin is not known then.
    pub categories: String,
    pub status: Status,
    pub params: Arc<[ParamInfo]>,
    /// True for a plugin that makes sound from notes: a score uses
    /// `.vsti()` for it.
    pub instrument: bool,
    /// The copies of the plugin that are prepared or run now.
    pub running: usize,
    /// The time the first load took.
    pub load_time: Duration,
    /// True when the copies of the plugin stay in the audio callback: see
    /// [`Host::set_audio_thread_plugins`].
    pub audio_thread: bool,
    /// The largest latency of the copies of the plugin, in frames. The last
    /// copy of each preset at each sample rate counts: see
    /// [`Host::latency`]. `None` before the first copy since the load.
    pub latency: Option<u32>,
    /// How many preset files the preset folders hold for the plugin, by the
    /// class id in the header of each file. The count grows when the read
    /// of the preset folders ends: see [`Host::set_preset_folders`].
    pub presets: usize,
}

/// The answer to a preset name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoundPreset {
    /// The number of the preset for the audio engine.
    Number(u32),
    /// The plugin thread reads the preset folder now. Ask again later.
    Pending,
    /// The plugin has no preset file with this name.
    Missing,
}

/// How far the plugin of one slot is, for a caller that holds a start
/// until its plugins are ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prepared {
    /// Built. The audio engine takes the plugin with the next
    /// [`Host::insert`], or an engine holds the plugin of the slot now.
    Ready(InsertKey),
    /// An engine gave the copy of the slot back, at any rate, and the
    /// plugin thread takes it home. The copy is ready with no build: see
    /// [`Host::insert`]. A copy that still sounds restarts and runs a short
    /// silence before it is back.
    Returning,
    /// Loaded, and no build waits for the slot: no caller asked for the
    /// build yet, or the engine that took the plugin ended it.
    Unbuilt(InsertKey),
    /// The read of the folders, the load, the preset read or the build
    /// runs now.
    Pending,
    /// No such plugin or preset, the wrong kind, or a failure: nothing to
    /// wait for. The text is the reason, for the user.
    Unavailable(String),
}

/// A plugin copy a chain wants, live or at rest: the slot of the copy, and
/// the plugin and the preset by name, as a score writes them. See
/// [`Host::keep`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WantedCopy {
    pub slot: InsertSlot,
    pub name: String,
    pub preset: Option<String>,
}

/// The answer to a plugin name.
pub enum Resolved {
    /// The number of the plugin for the audio engine, and the plugin.
    Ready(u32, Arc<Plugin>),
    /// The plugin loads now. Ask again later.
    Pending,
    /// No bundle has this name.
    Missing,
    Failed(String),
}

enum Stage {
    Found,
    Loading,
    Ready(Arc<Plugin>),
    Failed(String),
}

struct Entry {
    /// The class name, or the bundle name before the load or the test.
    name: String,
    /// From the load or the test. Empty before.
    vendor: String,
    categories: String,
    /// The class id, from the load or the test: see [`Scanned::class`].
    class: String,
    bundle: PathBuf,
    /// The compare forms of the name and of the bundle name. A note asks
    /// for a plugin by name, so the lookup does not build them each time.
    key: String,
    stem: String,
    /// False after a scan that did not find the bundle again.
    present: bool,
    stage: Stage,
}

impl Entry {
    fn new(name: String, bundle: PathBuf, stage: Stage) -> Self {
        let stem = bundle.file_stem().unwrap_or_default().to_string_lossy();
        Self {
            key: canonical(&name),
            stem: canonical(&stem),
            name,
            vendor: String::new(),
            categories: String::new(),
            class: String::new(),
            bundle,
            present: true,
            stage,
        }
    }
}

/// A preset name a score asked for: the number of the preset, or the time
/// the host last found no such file.
enum KnownPreset {
    Number(u32),
    MissingSince(Instant),
}

enum Build {
    Running,
    /// A plugin copy the host keeps for a slot.
    Ready(Box<Instance>),
    /// The count of the builds at the failure, and the reason for the user.
    Failed(u64, String),
}

/// The place of a build in the order the bound of the parked copies ends
/// builds: the lowest rank ends first. See [`State::make_room`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// A failed build, by the count of the builds at the failure.
    Failed(u64),
    /// A copy of a chain no caller names, by the order of the copy.
    Unnamed(u64),
    /// A copy with copies at rest that ask for its key, by the highest rest
    /// order among them, then by the order of the copy: see [`Host::keep`].
    Rest(u64, u64),
}

#[derive(Default)]
struct State {
    /// The number of a plugin is its place here plus 1. Rows are only added.
    entries: Vec<Entry>,
    /// The number of a preset is its place here plus 1.
    presets: Vec<(u32, PathBuf)>,
    /// The answers to the preset names the scores asked for, by plugin
    /// number and compare form of the name.
    known_presets: HashMap<(u32, String), KnownPreset>,
    /// The preset files of each plugin, by the lower case class id each
    /// file names: see [`preset::scan_folders`].
    preset_index: HashMap<String, Vec<(String, PathBuf)>>,
    /// True once the preset folders were read. A read in the line of the
    /// plugin thread turns it back: see [`Host::read_presets`].
    presets_read: bool,
    /// A read of the preset folders is in the line of the plugin thread.
    presets_reading: bool,
    /// The folders changed while a read ran: one more read follows.
    presets_again: bool,
    /// The folders with `.vstpreset` files the player added, in any
    /// structure, read after the preset folder.
    preset_extra: Vec<PathBuf>,
    /// The word to the watch of the preset folders: see
    /// [`Host::arm_preset_watch`]. A channel that reads as gone ended its
    /// watch, and the folders are read when the player asks again.
    preset_watch: Option<Sender<Word>>,
    /// The folders the watch of the preset folders was asked for, also when
    /// a missing folder made it watch the folder nearest it that is there:
    /// see [`Host::arm_preset_watch`].
    preset_watched: Vec<PathBuf>,
    /// The reads of the preset folders so far. A caller that holds the
    /// number of its last read asks again when this moves: the watch read
    /// the folders, and its answer may have changed.
    presets_version: u64,
    builds: HashMap<(InsertKey, u32, InsertSlot), Build>,
    /// The builds a render claimed and did not take yet: see
    /// [`Host::insert`]. The bound of the parked copies does not count such a
    /// build and does not end it.
    rendering: HashSet<(InsertKey, u32, InsertSlot)>,
    /// The builds an engine took for live play, with the cell of each copy:
    /// the cell says while the engine holds the plugin, and while the copy
    /// comes home. See [`Host::insert`].
    taken: HashMap<(InsertKey, u32, InsertSlot), Arc<CopyState>>,
    /// The copies that waited for their slot at another rate, by the build
    /// at the new rate: the build sets the copy up at its rate. `None` while
    /// the build works on the copy. See [`Host::claim`].
    retunes: HashMap<(InsertKey, u32, InsertSlot), Option<Box<Instance>>>,
    /// The latency of the last copy of each key at each rate, in frames:
    /// see [`Host::latency`].
    latencies: HashMap<(InsertKey, u32), u32>,
    /// The prepared plugins with a build in the line behind a load or a
    /// read of the preset folder: see [`Host::prepare`].
    preparing: Vec<Prepare>,
    /// The chains that ended: see [`Host::keep`]. No chain number comes a
    /// second time, so a chain here gets no new build. An engine holds the
    /// copy of such a chain until the end of its tail.
    ended: HashSet<u64>,
    /// The ready copies the end job of [`Host::keep`] ends: see
    /// [`Host::end_unwanted`]. `None` while no end job is in the line of the
    /// plugin thread or runs, so one end job at most is in the line. A later
    /// call adds its copies to that job.
    ending: Option<Vec<(InsertKey, u32, InsertSlot)>>,
    /// What each caller of [`Host::keep`] keeps, by the number of the
    /// caller.
    callers: HashMap<u64, Kept>,
    /// The compare forms of the plugin names whose copies stay in the audio
    /// callback: see [`Host::set_audio_thread_plugins`].
    audio_thread: HashSet<String>,
    preset_root: Option<PathBuf>,
    worker: Option<WorkerProgram>,
    /// The completed builds so far. The count orders copies and failures.
    built: u64,
    /// The reads of the folders in the line of the plugin thread.
    scans: usize,
    errors: Vec<String>,
    /// The plugins of each bundle that loads, from the cache file. The
    /// first read of the folders reads the file.
    cache: Cache,
    cache_read: bool,
    /// The bundles with their plugins by name, each with the stamp of its
    /// files at that time: from the cache, from the scan or from a load.
    scanned: HashMap<PathBuf, cache::Stamp>,
    scan: Scan,
}

/// The plugin copies and the chains of one caller of [`Host::keep`].
#[derive(Default)]
struct Kept {
    /// The copies each live chain of the caller wants, by chain.
    wanted: HashMap<u64, Vec<WantedCopy>>,
    /// The chains the caller names.
    named: HashSet<u64>,
    /// The copies the caller keeps at rest, by chain, each with the rest
    /// order of its score.
    rest: HashMap<u64, Vec<(WantedCopy, u64)>>,
}

/// The read of each bundle with no plugin names yet, on a thread of its
/// own.
#[derive(Default)]
struct Scan {
    /// A caller asked for the scan: see [`Host::scan_plugins`].
    wanted: bool,
    /// The scan thread runs.
    running: bool,
    /// The bundles the thread read since its start.
    done: usize,
    /// The bundle in its read now, and the class of the bundle in its load.
    bundle: Option<PathBuf>,
    class: Option<String>,
}

/// Where the scan is, while it runs: see [`Host::scan_progress`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanProgress {
    /// The bundles the scan read.
    pub read: usize,
    /// The bundles of the scan in all.
    pub bundles: usize,
    /// The plugin in its test now. The name of the bundle goes first when
    /// the two names are not alike, as for a shell of many plugins:
    /// `Shell › Comp`.
    pub plugin: String,
}

/// The name of the plugin in its test: see [`ScanProgress::plugin`].
fn name_in_test(bundle: &Path, class: Option<&str>) -> String {
    let stem = bundle.file_stem().unwrap_or_default().to_string_lossy();
    let Some(class) = class.filter(|class| !canonical(class).is_empty()) else {
        return stem.into_owned();
    };
    let (outer, inner) = (canonical(&stem), canonical(class));
    if outer.contains(&inner) || inner.contains(&outer) {
        class.to_owned()
    } else {
        format!("{stem} › {class}")
    }
}

impl State {
    /// The bundles a scan reads: in a folder, with no plugin names for the
    /// files they have now, and not in a load.
    fn unscanned(&self) -> Vec<PathBuf> {
        let mut bundles: Vec<PathBuf> = Vec::new();
        for entry in &self.entries {
            let waits = entry.present
                && matches!(entry.stage, Stage::Found)
                && !self.scanned.contains_key(&entry.bundle);
            if waits && !bundles.contains(&entry.bundle) {
                bundles.push(entry.bundle.clone());
            }
        }
        bundles
    }

    /// The number of the plugin of a row for the audio engine.
    fn number(row: usize) -> u32 {
        row as u32 + 1
    }

    /// Gives the rows of a bundle the names of its plugins. A plugin keeps
    /// the row with its name, and so its number. A plugin with no row takes
    /// a row of the bundle with no plugin, such as the row with the bundle
    /// name, or a new row.
    fn name_rows(&mut self, bundle: &Path, plugins: &[Scanned]) {
        let keys: Vec<String> = plugins
            .iter()
            .map(|plugin| canonical(&plugin.name))
            .collect();
        let rows = |entries: &[Entry]| -> Vec<usize> {
            let of_bundle = |(_, entry): &(usize, &Entry)| entry.bundle == bundle;
            let rows = entries.iter().enumerate().filter(of_bundle);
            rows.map(|(row, _)| row).collect()
        };
        let present = rows(&self.entries)
            .first()
            .is_none_or(|row| self.entries[*row].present);
        // A loaded plugin keeps its row and its name.
        let free =
            |entry: &Entry| !keys.contains(&entry.key) && !matches!(entry.stage, Stage::Ready(_));
        let mut spare: Vec<usize> = rows(&self.entries)
            .into_iter()
            .filter(|row| free(&self.entries[*row]))
            .rev()
            .collect();
        for (plugin, key) in plugins.iter().zip(keys) {
            let named = rows(&self.entries)
                .into_iter()
                .find(|row| self.entries[*row].key == key);
            let row = named.or_else(|| spare.pop()).unwrap_or_else(|| {
                let name = plugin.name.clone();
                self.entries.push(Entry {
                    present,
                    ..Entry::new(name, bundle.to_path_buf(), Stage::Found)
                });
                self.entries.len() - 1
            });
            if self.entries[row].key != key {
                // The row is the row of a different plugin now: the preset
                // answers of the old one go.
                self.known_presets
                    .retain(|(plugin, _), _| row_of(*plugin) != Some(row));
            }
            let entry = &mut self.entries[row];
            entry.name = plugin.name.clone();
            entry.key = key;
            entry.vendor = plugin.vendor.clone();
            entry.categories = plugin.categories.clone();
            entry.class = plugin.class.clone();
            // A plugin that did not load stays off, as a bundle does, until
            // its files change or a rescan.
            if let Some(fault) = &plugin.fault
                && matches!(entry.stage, Stage::Found | Stage::Loading)
            {
                entry.stage = Stage::Failed(fault.clone());
            }
        }
    }

    /// True for a build for a slot of a chain that ended.
    fn chain_ended(&self, build: &(InsertKey, u32, InsertSlot)) -> bool {
        self.ended.contains(&build.2.chain)
    }

    /// True for a build for a slot of a listed chain with a key no copy of
    /// the slot asks for, live or at rest: see [`Host::keep`]. A chain no
    /// caller lists asks for each key.
    fn unwanted(&self, build: &(InsertKey, u32, InsertSlot)) -> bool {
        let (key, _, slot) = build;
        let lists = |kept: &&Kept| {
            kept.wanted.contains_key(&slot.chain) || kept.rest.contains_key(&slot.chain)
        };
        let Some(kept) = self.callers.values().find(lists) else {
            return false;
        };
        let live = kept.wanted.get(&slot.chain).into_iter().flatten();
        let rest = kept.rest.get(&slot.chain).into_iter().flatten();
        let asks = |copy: &WantedCopy| copy.slot == *slot && self.asks_for(copy, *key);
        !live.chain(rest.map(|(copy, _)| copy)).any(asks)
    }

    /// True when `copy` asks for the copy with `key`. The name and the
    /// preset resolve as in [`Host::key`], with no load and no read of a
    /// folder. A name with no answer yet asks for each key: the host reads
    /// the folders, the plugin loads, or no call asked for the preset yet.
    fn asks_for(&self, copy: &WantedCopy, key: InsertKey) -> bool {
        self.asks_for_row(self.find(&copy.name), copy, key)
    }

    /// [`State::asks_for`] with `row`, the row [`State::find`] gives for the
    /// name of `copy`.
    fn asks_for_row(&self, row: Option<usize>, copy: &WantedCopy, key: InsertKey) -> bool {
        let Some(row) = row else {
            return self.scans > 0;
        };
        match &self.entries[row].stage {
            Stage::Ready(_) => {}
            Stage::Found | Stage::Loading => return true,
            Stage::Failed(_) => return false,
        }
        let plugin = State::number(row);
        let preset = copy.preset.as_deref().unwrap_or(DEFAULT_PRESET);
        let preset = match self.known_presets.get(&(plugin, canonical(preset))) {
            Some(KnownPreset::Number(number)) => *number,
            // With no file `default`, a call with no preset plays the state
            // the plugin starts in. A call with a missing preset gets no
            // copy.
            Some(KnownPreset::MissingSince(_)) if copy.preset.is_none() => 0,
            Some(KnownPreset::MissingSince(_)) => return false,
            // A call with no preset plays the state the plugin starts in,
            // and no word yet says otherwise: it asks for that state, not
            // for each key - a read of the folders that lands between the
            // word of a turn and a prepare would else build a copy of any
            // preset for the slot.
            None if copy.preset.is_none() => {
                return key == InsertKey { plugin, preset: 0 };
            }
            None => return true,
        };
        key == InsertKey { plugin, preset }
    }

    /// True when a caller of [`Host::keep`] names the chain.
    fn named(&self, chain: u64) -> bool {
        self.callers
            .values()
            .any(|kept| kept.named.contains(&chain))
    }

    /// The row [`State::find`] gives for each name [`State::rank`] reads: the
    /// name of each copy at rest, and of each live copy of a chain with
    /// copies at rest. [`State::make_room`] finds each name one time, and not
    /// one time for each build it ranks.
    fn rank_rows(&self) -> HashMap<String, Option<usize>> {
        let mut rows = HashMap::new();
        let lists = || self.callers.values();
        for (chain, rest) in lists().flat_map(|kept| &kept.rest) {
            let live = lists().filter_map(|kept| kept.wanted.get(chain)).flatten();
            for copy in rest.iter().map(|(copy, _)| copy).chain(live) {
                rows.entry(copy.name.clone())
                    .or_insert_with(|| self.find(&copy.name));
            }
        }
        rows
    }

    /// The rank of a ready copy of `build` with the order `order` at the
    /// bound of the parked copies: see [`State::make_room`]. A ready copy
    /// of a chain a caller names has no rank and stays, unless only copies
    /// at rest ask for its key. When copies at rest ask for the key of a
    /// ready copy, the copy ranks by the highest rest order among them.
    /// Each other ready copy ranks under each such copy. `rows` has the row
    /// of each name from [`State::rank_rows`]. A name not in `rows` resolves
    /// here.
    fn rank(
        &self,
        build: &(InsertKey, u32, InsertSlot),
        order: u64,
        rows: &HashMap<String, Option<usize>>,
    ) -> Option<Rank> {
        let (key, _, slot) = build;
        let row = |copy: &WantedCopy| {
            let found = rows.get(&copy.name).copied();
            found.unwrap_or_else(|| self.find(&copy.name))
        };
        let asks =
            |copy: &WantedCopy| copy.slot == *slot && self.asks_for_row(row(copy), copy, *key);
        let lists = || self.callers.values();
        let rest = lists().filter_map(|kept| kept.rest.get(&slot.chain));
        let rest = rest.flatten().filter(|(copy, _)| asks(copy));
        let rest = rest.map(|(_, rest)| *rest).max();
        let named = self.named(slot.chain);
        let live = || {
            let live = lists().filter_map(|kept| kept.wanted.get(&slot.chain));
            live.flatten().any(asks)
        };
        match rest {
            Some(rest) if !named || !live() => Some(Rank::Rest(rest, order)),
            _ if named => None,
            _ => Some(Rank::Unnamed(order)),
        }
    }

    /// The builds the bound of the parked copies counts: each build but a
    /// build a render did not take yet. See [`State::rendering`].
    fn parked(&self) -> usize {
        let rendering = self.rendering.iter();
        let rendering = rendering.filter(|build| self.builds.contains_key(build));
        self.builds.len() - rendering.count()
    }

    /// Ends the build of the lowest rank under `below` until the bound of
    /// the parked copies has room for one more build: see [`MAX_PARKED`].
    /// With no `below`, a build of any rank ends. A build that runs has no
    /// rank, and a copy of a named chain has none: see [`State::rank`]. A
    /// build a render did not take yet has no rank and does not count: see
    /// [`State::rendering`]. False when the bound has no room and no build
    /// ranks under `below`. The names of the copies resolve one time for the
    /// whole call: see [`State::rank_rows`].
    fn make_room(&mut self, below: Option<Rank>) -> bool {
        if self.parked() < MAX_PARKED {
            return true;
        }
        let rows = self.rank_rows();
        while self.parked() >= MAX_PARKED {
            let ranks = self.builds.iter().filter_map(|(build, built)| {
                let rank = match built {
                    _ if self.rendering.contains(build) => None,
                    Build::Failed(order, _) => Some(Rank::Failed(*order)),
                    Build::Ready(copy) => self.rank(build, copy.order(), &rows),
                    Build::Running => None,
                };
                let under = rank.filter(|rank| below.is_none_or(|below| *rank < below));
                under.map(|rank| (*build, rank))
            });
            let Some((lowest, _)) = ranks.min_by_key(|(_, rank)| *rank) else {
                return false;
            };
            self.builds.remove(&lowest);
        }
        true
    }

    /// True when a copy an engine gave back ends at its park: the chain of
    /// its slot ended, its slot does not want its key, or it has the preset
    /// mark: see [`CopyState::preset_changed`].
    fn ends_at_park(&self, build: &(InsertKey, u32, InsertSlot), copy: &Instance) -> bool {
        self.chain_ended(build)
            || self.unwanted(build)
            || copy.cell().preset_changed.load(Ordering::Relaxed)
    }

    /// The park of a copy an engine gave back, in one lock: see
    /// [`Host::park`]. The copy leaves the taken builds, and waits for its
    /// slot again at the rate of its build. At the bound of the parked
    /// copies it ends a build of a lower rank only, and the order of the
    /// copies does not count: a copy at rest ends a failed build, a copy of
    /// a chain no caller names or a copy at rest of a lower rest order. A
    /// copy of a chain no caller names ends a failed build only. A copy of
    /// a named chain has no rank: it ends a build of any rank, and parks
    /// past the bound when no build ranks. See [`State::make_room`]. Gives
    /// the copy back when it ends at its park or finds no room: the caller
    /// ends it on the plugin thread. A copy the bound ends leaves the list
    /// here, and its plugin ends on the plugin thread too.
    fn park(
        &mut self,
        build: (InsertKey, u32, InsertSlot),
        copy: Box<Instance>,
    ) -> Option<Box<Instance>> {
        self.taken.remove(&build);
        if self.ends_at_park(&build, &copy) {
            return Some(copy);
        }
        // With the order 0, no build of the kind and the rest order of the
        // copy ranks under it.
        let below = self.rank(&build, 0, &HashMap::new());
        if !self.make_room(below) && below.is_some() {
            return Some(copy);
        }
        copy.cell().set_place(Place::Home);
        self.latencies.insert((build.0, build.1), copy.latency());
        self.builds.insert(build, Build::Ready(copy));
        None
    }

    /// Takes the ready copies of the builds that `ends` names out of the
    /// list. The caller ends them on the plugin thread.
    // A copy stays in the box an engine takes.
    #[allow(clippy::vec_box)]
    fn take_ready(
        &mut self,
        ends: impl Fn(&(InsertKey, u32, InsertSlot)) -> bool,
    ) -> Vec<Box<Instance>> {
        let ready = |(build, built): (&(InsertKey, u32, InsertSlot), &Build)| {
            (matches!(built, Build::Ready(_)) && ends(build)).then_some(*build)
        };
        let builds: Vec<_> = self.builds.iter().filter_map(ready).collect();
        let copy = |build| match self.builds.remove(&build) {
            Some(Build::Ready(copy)) => Some(copy),
            _ => None,
        };
        builds.into_iter().filter_map(copy).collect()
    }

    /// True while an engine holds a copy of `key` for `slot` or gives one
    /// back, at any rate, and while a copy of `key` for `slot` takes a new
    /// rate: see [`Host::claim`].
    fn held(&self, key: InsertKey, slot: InsertSlot) -> bool {
        let of = |build: &(InsertKey, u32, InsertSlot)| (build.0, build.2) == (key, slot);
        let lent = |(build, cell): (_, &Arc<CopyState>)| of(build) && cell.place() != Place::Home;
        self.taken.iter().any(lent) || self.retunes.keys().any(of)
    }

    /// True while an engine holds the plugin it took for this build.
    fn in_engine(&self, build: &(InsertKey, u32, InsertSlot)) -> bool {
        let held = |cell: &Arc<CopyState>| cell.place() == Place::Engine;
        self.taken.get(build).is_some_and(held)
    }

    /// True while a copy of `key` for `slot` comes home from an engine, at
    /// any rate: see [`Host::park`].
    fn returning(&self, key: InsertKey, slot: InsertSlot) -> bool {
        self.taken.iter().any(|(build, cell)| {
            (build.0, build.2) == (key, slot) && cell.place() == Place::Returning
        })
    }

    /// The preset file of the copies of `key`. A copy of preset 0 plays the
    /// state the plugin starts in and reads no file.
    fn preset_file(&self, key: InsertKey) -> Option<&Path> {
        let place = (key.preset as usize).checked_sub(1)?;
        self.presets.get(place).map(|(_, file)| file.as_path())
    }

    /// The cell of each copy the host knows, with its key: each copy that
    /// waits for its slot, each copy an engine holds or gives back, and
    /// each copy that waits for its retune. The build that sets a copy up at
    /// its new rate stamps the preset file itself: see [`Host::finish`].
    fn cells(&self) -> impl Iterator<Item = (InsertKey, &CopyState)> {
        let ready = self.builds.iter().filter_map(|(build, built)| match built {
            Build::Ready(copy) => Some((build.0, copy.cell())),
            _ => None,
        });
        let lent = self
            .taken
            .iter()
            .filter(|(_, cell)| cell.place() != Place::Home);
        let lent = lent.map(|(build, cell)| (build.0, &**cell));
        let retunes = self.retunes.iter();
        let retunes = retunes.filter_map(|(build, copy)| Some((build.0, copy.as_ref()?.cell())));
        ready.chain(lent).chain(retunes)
    }

    /// The copies with a preset file that changed or went since their
    /// build, at the end of a read of the preset folders. `stamps` has the
    /// stamp of each file now: see [`Host::stamp_preset_files`]. Each copy
    /// that waits for its slot leaves the list: the caller ends it after the
    /// lock, and the next note of the slot builds a copy from the file. Each
    /// other such copy gets the preset mark: see [`CopyState::preset_changed`].
    /// A copy an engine holds keeps its state while it plays.
    #[allow(clippy::vec_box, reason = "a copy stays in the box an engine takes")]
    fn stale_copies(&mut self, stamps: &HashMap<PathBuf, u64>) -> Vec<Box<Instance>> {
        if stamps.is_empty() {
            return Vec::new();
        }
        let stale = |key, cell: &CopyState| {
            let stamp = self.preset_file(key).and_then(|file| stamps.get(file));
            stamp.is_some_and(|stamp| *stamp != cell.preset.load(Ordering::Relaxed))
        };
        let mut gone = HashSet::new();
        for (build, built) in &self.builds {
            if let Build::Ready(copy) = built
                && stale(build.0, copy.cell())
            {
                gone.insert(*build);
            }
        }
        for (key, cell) in self.cells() {
            if stale(key, cell) {
                cell.preset_changed.store(true, Ordering::Relaxed);
            }
        }
        self.take_ready(|build| gone.contains(build))
    }

    /// True when the copies of the plugin of a number stay in the audio
    /// callback: see [`Host::set_audio_thread_plugins`].
    fn on_audio_thread(&self, plugin: u32) -> bool {
        let entry = row_of(plugin).and_then(|row| self.entries.get(row));
        entry.is_some_and(|entry| self.audio_thread.contains(&entry.key))
    }

    fn find(&self, name: &str) -> Option<usize> {
        let wanted = canonical(name);
        if wanted.is_empty() {
            return None;
        }
        if let Some(index) = self.find_whole(&wanted) {
            return Some(index);
        }
        let present = || {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.present)
        };
        // A part of a name finds the plugin with the shortest name that
        // has the part: first as whole words, then at the start of a word,
        // then anywhere. "tal" finds TAL-NoiseMaker before Talk Box and
        // Vital.
        let words = |entry: &Entry| {
            let stem = entry.bundle.file_stem().unwrap_or_default();
            let stem = word_match(&stem.to_string_lossy(), &entry.stem, &wanted);
            word_match(&entry.name, &entry.key, &wanted).max(stem)
        };
        let anywhere = |entry: &Entry| entry.key.contains(&wanted) || entry.stem.contains(&wanted);
        let shortest = |has: &dyn Fn(&Entry) -> bool| {
            present()
                .filter(|(_, entry)| has(entry))
                .min_by_key(|(_, entry)| entry.key.len())
        };
        let partial = shortest(&|entry| words(entry) == 2)
            .or_else(|| shortest(&|entry| words(entry) == 1))
            .or_else(|| shortest(&anywhere));
        if let Some((index, _)) = partial {
            return Some(index);
        }
        // A bundle can hold a plugin with a longer name than the bundle.
        // Such a name is known only after the load, so a bundle not yet
        // loaded is a candidate when its name is a part of the name wanted.
        present()
            .filter(|(_, entry)| matches!(entry.stage, Stage::Found | Stage::Loading))
            .filter(|(_, entry)| !entry.stem.is_empty() && wanted.contains(&entry.stem))
            .max_by_key(|(_, entry)| entry.stem.len())
            .map(|(index, _)| index)
    }

    /// The plugin with `wanted` as its name, else as its bundle name, both
    /// in compare form. A part of a name finds no plugin here.
    fn find_whole(&self, wanted: &str) -> Option<usize> {
        if wanted.is_empty() {
            return None;
        }
        let present = || {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.present)
        };
        let found = present().find(|(_, entry)| entry.key == wanted);
        let found = found.or_else(|| present().find(|(_, entry)| entry.stem == wanted));
        found.map(|(index, _)| index)
    }
}

/// Where `wanted` is in `name`: 2 when it covers whole words, as "tal" in
/// "TAL-NoiseMaker", 1 when it starts a word, as "tal" in "Talk Box",
/// and 0 for no part at the start of a word, as "tal" in "Vital". A word
/// starts after a space or a sign, at a capital after a small letter, and
/// at a digit after a letter. `key` is the compare form of `name`, and
/// `wanted` is in compare form too.
fn word_match(name: &str, key: &str, wanted: &str) -> u8 {
    let mut rank = 0;
    // The ends of the parts found so far: a part that ends where a word
    // starts covers whole words.
    let mut ends: Vec<usize> = Vec::new();
    let mut at = 0;
    let mut before: Option<char> = None;
    for char in name.chars() {
        if char.is_alphanumeric() {
            let head = before.is_none_or(|before| {
                !before.is_alphanumeric()
                    || (before.is_lowercase() && char.is_uppercase())
                    || (before.is_alphabetic() && char.is_numeric())
            });
            if head {
                if ends.contains(&at) {
                    return 2;
                }
                if key.get(at..).is_some_and(|rest| rest.starts_with(wanted)) {
                    rank = 1;
                    ends.push(at + wanted.len());
                }
            }
            at += char.to_lowercase().map(char::len_utf8).sum::<usize>();
        }
        before = Some(char);
    }
    if ends.contains(&at) { 2 } else { rank }
}

/// A plugin name as a folder name: a sign a file system refuses becomes
/// `-`, and the name ends with no dot or space.
fn folder_name(name: &str) -> String {
    let safe = |char: char| {
        if char.is_control() || r#"/\:*?"<>|"#.contains(char) {
            '-'
        } else {
            char
        }
    };
    let name: String = name.chars().map(safe).collect();
    name.trim_end_matches(['.', ' ']).to_owned()
}

/// The folder of a plugin under `root`: the folder with the name of the
/// plugin in compare form.
fn preset_folder(root: &Path, plugin: &str) -> Option<PathBuf> {
    let wanted = canonical(plugin);
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| canonical(&name.to_string_lossy()) == wanted)
        })
}

struct Shared {
    jobs: Sender<Job>,
    state: Mutex<State>,
    /// Set by [`Host::shutdown`]: the line takes no new job.
    closed: AtomicBool,
    /// Held for the check of `closed` and the send of a job as one step,
    /// so no job joins the line behind the last job of a shutdown.
    gate: Mutex<()>,
    /// One thread at a time writes the cache file.
    saving: Mutex<()>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // The plugin thread ends each plugin and unloads each library.
        let state = self.state.get_mut();
        let state = std::mem::take(state.unwrap_or_else(|error| error.into_inner()));
        let _ = self.jobs.send(Box::new(move || drop(state)));
    }
}

/// Takes a lock of the host. A panic of a thread with the lock does not end
/// the host: the next caller reads the value as that thread left it.
fn locked<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|error| error.into_inner())
}

/// The plugin host. A clone is a second handle to the same host.
#[derive(Clone)]
pub struct Host {
    shared: Arc<Shared>,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// Starts the plugin thread. The host loads no plugin until asked.
    pub fn new() -> Self {
        let (jobs, work) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("rustel-vst3".into())
            .spawn(move || {
                // The thread ends when the host and every plugin are gone.
                for job in work {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            })
            .expect("plugin thread");
        Self {
            shared: Arc::new(Shared {
                jobs,
                state: Mutex::new(State::default()),
                closed: AtomicBool::new(false),
                gate: Mutex::new(()),
                saving: Mutex::new(()),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        locked(&self.shared.state)
    }

    /// Puts a job in the line of the plugin thread. The receiver gets a
    /// value when the job is done. After [`Host::shutdown`] the job does
    /// not run, and the receiver reads as disconnected.
    fn enqueue(&self, job: impl FnOnce() + Send + 'static) -> mpsc::Receiver<()> {
        let (done, finished) = mpsc::sync_channel::<()>(1);
        let _gate = locked(&self.shared.gate);
        if !self.shared.closed.load(Ordering::Relaxed) {
            let _ = self.shared.jobs.send(Box::new(move || {
                job();
                let _ = done.send(());
            }));
        }
        finished
    }

    /// Ends the work of the host before the process ends. The line takes
    /// no new job, so a job in the line now adds no work behind this one.
    /// Each plugin with no running copy unloads, the scan ends its load
    /// test, and the call waits for the two threads, `limit` at most.
    pub fn shutdown(&self, limit: Duration) {
        let start = Instant::now();
        let (done, finished) = mpsc::sync_channel::<()>(1);
        let host = self.clone();
        {
            let _gate = locked(&self.shared.gate);
            self.shared.closed.store(true, Ordering::Relaxed);
            let _ = self.shared.jobs.send(Box::new(move || {
                host.finish_unload(&[]);
                let _ = done.send(());
            }));
        }
        let _ = finished.recv_timeout(limit);
        // The scan thread ends the test process it waits for. With no wait
        // here, that process stays after this one.
        while self.state().scan.running && start.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits for a job of the plugin thread, for [`WAIT_LIMIT`] at most: a
    /// plugin that hangs must not hang its caller for good.
    fn wait_for(finished: mpsc::Receiver<()>) {
        let _ = finished.recv_timeout(WAIT_LIMIT);
    }

    /// Sets the program that loads a bundle in a process of its own for
    /// the scan: see [`WorkerProgram`].
    pub fn set_worker(&self, program: Option<WorkerProgram>) {
        self.state().worker = program;
    }

    /// The folder with one folder of `.vstpreset` files for each plugin.
    /// The next ask for a preset reads the preset folders again - a read
    /// that runs now reads them after its end, with the new folder.
    pub fn set_preset_folder(&self, folder: PathBuf) {
        {
            let mut state = self.state();
            state.preset_root = Some(folder);
            state.preset_index.clear();
            state.presets_read = false;
            if state.presets_reading {
                state.presets_again = true;
            }
        }
        self.arm_preset_watch();
    }

    /// The folders with `.vstpreset` files the player added, in any folder
    /// structure: the header of each file says its plugin. The read of the
    /// preset folders runs again, on the plugin thread.
    pub fn set_preset_folders(&self, folders: Vec<PathBuf>) {
        {
            let mut state = self.state();
            state.preset_extra = folders;
        }
        self.read_presets(true);
    }

    /// Watches the preset folders: the root, and the folders the player
    /// added. The events of the file system say when a preset comes in or
    /// goes out, and the read of the folders follows each quiet behind
    /// them - no read of the disk with no change, and no ask of the
    /// player. Each read ends here: a folder that came in takes its watch
    /// of its own, and the watch of the folders before ends with its word.
    /// The watch that is on stays on when the folders are the same - a new
    /// watch takes a moment before the events of the system reach it, and
    /// a change in that moment would be lost. The watch ends with the
    /// host.
    fn arm_preset_watch(&self) {
        let folders = {
            let mut state = self.state();
            let mut folders = state.preset_extra.clone();
            folders.extend(state.preset_root.iter().cloned());
            if state.preset_watch.is_some() && state.preset_watched == folders {
                return;
            }
            if let Some(watch) = state.preset_watch.take() {
                let _ = watch.send(Word::Stop);
            }
            folders
        };
        if folders.is_empty() {
            return;
        }
        let weak = Arc::downgrade(&self.shared);
        let alive = weak.clone();
        let Some(word) = crate::watch::watch_folders(
            folders.clone(),
            move || alive.upgrade().is_some(),
            move || {
                // The watch holds no handle to the host: the host may end
                // while it runs, and the read is then the least of the
                // worries. The weak handle says the host is here at all.
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                Host { shared }.read_presets(true);
            },
        ) else {
            return;
        };
        let mut state = self.state();
        state.preset_watched = folders;
        state.preset_watch = Some(word);
    }

    /// Takes the read of the preset folders for this caller, on the plugin
    /// thread or in line of it: the root, and the folders the player
    /// added. With `force`, a read runs again after one that runs now -
    /// the folders changed. `None` also when a read ran already and
    /// nothing asks for one more.
    fn claim_presets_read(&self, force: bool) -> Option<(Option<PathBuf>, Vec<PathBuf>)> {
        let mut state = self.state();
        if state.presets_reading {
            if force {
                state.presets_again = true;
            }
            return None;
        }
        if state.presets_read && !force {
            return None;
        }
        state.presets_reading = true;
        Some((state.preset_root.clone(), state.preset_extra.clone()))
    }

    /// Reads the folder of one plugin under the preset root again, in the
    /// job of its load: a preset file the player put there while the
    /// studio runs counts from the load. The folder is the one with the
    /// name of the plugin, for the store of drops and saves: the header of
    /// each file does not say its plugin here. The files of the other
    /// folders keep the answer of the last full read. A plugin of the bundle
    /// that was loaded before keeps its row and its copies over the load.
    /// So the read also ends each copy that waits for its slot with a file
    /// of this folder saved again or gone since its build, and marks each
    /// other such copy, as [`Host::finish_presets`] does: see
    /// [`State::stale_copies`].
    fn refresh_preset_folder(&self, plugin: &Scanned) {
        let root = self.state().preset_root.clone();
        let Some(root) = root else {
            return;
        };
        if plugin.class.is_empty() {
            return;
        }
        // The folder stays the folder of the plugin when it is gone: the
        // read then says no file, and the files the folder held leave.
        let folder = preset_folder(&root, &plugin.name)
            .unwrap_or_else(|| root.join(folder_name(&plugin.name)));
        let files = crate::preset::read_folder(&folder);
        let fresh: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
        // The read checks the copies of the files of this folder only.
        let stamps = self.stamp_preset_files(Some(folder.as_path()));
        let mut state = self.state();
        let list = state
            .preset_index
            .entry(plugin.class.to_ascii_lowercase())
            .or_default();
        // Only the files of this folder give way. A name of both this
        // folder and a folder the player added keeps the file of this
        // folder, as a read of the disk puts the root first.
        list.retain(|(known, path)| !path.starts_with(&folder) && !fresh.contains(known));
        list.extend(files);
        list.sort();
        // A file of the read can answer a preset that was missing before:
        // a kept miss does not hold its missing past the read.
        state
            .known_presets
            .retain(|_, known| matches!(known, KnownPreset::Number(_)));
        state.presets_version += 1;
        let stale = state.stale_copies(&stamps);
        drop(state);
        // A copy that waits for its slot keeps no state of a file saved
        // again: it ends here, in the job of the load.
        for copy in stale {
            copy.end_here();
        }
    }

    /// Reads the preset folders on the plugin thread: a folder on a slow
    /// disk does not hold the caller, and the asks keep the answer of the
    /// read before until the new read ends. No caller waits.
    fn read_presets(&self, force: bool) {
        if let Some((root, extra)) = self.claim_presets_read(force) {
            let host = self.clone();
            self.enqueue(move || host.finish_presets(root, extra));
        }
    }

    /// The read itself, on the plugin thread: in a job of the line, or in
    /// the job of a load, which a score asks for a preset right after. A
    /// read claimed while this one ran follows here at once. At its end the
    /// read ends each copy that waits for its slot with a preset file saved
    /// again or gone since its build, in the job of the read: see
    /// [`State::stale_copies`].
    ///
    /// The folders the player added hold any structure, so the header of
    /// each file names its plugin. The root is the store of rustel, with
    /// one folder for each plugin, so the folder names the plugin: a file
    /// of another plugin the player put there reaches the build, which
    /// refuses it with the reason. A name of the root and of an added
    /// folder keeps the file of the root.
    fn finish_presets(&self, root: Option<PathBuf>, extra: Vec<PathBuf>) {
        let mut index = crate::preset::scan_folders(&extra);
        let plugins = {
            let state = self.state();
            state
                .entries
                .iter()
                .filter(|entry| entry.present && !entry.class.is_empty())
                .map(|entry| (entry.name.clone(), entry.class.to_ascii_lowercase()))
                .collect::<Vec<_>>()
        };
        if let Some(root) = &root {
            for (name, class) in plugins {
                let Some(folder) = preset_folder(root, &name) else {
                    continue;
                };
                let files = crate::preset::read_folder(&folder);
                let fresh: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
                let list = index.entry(class).or_default();
                list.retain(|(known, _)| !fresh.contains(known));
                list.extend(files);
            }
        }
        for files in index.values_mut() {
            files.sort();
        }
        let stamps = self.stamp_preset_files(None);
        let (again, stale) = {
            let mut state = self.state();
            state.preset_index = index;
            // A file of the read can answer a preset that was missing
            // before: a kept miss does not hold its missing past the read.
            state
                .known_presets
                .retain(|_, known| matches!(known, KnownPreset::Number(_)));
            state.presets_version += 1;
            state.presets_read = true;
            state.presets_reading = false;
            let stale = state.stale_copies(&stamps);
            (std::mem::take(&mut state.presets_again), stale)
        };
        // A copy that waits for its slot keeps no state of a file saved
        // again: it ends here, in the job of the read.
        for copy in stale {
            copy.end_here();
        }
        if again && let Some((root, extra)) = self.claim_presets_read(true) {
            return self.finish_presets(root, extra);
        }
        // The folders as they are now take the watch of the next change:
        // a folder that came in has a watch of its own from here.
        self.arm_preset_watch();
    }

    /// Keeps each copy of the plugins in `names` in the audio callback, for
    /// a plugin that breaks on another thread: such a copy answers false to
    /// [`OrbitInsert::any_thread`]. A name is the whole name of a plugin, and
    /// compares in the form of [`canonical`]. A copy an engine takes after
    /// the call follows the list.
    pub fn set_audio_thread_plugins(&self, names: &[String]) {
        self.state().audio_thread = names.iter().map(|name| canonical(name)).collect();
    }

    /// Reads the folders again, on the plugin thread: a folder on a slow
    /// disk does not hold the caller. New bundles join the list. A plugin
    /// that failed gets a new try when its files changed, or when
    /// [`Host::rescan`] empties the cache. Plugin numbers stay as they are.
    /// [`Host::wait_idle`] returns after the read.
    pub fn scan(&self, folders: &[PathBuf]) {
        let folders = folders.to_vec();
        let host = self.clone();
        let mut state = self.state();
        state.scans += 1;
        self.enqueue(move || host.finish_scan(&folders));
    }

    /// True while a read of the folders is in the line or runs.
    pub fn scanning(&self) -> bool {
        self.state().scans > 0
    }

    /// The plugins in their load now, one name for each bundle.
    pub fn loads(&self) -> Vec<String> {
        let state = self.state();
        let mut bundles: Vec<&Path> = Vec::new();
        let mut names = Vec::new();
        for entry in &state.entries {
            if matches!(entry.stage, Stage::Loading) && !bundles.contains(&entry.bundle.as_path()) {
                bundles.push(&entry.bundle);
                names.push(entry.name.clone());
            }
        }
        names
    }

    /// [`Host::scan`], and each bundle gets a new test: the scan cache is
    /// empty before the read. A loaded plugin stays loaded.
    pub fn rescan(&self, folders: &[PathBuf]) {
        let folders = folders.to_vec();
        let host = self.clone();
        let mut state = self.state();
        state.scans += 1;
        self.enqueue(move || {
            {
                let mut state = host.state();
                state.cache = Cache::default();
                state.cache_read = true;
            }
            host.save_cache();
            host.finish_scan(&folders);
        });
    }

    fn finish_scan(&self, folders: &[PathBuf]) {
        let bundles = scan::find_bundles(folders);
        // The cache file and the stamps are reads of the disk, so they run
        // with no lock.
        let file = {
            let state = self.state();
            let root = state.preset_root.as_ref().filter(|_| !state.cache_read);
            root.map(|root| root.join(cache::FILE_NAME))
        };
        let read = file.map(|file| Cache::read(&file));
        let stamps: Vec<cache::Stamp> = bundles.iter().map(|bundle| cache::stamp(bundle)).collect();
        let mut state = self.state();
        if let Some(read) = read {
            state.cache = read;
            state.cache_read = true;
        }
        state.scans -= 1;
        for entry in &mut state.entries {
            entry.present = bundles.contains(&entry.bundle);
            if matches!(entry.stage, Stage::Failed(_)) {
                entry.stage = Stage::Found;
            }
        }
        for bundle in &bundles {
            if state.entries.iter().all(|entry| entry.bundle != *bundle) {
                let name = bundle.file_stem().unwrap_or_default().to_string_lossy();
                let entry = Entry::new(name.into_owned(), bundle.clone(), Stage::Found);
                state.entries.push(entry);
            }
        }
        // A bundle with the files of its last read has its plugins by
        // name.
        state.scanned.clear();
        for (bundle, stamp) in bundles.iter().zip(stamps) {
            if let Some(plugins) = state.cache.plugins(bundle, stamp).map(<[Scanned]>::to_vec) {
                state.scanned.insert(bundle.clone(), stamp);
                state.name_rows(bundle, &plugins);
            } else if let Some(fault) = state.cache.fault(bundle, stamp).map(str::to_owned) {
                // The bundle has the fault of its last test, with the same
                // files: it stays off, and no new test runs.
                state.scanned.insert(bundle.clone(), stamp);
                for entry in &mut state.entries {
                    if entry.bundle == *bundle && matches!(entry.stage, Stage::Found) {
                        entry.stage = Stage::Failed(fault.clone());
                    }
                }
            }
        }
        state
            .builds
            .retain(|_, build| !matches!(build, Build::Failed(..)));
        state
            .known_presets
            .retain(|_, known| matches!(known, KnownPreset::Number(_)));
        drop(state);
        // A new preset file counts from this read of the folders.
        self.read_presets(true);
        self.make_preset_folders();
        self.scan_soon();
    }

    /// Reads each bundle with no plugin names in the scan cache, one at a
    /// time, each in a scan process, on a thread of its own. The read finds
    /// the plugins of the bundle, so the list has their names and kinds
    /// before the first load. The scan needs a [`WorkerProgram`], and
    /// starts again after each read of the folders. No caller waits: see
    /// [`Host::scan_progress`].
    pub fn scan_plugins(&self) {
        self.state().scan.wanted = true;
        // After the read of the folders that is in the line now.
        let host = self.clone();
        self.enqueue(move || host.scan_soon());
    }

    /// Where the scan is, while it runs.
    pub fn scan_progress(&self) -> Option<ScanProgress> {
        let state = self.state();
        let scan = &state.scan;
        scan.running.then(|| ScanProgress {
            read: scan.done,
            bundles: scan.done + state.unscanned().len(),
            plugin: scan
                .bundle
                .as_deref()
                .map(|bundle| name_in_test(bundle, scan.class.as_deref()))
                .unwrap_or_default(),
        })
    }

    /// Starts the scan thread when a caller asked for the scan and a
    /// bundle has no plugin names.
    fn scan_soon(&self) {
        let mut state = self.state();
        let scan = &state.scan;
        if !scan.wanted || scan.running || state.worker.is_none() || state.unscanned().is_empty() {
            return;
        }
        state.scan.running = true;
        state.scan.done = 0;
        state.scan.bundle = state.unscanned().into_iter().next();
        state.scan.class = None;
        let host = self.clone();
        let thread = std::thread::Builder::new().name("rustel-vst3-scan".into());
        if thread.spawn(move || host.run_scan()).is_err() {
            state.scan.running = false;
        }
    }

    /// The scan thread: one bundle after the other in a scan process, until
    /// each bundle has its plugin names or the host ends.
    fn run_scan(&self) {
        // A scan that ends for any reason, a panic too, leaves the flag
        // down, so a caller that waits for the scan goes on. The ordinary
        // end clears the flag in the lock that finds no more work, so a new
        // scan thread can start right after: the guard must not clear the
        // flag of that thread.
        struct Ended<'a>(&'a Host, bool);
        impl Drop for Ended<'_> {
            fn drop(&mut self) {
                if !self.1 {
                    self.0.state().scan.running = false;
                }
            }
        }
        let mut ended = Ended(self, false);
        loop {
            let next = {
                let mut state = self.state();
                let closed = self.shared.closed.load(Ordering::Relaxed);
                let bundle = state.unscanned().into_iter().next().filter(|_| !closed);
                let next = bundle.zip(state.worker.clone());
                state.scan.running = next.is_some();
                state.scan.bundle = next.as_ref().map(|(bundle, _)| bundle.clone());
                state.scan.class = None;
                next
            };
            let Some((bundle, program)) = next else {
                ended.1 = true;
                return;
            };
            let stamp = cache::stamp(&bundle);
            let read = crate::worker::scan(
                &program,
                &bundle,
                crate::worker::WORK_LIMIT,
                &self.shared.closed,
                &mut |class| self.state().scan.class = Some(class.to_owned()),
            );
            // With no scan process, each bundle stays as it is, and loads
            // in this process when a score names it. The next scan tries
            // again.
            let crate::worker::Scan::Read(read) = read else {
                return;
            };
            let mut state = self.state();
            state.scan.done += 1;
            let stored = match read {
                Ok(plugins) => {
                    state.scanned.insert(bundle.clone(), stamp);
                    state.name_rows(&bundle, &plugins);
                    state.cache.store(&bundle, stamp, &plugins)
                }
                // The host ends: the bundle keeps its place for the next
                // start.
                Err(_) if self.shared.closed.load(Ordering::Relaxed) => false,
                // The list shows the reason. No score asked for the plugin,
                // so the user gets no notice. The fault goes in the cache,
                // so the next start runs no new test of these files.
                Err(reason) => {
                    for entry in &mut state.entries {
                        if entry.bundle == bundle && matches!(entry.stage, Stage::Found) {
                            entry.stage = Stage::Failed(reason.clone());
                        }
                    }
                    state.cache.store_fault(&bundle, stamp, &reason)
                }
            };
            drop(state);
            if stored {
                self.save_cache();
                self.make_preset_folders();
            }
        }
    }

    /// Makes a preset folder for each plugin with a known name and no
    /// folder yet, so the folder names show the exact plugin names. A
    /// folder counts by the compare form of its name. Runs on a thread of
    /// the host, as the disk is slow.
    fn make_preset_folders(&self) {
        let (root, names) = {
            let state = self.state();
            let named = |entry: &&Entry| {
                entry.present
                    && state.scanned.contains_key(&entry.bundle)
                    && !matches!(entry.stage, Stage::Failed(_))
            };
            let names: Vec<String> = state
                .entries
                .iter()
                .filter(named)
                .map(|entry| entry.name.clone())
                .collect();
            (state.preset_root.clone(), names)
        };
        let Some(root) = root else {
            return;
        };
        let entries = std::fs::read_dir(&root).into_iter().flatten().flatten();
        let mut have: HashSet<String> = entries
            .map(|entry| canonical(&entry.file_name().to_string_lossy()))
            .collect();
        for name in names {
            let key = canonical(&name);
            if !key.is_empty() && have.insert(key) {
                let _ = std::fs::create_dir_all(root.join(folder_name(&name)));
            }
        }
    }

    /// Writes the cache file beside the preset folders.
    fn save_cache(&self) {
        let _one_writer = locked(&self.shared.saving);
        let (cache, root) = {
            let state = self.state();
            (state.cache.clone(), state.preset_root.clone())
        };
        if let Some(root) = root {
            cache.write(&root.join(cache::FILE_NAME));
        }
    }

    /// The plugin list, by name.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        let state = self.state();
        let mut list: Vec<PluginInfo> = state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.present)
            .map(|(row, entry)| {
                let plugin = match &entry.stage {
                    Stage::Ready(plugin) => Some(plugin),
                    _ => None,
                };
                let latencies = state.latencies.iter();
                let latencies = latencies.filter(|((key, _), _)| row_of(key.plugin) == Some(row));
                PluginInfo {
                    name: entry.name.clone(),
                    bundle: entry.bundle.clone(),
                    vendor: entry.vendor.clone(),
                    categories: entry.categories.clone(),
                    status: match &entry.stage {
                        Stage::Found => Status::Found,
                        Stage::Loading => Status::Loading,
                        Stage::Ready(_) => Status::Ready,
                        Stage::Failed(error) => Status::Failed(error.clone()),
                    },
                    params: plugin
                        .map(|plugin| Arc::clone(&plugin.params))
                        .unwrap_or_default(),
                    instrument: entry.categories.contains("Instrument"),
                    running: plugin.map_or(0, |plugin| plugin.running.load(Ordering::Relaxed)),
                    load_time: plugin.map_or(Duration::ZERO, |plugin| plugin.load_time),
                    audio_thread: state.audio_thread.contains(&entry.key),
                    latency: latencies.map(|(_, latency)| *latency).max(),
                    presets: if entry.class.is_empty() {
                        0
                    } else {
                        state
                            .preset_index
                            .get(&entry.class.to_ascii_lowercase())
                            .map_or(0, Vec::len)
                    },
                }
            })
            .collect();
        list.sort_by_key(|info| info.name.to_lowercase());
        list
    }

    /// The names of the preset files of a plugin, sorted. `None` means the
    /// plugin thread reads the preset folder now: ask again later.
    pub fn presets(&self, plugin: &str) -> Option<Vec<String>> {
        let files = self.preset_list(plugin)?;
        Some(files.into_iter().map(|(name, _)| name).collect())
    }

    /// The number of the reads of the preset folders so far. A caller that
    /// holds the number of its last read reads again when this moves: the
    /// watch read the folders, and the answer may have changed.
    pub fn presets_version(&self) -> u64 {
        self.state().presets_version
    }

    /// True while the plugin thread reads the preset folders, also for the
    /// change of the folders themselves: a long read says so, and no
    /// caller waits for it.
    pub fn presets_reading(&self) -> bool {
        self.state().presets_reading
    }

    /// Copies a `.vstpreset` file into the preset folder of its plugin: the
    /// plugin with the class the file names. A score then plays the preset
    /// by the name of the file. Gives the plugin name and the preset name.
    pub fn add_preset(&self, file: &Path) -> Result<(String, String), String> {
        let bytes = std::fs::read(file).map_err(|error| error.to_string())?;
        let class = crate::preset::parse(&bytes)?.class.to_owned();
        let (plugin, root) = {
            let state = self.state();
            let of_class =
                |entry: &&Entry| entry.present && entry.class.eq_ignore_ascii_case(&class);
            let plugin = state.entries.iter().find(of_class);
            (
                plugin.map(|entry| entry.name.clone()),
                state.preset_root.clone(),
            )
        };
        let plugin = plugin.ok_or(
            "no plugin of the scan has the class of this preset: a rescan reads the class of each plugin",
        )?;
        let root = root.ok_or("rustel has no preset folder")?;
        let preset = file.file_stem().unwrap_or_default().to_string_lossy();
        let folder =
            preset_folder(&root, &plugin).unwrap_or_else(|| root.join(folder_name(&plugin)));
        let target = folder.join(file.file_name().unwrap_or_default());
        if target.exists() {
            return Err(format!(
                "{plugin} has a preset with the name '{preset}' already"
            ));
        }
        std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::copy(file, &target))
            .map_err(|error| error.to_string())?;
        // The read of the preset folders knows the new file now: no read of
        // the disk again. A preset of an added folder with this name gives
        // way, as a read of the disk puts the root first.
        let mut state = self.state();
        let files = state
            .preset_index
            .entry(class.to_ascii_lowercase())
            .or_default();
        files.retain(|(known, _)| known.as_str() != preset.as_ref());
        files.push((preset.clone().into_owned(), target));
        files.sort();
        state
            .known_presets
            .retain(|_, known| matches!(known, KnownPreset::Number(_)));
        state.presets_version += 1;
        Ok((plugin, preset.into_owned()))
    }

    /// The preset files of a plugin, from the read of the preset folders:
    /// the files whose header names the class of the plugin. `None` while
    /// the read runs: ask again later. No caller waits for the disk.
    fn preset_list(&self, plugin: &str) -> Option<Vec<(String, PathBuf)>> {
        let state = self.state();
        if !state.presets_read {
            drop(state);
            self.read_presets(false);
            return None;
        }
        let key = canonical(plugin);
        let Some(entry) = state.entries.iter().find(|entry| entry.key == key) else {
            // No plugin of the scan has this name, so no read brings its
            // presets: the answer stands as none, and no ask waits.
            return Some(Vec::new());
        };
        // The class of the plugin comes from the load or the test of its
        // bundle, so a plugin with no class yet has no answer: ask again.
        // A plugin past its load with no class keeps none - an entry of an
        // old scan cache - so no ask waits for one.
        if entry.class.is_empty() {
            if matches!(entry.stage, Stage::Found | Stage::Loading) {
                return None;
            }
            return Some(Vec::new());
        }
        Some(
            state
                .preset_index
                .get(&entry.class.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// The problems since the last call, for the user.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut self.state().errors)
    }

    /// Loads the bundle of one row, if no load ran before. With `wait`,
    /// returns after the load.
    fn load(&self, index: usize, wait: bool) {
        // The stage changes and the job joins the line under one lock, so
        // a caller that waits behind this load finds the job in the line.
        let finished = {
            let mut state = self.state();
            if matches!(state.entries[index].stage, Stage::Found) {
                // A bundle loads one time for all its plugins: each row of
                // the bundle waits for this load.
                let bundle = state.entries[index].bundle.clone();
                for entry in &mut state.entries {
                    if entry.bundle == bundle && matches!(entry.stage, Stage::Found) {
                        entry.stage = Stage::Loading;
                    }
                }
                let host = self.clone();
                Some(self.enqueue(move || host.finish_load(bundle)))
            } else {
                None
            }
        };
        match finished {
            Some(finished) if wait => Self::wait_for(finished),
            // The plugin thread does one job at a time. An empty job ends
            // after a load that runs now.
            None if wait => self.wait_idle(),
            _ => {}
        }
    }

    /// Loads a bundle in this process and stores its plugins. Runs on the
    /// plugin thread.
    fn finish_load(&self, bundle: PathBuf) {
        let start = Instant::now();
        let stamp = cache::stamp(&bundle);
        let loaded = contained("load", || load_bundle(&bundle, &mut |_| {}));
        let loaded = loaded.map(|(mut plugins, failed): (Vec<Plugin>, Vec<Scanned>)| {
            for plugin in &mut plugins {
                plugin.load_time = start.elapsed();
            }
            (plugins, failed)
        });
        let mut state = self.state();
        match loaded {
            Ok((plugins, failed)) => {
                // A plugin keeps its row, and so its number, over an unload
                // and a new load.
                let loads = plugins.iter().map(Plugin::scanned);
                let scanned: Vec<Scanned> = loads.chain(failed).collect();
                state.name_rows(&bundle, &scanned);
                // A row that is loaded keeps its plugin: the copies in the
                // audio engine are copies of that plugin. A plugin with no
                // row to take ends after the lock.
                let mut spare = Vec::new();
                for plugin in plugins {
                    let key = canonical(plugin.name());
                    let mut rows = state.entries.iter_mut();
                    let row = rows.find(|row| row.bundle == bundle && row.key == key);
                    match row.filter(|row| !matches!(row.stage, Stage::Ready(_))) {
                        Some(entry) => entry.stage = Stage::Ready(Arc::new(plugin)),
                        None => spare.push(plugin),
                    }
                }
                // A row from an earlier load with no plugin in the bundle
                // now: the file changed.
                for entry in &mut state.entries {
                    if entry.bundle == bundle && matches!(entry.stage, Stage::Loading) {
                        entry.stage = Stage::Failed("the bundle has no such plugin now".into());
                    }
                }
                // The next start has the plugins of the bundle by name.
                state.scanned.insert(bundle.clone(), stamp);
                let stored = state.cache.store(&bundle, stamp, &scanned);
                drop(state);
                drop(spare);
                if stored {
                    self.save_cache();
                    self.make_preset_folders();
                }
                // A score asks for a preset right after the load, so the
                // first read of the preset folders runs here, in this job,
                // and no ask waits behind one more job of the line. A
                // later load reads the folder of each plugin it loaded
                // again, for a file the player put there while the studio
                // runs: the load pays for that one folder, as it always
                // did, and the other folders keep the answer of the last
                // full read.
                if let Some((root, extra)) = self.claim_presets_read(false) {
                    self.finish_presets(root, extra);
                } else {
                    for plugin in &scanned {
                        self.refresh_preset_folder(plugin);
                    }
                }
            }
            // The list shows the reason, and `Host::key` gives the reason
            // to each call of a score. A line here would say the fault a
            // second time.
            Err(error) => {
                for entry in &mut state.entries {
                    if entry.bundle == bundle && matches!(entry.stage, Stage::Loading) {
                        entry.stage = Stage::Failed(error.clone());
                    }
                }
            }
        }
    }

    /// Unloads each plugin bundle with no plugin in `keep` and no running
    /// copy, on the plugin thread. `keep` holds plugin names as a score
    /// writes them. A copy that was prepared for a plugin not in `keep`
    /// ends too. The next use of an unloaded plugin loads its bundle again.
    pub fn unload_unused(&self, keep: &[String]) {
        let keep = keep.to_vec();
        let host = self.clone();
        self.enqueue(move || host.finish_unload(&keep));
    }

    /// True when an engine holds a copy of a plugin that no name in `keep`
    /// reaches. [`Host::unload_unused`] keeps such a plugin loaded until the
    /// engine gives the copy back. `keep` holds plugin names as a score
    /// writes them.
    pub fn engine_holds_unkept(&self, keep: &[String]) -> bool {
        let state = self.state();
        let kept: Vec<&Path> = keep
            .iter()
            .filter_map(|name| state.find(name))
            .map(|index| state.entries[index].bundle.as_path())
            .collect();
        state.taken.iter().any(|(build, cell)| {
            cell.place() == Place::Engine
                && row_of(build.0.plugin)
                    .and_then(|row| state.entries.get(row))
                    .is_some_and(|entry| !kept.contains(&entry.bundle.as_path()))
        })
    }

    /// True while an engine holds a copy for a slot of a chain in `chains`.
    /// A chain that ends keeps such a copy until the engine gives it back:
    /// see [`Host::keep`].
    pub fn engine_holds_chains(&self, chains: &[u64]) -> bool {
        let state = self.state();
        state
            .taken
            .iter()
            .any(|(build, cell)| cell.place() == Place::Engine && chains.contains(&build.2.chain))
    }

    /// The names in `names` that are the whole name or the bundle name of a
    /// plugin. A part of a name is not: `piano` is no name of `Pianoteq 8`.
    pub fn whole_names(&self, names: &[String]) -> Vec<String> {
        let state = self.state();
        let whole = |name: &&String| state.find_whole(&canonical(name)).is_some();
        names.iter().filter(whole).cloned().collect()
    }

    /// Tells the host the plugin copies the chains of one caller want.
    /// `caller` is the number of the caller, as the chains of one session.
    /// `live` has each copy a live chain of the caller wants. `named` has
    /// the chains the caller names. `rest` has each copy the caller keeps
    /// at rest, for an open score that does not play now, with the rest
    /// order of the score: a score that went to rest later has a higher
    /// order. A copy at rest keeps its build as a live copy does. The bound
    /// of the parked copies ends no copy of a named chain, unless only a
    /// copy at rest asks for it. It ends the copies at rest after each copy
    /// of a chain no caller names, the lowest order first. A call takes the
    /// place of the last call of the same caller, so a second caller
    /// unnames no chain of the first. `ended` has the chains that ended: no
    /// score has these chains now.
    ///
    /// Each build of an ended chain ends, and so does each build for a slot
    /// of a listed chain with a key no copy of the slot asks for, live or
    /// at rest. So no engine takes such a copy, a build in the line makes
    /// no copy, and no later call starts one. A ready copy ends in the end
    /// job of the plugin thread. One end job at most is in the line, and a
    /// call while it waits adds its copies to it. The job ends 4 copies at
    /// most, and a job at the end of the line ends the others. A call that
    /// wants the copy again before its job keeps the copy. A copy an engine
    /// gives back for such a slot ends when it comes home. A chain no caller
    /// lists keeps its builds. The host resolves each name and preset as
    /// [`Host::key`] does, with no load and no read of a folder. A name with
    /// no answer yet keeps each key of its slot.
    ///
    /// Gives the slot and the key of each copy an engine holds for a chain
    /// the caller names, with a key no copy of its slot asks for. The engine
    /// gives such a copy back when it is not busy and its stage is not
    /// heard.
    pub fn keep(
        &self,
        caller: u64,
        live: Vec<WantedCopy>,
        named: Vec<u64>,
        rest: Vec<(WantedCopy, u64)>,
        ended: Vec<u64>,
    ) -> Vec<(InsertSlot, InsertKey)> {
        let mut state = self.state();
        state.ended.extend(ended);
        let mut kept = Kept {
            named: named.into_iter().collect(),
            ..Kept::default()
        };
        for copy in live {
            kept.wanted.entry(copy.slot.chain).or_default().push(copy);
        }
        for (copy, order) in rest {
            let chain = copy.slot.chain;
            kept.rest.entry(chain).or_default().push((copy, order));
        }
        if kept.wanted.is_empty() && kept.named.is_empty() && kept.rest.is_empty() {
            state.callers.remove(&caller);
        } else {
            state.callers.insert(caller, kept);
        }
        let kept_chain = |chain| {
            let kept = state.callers.get(&caller);
            kept.is_some_and(|kept| kept.named.contains(&chain))
        };
        let held: Vec<(InsertSlot, InsertKey)> = state
            .taken
            .iter()
            .filter(|(build, cell)| {
                cell.place() == Place::Engine && kept_chain(build.2.chain) && state.unwanted(build)
            })
            .map(|((key, _, slot), _)| (*slot, *key))
            .collect();
        let gone: HashSet<(InsertKey, u32, InsertSlot)> = state
            .builds
            .keys()
            .filter(|build| state.chain_ended(build) || state.unwanted(build))
            .copied()
            .collect();
        // A ready copy ends on the plugin thread. A build in the line
        // finds no entry: see `Host::finish`.
        state
            .builds
            .retain(|build, built| matches!(built, Build::Ready(_)) || !gone.contains(build));
        let ready: Vec<(InsertKey, u32, InsertSlot)> = gone
            .into_iter()
            .filter(|build| state.builds.contains_key(build))
            .collect();
        // One end job at most is in the line. A copy joins its list one
        // time, and the first copy starts the job.
        let start = state.ending.is_none() && !ready.is_empty();
        if !ready.is_empty() {
            let ending = state.ending.get_or_insert_default();
            for build in ready {
                if !ending.contains(&build) {
                    ending.push(build);
                }
            }
        }
        drop(state);
        if start {
            self.end_unwanted();
        }
        held
    }

    /// Ends the ready copies of [`State::ending`] that no chain wants, on
    /// the plugin thread: [`ENDS_PER_JOB`] copies in a job, then a job at the
    /// end of the line with the others and the copies later calls of
    /// [`Host::keep`] added. A copy that a call of [`Host::keep`] wants again
    /// before its job stays.
    fn end_unwanted(&self) {
        let host = self.clone();
        self.enqueue(move || {
            let copies = {
                let mut state = host.state();
                let mut gone = state.ending.take().unwrap_or_default();
                // A later call that wants a copy again keeps it.
                gone.retain(|build| state.chain_ended(build) || state.unwanted(build));
                // The others stay in the list while this job ends its
                // copies, so a call meanwhile adds to the list and starts no
                // job.
                state.ending = Some(gone.split_off(gone.len().min(ENDS_PER_JOB)));
                state.take_ready(|build| gone.contains(build))
            };
            for copy in copies {
                copy.end_here();
            }
            let mut state = host.state();
            if state.ending.as_ref().is_some_and(Vec::is_empty) {
                state.ending = None;
            } else {
                drop(state);
                host.end_unwanted();
            }
        });
    }

    /// Runs on the plugin thread.
    fn finish_unload(&self, keep: &[String]) {
        // The rows of the bundles no name in `keep` reaches.
        let unkept = |state: &State| -> Vec<usize> {
            let kept: Vec<&Path> = keep
                .iter()
                .filter_map(|name| state.find(name))
                .map(|index| state.entries[index].bundle.as_path())
                .collect();
            (0..state.entries.len())
                .filter(|index| !kept.contains(&state.entries[*index].bundle.as_path()))
                .collect()
        };
        // A copy that waits for a slot keeps its plugin loaded. No open
        // score asks for these copies now, so they end first.
        let parked = {
            let mut state = self.state();
            let rows = unkept(&state);
            state.take_ready(|build| row_of(build.0.plugin).is_some_and(|row| rows.contains(&row)))
        };
        for instance in parked {
            instance.end_here();
        }
        // The plugins leave the list under the lock, and their libraries
        // unload after the lock: an unload runs plugin code.
        let unloaded: Vec<Arc<Plugin>> = {
            let mut state = self.state();
            let rows = unkept(&state);
            let busy: Vec<PathBuf> = rows
                .iter()
                .filter(|row| {
                    // A failed build holds no plugin.
                    let building = state.builds.iter().any(|(build, built)| {
                        row_of(build.0.plugin) == Some(**row) && !matches!(built, Build::Failed(..))
                    });
                    let running = match &state.entries[**row].stage {
                        Stage::Ready(plugin) => plugin.running.load(Ordering::Relaxed) > 0,
                        Stage::Loading => true,
                        _ => false,
                    };
                    building || running
                })
                .map(|row| state.entries[*row].bundle.clone())
                .collect();
            rows.into_iter()
                .filter_map(|row| {
                    let entry = &mut state.entries[row];
                    if busy.contains(&entry.bundle) || !matches!(entry.stage, Stage::Ready(_)) {
                        return None;
                    }
                    match std::mem::replace(&mut entry.stage, Stage::Found) {
                        Stage::Ready(plugin) => {
                            // The latencies of its copies go with the plugin.
                            state
                                .latencies
                                .retain(|(key, _), _| row_of(key.plugin) != Some(row));
                            Some(plugin)
                        }
                        _ => None,
                    }
                })
                .collect()
        };
        self.end_plugins(unloaded);
    }

    /// Ends plugins on the plugin thread. A caller that reads a plugin now
    /// holds a handle for a moment, and the last handle ends the plugin.
    /// Such a plugin goes to the end of the line until the handle is gone.
    fn end_plugins(&self, plugins: Vec<Arc<Plugin>>) {
        let read: Vec<Arc<Plugin>> = plugins
            .into_iter()
            .filter_map(|plugin| Arc::try_unwrap(plugin).err())
            .collect();
        if read.is_empty() {
            return;
        }
        // After a shutdown no later job ends these plugins here, and the
        // last handle must not end a plugin on the thread of its caller.
        // The plugins stay to the end of the process.
        if self.shared.closed.load(Ordering::Relaxed) {
            std::mem::forget(read);
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
        let host = self.clone();
        self.enqueue(move || host.end_plugins(read));
    }

    /// Finds the plugin a score names and loads the plugin on first use.
    /// With `wait`, the answer is never [`Resolved::Pending`].
    pub fn resolve(&self, name: &str, wait: bool) -> Resolved {
        let found = {
            let state = self.state();
            (state.find(name), state.scans > 0)
        };
        let index = match found {
            (Some(index), _) => index,
            // A read of the folders is not done: the name is not known yet.
            (None, true) if wait => {
                self.wait_idle();
                match self.state().find(name) {
                    Some(index) => index,
                    None => return Resolved::Missing,
                }
            }
            (None, true) => return Resolved::Pending,
            (None, false) => return Resolved::Missing,
        };
        self.load(index, wait);
        // A bundle can hold more than one plugin, so the name can now be
        // the name of a different row, or of no row.
        let state = self.state();
        let index = match (state.find(name), &state.entries[index].stage) {
            (Some(index), _) => index,
            (None, Stage::Failed(_)) => index,
            (None, _) => return Resolved::Missing,
        };
        match &state.entries[index].stage {
            Stage::Ready(plugin) => Resolved::Ready(State::number(index), Arc::clone(plugin)),
            Stage::Found | Stage::Loading => Resolved::Pending,
            Stage::Failed(error) => Resolved::Failed(error.clone()),
        }
    }

    /// The plugin a score names, if the plugin is loaded. The call loads
    /// nothing.
    pub fn loaded(&self, name: &str) -> Option<Arc<Plugin>> {
        let state = self.state();
        match &state.entries[state.find(name)?].stage {
            Stage::Ready(plugin) => Some(Arc::clone(plugin)),
            _ => None,
        }
    }

    /// The number of a preset of a plugin, from the name of its file.
    pub fn preset(&self, plugin: u32, name: &str) -> FoundPreset {
        let wanted = (plugin, canonical(name));
        let entry_name = {
            let state = self.state();
            // A note asks on each start. The answer for a name is kept,
            // and a name with no file gets a new look after `PRESET_RETRY`.
            match state.known_presets.get(&wanted) {
                Some(KnownPreset::Number(number)) => return FoundPreset::Number(*number),
                Some(KnownPreset::MissingSince(since)) if since.elapsed() < PRESET_RETRY => {
                    return FoundPreset::Missing;
                }
                _ => {}
            }
            let entry = row_of(plugin).and_then(|row| state.entries.get(row));
            match entry {
                Some(entry) => entry.name.clone(),
                None => return FoundPreset::Missing,
            }
        };
        let Some(files) = self.preset_list(&entry_name) else {
            return FoundPreset::Pending;
        };
        let mut state = self.state();
        let file = files
            .into_iter()
            .find(|(name, _)| canonical(name) == wanted.1);
        let Some((_, path)) = file else {
            // A read of the folders in the line can still bring the file:
            // the answer says missing now, and the next ask decides again,
            // for a kept answer would hold the missing past the read.
            if !state.presets_reading {
                state
                    .known_presets
                    .insert(wanted, KnownPreset::MissingSince(Instant::now()));
            }
            return FoundPreset::Missing;
        };
        let known = state
            .presets
            .iter()
            .position(|preset| preset.0 == plugin && preset.1 == path);
        let place = known.unwrap_or_else(|| {
            state.presets.push((plugin, path));
            state.presets.len() - 1
        });
        let number = place as u32 + 1;
        state
            .known_presets
            .insert(wanted, KnownPreset::Number(number));
        FoundPreset::Number(number)
    }

    /// The key of one plugin call of a score, and the plugin. `name` and
    /// `preset` are the names the score writes, and `instrument` is true
    /// for `.vsti()`. The call loads the plugin on first use, and waits for
    /// nothing. `Ok(None)` means the plugin or the preset loads now: ask
    /// again later. `Err` has the reason the host does not serve the call,
    /// for the user.
    pub fn key(
        &self,
        name: &str,
        preset: Option<&str>,
        instrument: bool,
    ) -> Result<Option<(InsertKey, Arc<Plugin>)>, String> {
        let (plugin, loaded) = match self.resolve(name, false) {
            Resolved::Ready(plugin, loaded) => (plugin, loaded),
            Resolved::Pending => return Ok(None),
            Resolved::Missing => return Err(format!("no plugin has the name '{name}'")),
            Resolved::Failed(reason) => return Err(format!("{name} did not load: {reason}")),
        };
        let found = loaded.name();
        match (instrument, loaded.is_instrument()) {
            (true, false) => return Err(format!("{found} is an effect: use .vst()")),
            (false, true) => return Err(format!("{found} is an instrument: use .vsti()")),
            _ => {}
        }
        let preset = match preset {
            None => match self.preset(plugin, DEFAULT_PRESET) {
                FoundPreset::Number(number) => number,
                FoundPreset::Pending => return Ok(None),
                FoundPreset::Missing => 0,
            },
            Some(preset) => match self.preset(plugin, preset) {
                FoundPreset::Number(number) => number,
                // The host reads the preset folder now.
                FoundPreset::Pending => return Ok(None),
                FoundPreset::Missing => {
                    return Err(format!("{found} has no preset with the name '{preset}'"));
                }
            },
        };
        Ok(Some((InsertKey { plugin, preset }, loaded)))
    }

    /// [`Host::insert`] as the provider the audio engine takes.
    pub fn provider(&self, wait: bool) -> Arc<InsertProvider> {
        let host = self.clone();
        Arc::new(move |key, sample_rate, slot| host.insert(key, sample_rate, slot, wait))
    }

    /// Returns after the plugin thread finished the work it has now.
    pub fn wait_idle(&self) {
        self.wait_idle_within(WAIT_LIMIT, || false);
    }

    /// [`Host::wait_idle`] for `limit` at most, and until `cancelled` says
    /// to stop. True means the plugin thread finished its work.
    pub fn wait_idle_within(&self, limit: Duration, cancelled: impl Fn() -> bool) -> bool {
        let finished = self.enqueue(|| {});
        let start = Instant::now();
        loop {
            match finished.recv_timeout(Duration::from_millis(20)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return true,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancelled() || start.elapsed() >= limit {
                        return false;
                    }
                }
            }
        }
    }

    /// Builds a plugin for one slot of the audio engine. With `wait`, the
    /// call returns the plugin: this is for a render to a file, and the copy
    /// ends at its drop. A render ends no copy a caller keeps at rest to
    /// make room under the bound of the parked copies: with no other copy
    /// to end, it builds past the bound. The bound does not count the build
    /// of a render and does not end it before the render takes its copy.
    /// See [`Host::keep`]. With no wait, the first call starts the work and
    /// a later call returns the plugin: this is for live play, where the
    /// caller has no time to lose. A live copy goes home at its drop and
    /// waits for the slot again, so a later call for the slot builds
    /// nothing. A quiet copy parks at once, on the thread of the drop. The
    /// plugin thread plays out the tail of each other copy first. At the
    /// bound of the parked copies, a copy that comes home takes the place of
    /// a failed build. A copy at rest also takes the place of a copy of a
    /// chain no caller names or of a copy at rest of a lower order. With no
    /// such place, the copy ends. A copy of a named chain takes the place of
    /// any copy the bound ends, and parks past the bound with none. While an
    /// engine holds such a copy or gives it back, no call builds a second
    /// copy of its key for the slot, at any rate.
    pub fn insert(
        &self,
        key: InsertKey,
        sample_rate: u32,
        slot: InsertSlot,
        wait: bool,
    ) -> Option<Box<dyn OrbitInsert>> {
        let build = (key, sample_rate, slot);
        let take = |state: &mut State| {
            state.rendering.remove(&build);
            match state.builds.remove(&build) {
                Some(Build::Ready(mut copy)) => {
                    copy.set_any_thread(!state.on_audio_thread(key.plugin));
                    if wait {
                        return Some(copy as Box<dyn OrbitInsert>);
                    }
                    state.taken.retain(|_, cell| cell.place() != Place::Home);
                    state.taken.insert(build, copy.hold());
                    let host = Arc::downgrade(&self.shared);
                    let copy = Some(copy);
                    Some(Box::new(Lent { build, copy, host }) as Box<dyn OrbitInsert>)
                }
                Some(other) => {
                    state.builds.insert(build, other);
                    None
                }
                None => None,
            }
        };
        // The build is marked and its job joins the line under one lock,
        // so a caller that waits behind this build finds the job there.
        let finished = {
            let mut state = self.state();
            if let Some(found) = state.builds.get(&build) {
                if wait && matches!(found, Build::Running) {
                    // A request with no wait started this build.
                    drop(state);
                    self.wait_idle();
                    return take(&mut self.state());
                }
                return take(&mut state);
            }
            let claimed = Self::claim(&mut state, build, wait)?;
            if wait {
                state.rendering.insert(build);
            }
            let host = self.clone();
            self.enqueue(move || host.finish(build, claimed))
        };
        if !wait {
            return None;
        }
        Self::wait_for(finished);
        take(&mut self.state())
    }

    /// How far the plugin a score names is for one slot. The call starts
    /// the load of a plugin not yet loaded, and waits for nothing.
    pub fn prepared(
        &self,
        name: &str,
        preset: Option<&str>,
        instrument: bool,
        sample_rate: u32,
        slot: InsertSlot,
    ) -> Prepared {
        let key = match self.key(name, preset, instrument) {
            Ok(Some((key, _))) => key,
            Ok(None) => return Prepared::Pending,
            Err(reason) => return Prepared::Unavailable(reason),
        };
        let build = (key, sample_rate, slot);
        let state = self.state();
        match state.builds.get(&build) {
            Some(Build::Ready(_)) => Prepared::Ready(key),
            Some(Build::Running) => Prepared::Pending,
            Some(Build::Failed(_, reason)) => Prepared::Unavailable(reason.clone()),
            None if state.in_engine(&build) => Prepared::Ready(key),
            None if state.returning(key, slot) => Prepared::Returning,
            None => Prepared::Unbuilt(key),
        }
    }

    /// The copies on their way home that the live chains of `caller` want,
    /// for a caller that holds its play at the rate `sample_rate` until they
    /// are back: see [`Host::keep`]. Such a copy comes home from an engine,
    /// takes a new rate now, or waits for its slot at another rate with no
    /// build at `sample_rate`. It counts when a live copy of its slot in the
    /// last call of [`Host::keep`] of `caller` asks for its key. A copy the
    /// caller keeps at rest does not count, and keeps its rate until its
    /// score plays again. The call starts the retune of each counted copy
    /// that waits at another rate, so the count is of the work it waits
    /// for. The build of a new copy does not count. The call makes no plugin
    /// call, starts no load and reads no folder.
    pub fn returning(&self, caller: u64, sample_rate: u32) -> usize {
        let mut state = self.state();
        let wanted = |state: &State, (key, _, slot): &(InsertKey, u32, InsertSlot)| {
            let kept = state.callers.get(&caller);
            let copies = kept.and_then(|kept| kept.wanted.get(&slot.chain));
            let asks = |copy: &WantedCopy| copy.slot == *slot && state.asks_for(copy, *key);
            copies.is_some_and(|copies| copies.iter().any(asks))
        };
        let lent = state.taken.iter();
        let lent = lent.filter(|(_, cell)| cell.place() == Place::Returning);
        let moving = lent.map(|(build, _)| build).chain(state.retunes.keys());
        let mut count = moving.filter(|build| wanted(&state, build)).count();
        let parked: Vec<(InsertKey, u32, InsertSlot)> = state
            .builds
            .iter()
            .filter(|(_, built)| matches!(built, Build::Ready(_)))
            .map(|(build, _)| *build)
            .filter(|build| wanted(&state, build))
            .collect();
        for (key, _, slot) in parked {
            // The claim refuses a copy with a build at the rate, the copy
            // itself too. The build is marked and its job joins the line
            // under one lock, as in `Host::insert`.
            let build = (key, sample_rate, slot);
            if let Some(claimed) = Self::claim(&mut state, build, false) {
                let host = self.clone();
                self.enqueue(move || host.finish(build, claimed));
                count += 1;
            }
        }
        count
    }

    /// The latency of the plugin copy of one slot, in frames: the copy that
    /// waits for the slot, else the copy an engine holds or gives back, else
    /// the last copy of the key at this rate. `None` when the host built no
    /// copy of the key at this rate since the plugin loaded. The call makes
    /// no plugin call and waits for nothing.
    pub fn latency(&self, key: InsertKey, sample_rate: u32, slot: InsertSlot) -> Option<u32> {
        let build = (key, sample_rate, slot);
        let state = self.state();
        match (state.builds.get(&build), state.taken.get(&build)) {
            (Some(Build::Ready(copy)), _) => Some(copy.latency()),
            (_, Some(cell)) if cell.place() != Place::Home => {
                Some(cell.latency.load(Ordering::Relaxed))
            }
            _ => state.latencies.get(&(key, sample_rate)).copied(),
        }
    }

    /// Loads the plugin a score names and builds it for one slot, with no
    /// wait, so the plugin is ready before the first note asks. A later
    /// [`Host::insert`] for the same slot takes the plugin. A name the host
    /// cannot serve does nothing here: the note reports the reason.
    ///
    /// For a loaded plugin with a known preset, the call marks the build:
    /// [`Host::prepared`] then answers [`Prepared::Pending`], and a second
    /// call adds no job. The build of a plugin that loads now goes in the
    /// line behind the load, one time for each slot.
    pub fn prepare(
        &self,
        name: &str,
        preset: Option<&str>,
        instrument: bool,
        sample_rate: u32,
        slot: InsertSlot,
    ) {
        let key = match self.key(name, preset, instrument) {
            Ok(Some((key, _))) => key,
            // The plugin loads, or the host reads its preset folder.
            Ok(None) => {
                let request = Prepare {
                    name: name.to_owned(),
                    preset: preset.map(str::to_owned),
                    instrument,
                    sample_rate,
                    slot,
                };
                let mut state = self.state();
                if !state.preparing.contains(&request) {
                    state.preparing.push(request.clone());
                    drop(state);
                    self.prepare_soon(request, PREPARE_TRIES);
                }
                return;
            }
            Err(_) => return,
        };
        let build = (key, sample_rate, slot);
        // The build is marked and its job joins the line under one lock,
        // as in `Host::insert`.
        let mut state = self.state();
        if let Some(claimed) = Self::claim(&mut state, build, false) {
            let host = self.clone();
            self.enqueue(move || host.finish(build, claimed));
        }
    }

    /// Puts the build of a prepared plugin in the line of the plugin
    /// thread. The plugin thread does one job at a time, so the build runs
    /// after the read of the folders or the load that is in the line now.
    /// The build has the key a note asks for: see [`Host::key`]. While the
    /// plugin loads or the host reads its preset folder, the build has no
    /// key yet. It then goes to the end of the line again, `tries` times at
    /// most.
    fn prepare_soon(&self, request: Prepare, tries: u32) {
        let host = self.clone();
        self.enqueue(move || {
            let preset = request.preset.as_deref();
            let key = host.key(&request.name, preset, request.instrument);
            if matches!(key, Ok(None)) && tries > 0 {
                return host.prepare_soon(request, tries - 1);
            }
            // The request leaves the line here. A later call is a new one.
            host.state().preparing.retain(|waiting| *waiting != request);
            let Ok(Some((key, _))) = key else {
                return;
            };
            let build = (key, request.sample_rate, request.slot);
            let claimed = Self::claim(&mut host.state(), build, false);
            if let Some(claimed) = claimed {
                host.finish(build, claimed);
            }
        });
    }

    /// Marks a build as started and gives what the build needs. `None`
    /// means the build exists already, the chain of its slot ended, no
    /// chain wants the key for the slot, the host has no such plugin, or
    /// the parked copies leave no room. `None` also while an engine holds a
    /// copy of the key for the slot or gives one back, at any rate: that
    /// copy comes home, so a second copy would stay parked. A copy of the
    /// key that waits for the slot at another rate goes to the build, which
    /// sets it up at its rate. Such a build makes no copy, so the bound does
    /// not count it. A `render` build ends no copy at rest to make room,
    /// and builds past the bound when no other build ends: see
    /// [`MAX_PARKED`].
    fn claim(
        state: &mut State,
        build: (InsertKey, u32, InsertSlot),
        render: bool,
    ) -> Option<(Arc<Plugin>, Option<PathBuf>)> {
        let (key, _, slot) = build;
        if state.builds.contains_key(&build)
            || state.chain_ended(&build)
            || state.unwanted(&build)
            || state.held(key, slot)
        {
            return None;
        }
        let row = row_of(key.plugin)?;
        let entry = state.entries.get(row)?;
        let Stage::Ready(plugin) = &entry.stage else {
            return None;
        };
        let plugin = Arc::clone(plugin);
        let preset = match key.preset {
            0 => None,
            number => Some(state.presets.get(number as usize - 1)?.1.clone()),
        };
        let parked = state.builds.iter().find_map(|(other, built)| {
            let ready = matches!(built, Build::Ready(_)) && (other.0, other.2) == (key, slot);
            ready.then_some(*other)
        });
        if let Some(Build::Ready(copy)) = parked.and_then(|parked| state.builds.remove(&parked)) {
            state.retunes.insert(build, Some(copy));
            state.builds.insert(build, Build::Running);
            return Some((plugin, preset));
        }
        // A plugin prepared for a score that no longer asks for it stays
        // parked. Bound copies, failures and queued builds together: the
        // build of the lowest rank ends first. A render ends only a failure
        // or a copy of a chain no caller names, the ranks under each copy
        // at rest. With neither, it builds past the bound: it takes its copy
        // at once. A live build with no build to end does not start, and
        // gives one line.
        let below = render.then_some(Rank::Rest(0, 0));
        if !state.make_room(below) && !render {
            let (call, name) = (plugin.call(), plugin.name());
            let line = format!(
                "{call} skipped: {name} did not start: the host keeps {MAX_PARKED} copies ready at most"
            );
            if !state.errors.contains(&line) {
                state.errors.push(line);
            }
            return None;
        }
        state.builds.insert(build, Build::Running);
        Some((plugin, preset))
    }

    /// Builds the plugin of a claimed build. Runs on the plugin thread. A
    /// build with no entry makes no copy: the chain of its slot ended, or no
    /// chain wants the key for the slot, and [`Host::keep`] took the entry.
    /// A build no chain wants now leaves the list here. When this happens
    /// during the build, the copy ends here.
    ///
    /// The build of a copy that waited for its slot at another rate sets the
    /// copy up at the rate of the build: see [`Instance::retune`]. A rate
    /// the plugin refuses is a failure with its reason. A copy whose preset
    /// file changed since its build ends, and the build reads the file as
    /// a new build does.
    fn finish(
        &self,
        build: (InsertKey, u32, InsertSlot),
        (plugin, preset): (Arc<Plugin>, Option<PathBuf>),
    ) {
        let waits = |state: &mut State| {
            let running = matches!(state.builds.get(&build), Some(Build::Running));
            if running && state.unwanted(&build) {
                state.builds.remove(&build);
                return false;
            }
            running
        };
        let retune = {
            let mut state = self.state();
            // The build keeps its place in the retunes while it works on the
            // copy: see `Host::returning`.
            let retune = state.retunes.get_mut(&build).and_then(Option::take);
            if !waits(&mut state) {
                state.retunes.remove(&build);
                drop(state);
                if let Some(copy) = retune {
                    copy.end_here();
                }
                return;
            }
            retune
        };
        let retire = self.shared.jobs.clone();
        let (key, rate) = (build.0, build.1);
        // The stamp comes before the read of the file, so a save after the
        // stamp ends the copy at the next read of the preset folders.
        let stamp = preset.as_deref().map(|file| cache::stamp(file).number());
        let changed = |copy: &Instance| {
            let cell = copy.cell();
            let built = cell.preset.load(Ordering::Relaxed);
            cell.preset_changed.load(Ordering::Relaxed) || stamp.is_some_and(|now| now != built)
        };
        let built = match retune {
            Some(mut copy) if !changed(&copy) => match contained("retune", || copy.retune(rate)) {
                Ok(()) => Ok(copy),
                Err(error) => {
                    copy.end_here();
                    Err(error)
                }
            },
            retune => {
                if let Some(copy) = retune {
                    copy.end_here();
                }
                let preset = preset
                    .map(|path| {
                        std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
                    })
                    .transpose();
                let built = preset.and_then(|preset| {
                    let build = || build_instance(&plugin, preset.as_deref(), key, rate, retire);
                    contained("build", build)
                });
                built.map(Box::new)
            }
        };
        let mut state = self.state();
        state.retunes.remove(&build);
        if !waits(&mut state) {
            drop(state);
            if let Ok(copy) = built {
                copy.end_here();
            }
            return;
        }
        state.built += 1;
        let result = match built {
            Ok(mut copy) => {
                copy.set_order(state.built);
                if let Some(stamp) = stamp {
                    copy.cell().preset.store(stamp, Ordering::Relaxed);
                }
                state.latencies.insert((key, rate), copy.latency());
                Build::Ready(copy)
            }
            Err(error) => {
                // A caller that asks `Host::prepared` for the slot makes the
                // same line from the reason. A caller that says each text one
                // time then says the fault one time.
                let reason = format!("{} did not start: {error}", plugin.name());
                state
                    .errors
                    .push(format!("{} skipped: {reason}", plugin.call()));
                Build::Failed(state.built, reason)
            }
        };
        state.builds.insert(build, result);
    }

    /// Takes home a copy an engine gave back, on the plugin thread: see
    /// [`Host::insert`]. A copy ends here with no flush after a shutdown,
    /// for a slot of a chain that ended, for a key its slot does not want,
    /// and with the preset mark: see [`CopyState::preset_changed`]. Each
    /// other copy plays out its tail with no lock ([`Instance::flush_tail`]),
    /// and waits for its slot again at the rate of its build, after the
    /// same checks, when the bound of the parked copies has room for it:
    /// see [`State::park`]. A quiet copy parks at its drop and takes no such
    /// job: see [`Lent`].
    fn park(&self, build: (InsertKey, u32, InsertSlot), mut copy: Box<Instance>) {
        let closed = || self.shared.closed.load(Ordering::Relaxed);
        let mut state = self.state();
        if !closed() && !state.ends_at_park(&build, &copy) {
            drop(state);
            let flushed = copy.flush_tail(&self.shared.closed);
            state = self.state();
            if flushed.is_ok() && !closed() {
                let ended = state.park(build, copy);
                drop(state);
                if let Some(copy) = ended {
                    copy.end_here();
                }
                return;
            }
        }
        state.taken.remove(&build);
        drop(state);
        copy.end_here();
    }

    /// The stamp of the preset file of each copy the host knows, for the
    /// end of a read of the preset folders: see [`State::stale_copies`].
    /// With `folder`, only the files under that folder get a stamp, for the
    /// read of one folder in a load. The list comes from one lock, and each
    /// file gets one stamp with no lock.
    fn stamp_preset_files(&self, folder: Option<&Path>) -> HashMap<PathBuf, u64> {
        let files: HashSet<PathBuf> = {
            let state = self.state();
            let files = state.cells().filter_map(|(key, _)| state.preset_file(key));
            let files = files.filter(|file| folder.is_none_or(|folder| file.starts_with(folder)));
            files.map(Path::to_path_buf).collect()
        };
        let stamp = |file: PathBuf| {
            let stamp = cache::stamp(&file).number();
            (file, stamp)
        };
        files.into_iter().map(stamp).collect()
    }
}

/// A plugin copy an engine took for live play: see [`Host::insert`]. Each
/// call of the engine goes to the copy. The copy goes home at the drop. A
/// quiet copy parks there, on the thread of the drop: a copy with no tail
/// to play out ([`Instance::skips_flush`]), no restart its plugin asked
/// for, and no preset mark. The plugin thread plays out the tail of each
/// other copy and keeps it for its slot: see [`Host::park`].
struct Lent {
    /// The key, the rate and the slot the copy was built for.
    build: (InsertKey, u32, InsertSlot),
    /// The copy, until the drop.
    copy: Option<Box<Instance>>,
    host: Weak<Shared>,
}

impl OrbitInsert for Lent {
    fn key(&self) -> InsertKey {
        self.build.0
    }

    fn set_param(&mut self, param: InsertParam, frames: u32) {
        if let Some(copy) = &mut self.copy {
            copy.set_param(param, frames);
        }
    }

    fn set_param_now(&mut self, param: InsertParam) {
        if let Some(copy) = &mut self.copy {
            copy.set_param_now(param);
        }
    }

    fn restore_params(&mut self, frames: u32) {
        if let Some(copy) = &mut self.copy {
            copy.restore_params(frames);
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Some(copy) = &mut self.copy {
            copy.process(left, right);
        }
    }

    fn submit(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Some(copy) = &mut self.copy {
            copy.submit(left, right);
        }
    }

    fn collect(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Some(copy) = &mut self.copy {
            copy.collect(left, right);
        }
    }

    fn flush(&mut self, frames: usize) {
        if let Some(copy) = &mut self.copy {
            copy.flush(frames);
        }
    }

    fn prime(&mut self, frames: usize) {
        if let Some(copy) = &mut self.copy {
            copy.prime(frames);
        }
    }

    fn sync(&mut self, beats: f64, tempo: f32, frames: u32) {
        if let Some(copy) = &mut self.copy {
            copy.sync(beats, tempo, frames);
        }
    }

    fn note(&mut self, note: InsertNote, frames: u32) {
        if let Some(copy) = &mut self.copy {
            copy.note(note, frames);
        }
    }

    fn score(&mut self, score: u64, frames: u32) {
        if let Some(copy) = &mut self.copy {
            copy.score(score, frames);
        }
    }

    fn cut_notes(&mut self, frames: u32) {
        if let Some(copy) = &mut self.copy {
            copy.cut_notes(frames);
        }
    }

    fn renders_ahead(&self) -> bool {
        self.copy.as_ref().is_some_and(|copy| copy.renders_ahead())
    }

    fn busy(&self) -> bool {
        self.copy.as_ref().is_some_and(|copy| copy.busy())
    }

    fn any_thread(&self) -> bool {
        self.copy.as_ref().is_some_and(|copy| copy.any_thread())
    }

    fn reset(&mut self) {
        if let Some(copy) = &mut self.copy {
            copy.reset();
        }
    }

    fn tail_frames(&self) -> u32 {
        self.copy.as_ref().map_or(0, |copy| copy.tail_frames())
    }

    fn drain(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Some(copy) = &mut self.copy {
            copy.drain(left, right);
        }
    }
}

impl Drop for Lent {
    fn drop(&mut self) {
        let Some(mut copy) = self.copy.take() else {
            return;
        };
        // With no host, the copy ends here as any copy does.
        let Some(shared) = self.host.upgrade() else {
            return;
        };
        let host = Host { shared };
        let build = self.build;
        // A quiet copy parks here with one lock and no plugin call, so the
        // next take of its slot finds it at once, and not behind the builds
        // in the line of the plugin thread. A copy this park refuses ends in
        // a job of that thread.
        copy.reset();
        let cell = copy.cell();
        let quiet = copy.skips_flush()
            && !cell.latency_changed.load(Ordering::Acquire)
            && !cell.preset_changed.load(Ordering::Relaxed);
        if quiet && !host.shared.closed.load(Ordering::Relaxed) {
            copy.rebase();
            let ended = host.state().park(build, copy);
            if let Some(copy) = ended {
                host.enqueue(move || copy.end_here());
            }
            return;
        }
        copy.cell().set_place(Place::Returning);
        let home = host.clone();
        // After a shutdown the line takes no job, and the copy ends with
        // the job here.
        host.enqueue(move || home.park(build, copy));
    }
}

/// The row of a plugin number.
fn row_of(number: u32) -> Option<usize> {
    (number as usize).checked_sub(1)
}

/// Runs a load or a build on the plugin thread. A panic in the work is a
/// failure with a reason. The caller stores the failure, so no row stays
/// [`Stage::Loading`] and no build stays [`Build::Running`].
fn contained<T>(work: &str, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let run = std::panic::AssertUnwindSafe(run);
    std::panic::catch_unwind(run).unwrap_or_else(|_| Err(format!("the {work} panicked")))
}

/// A class as the scan cache keeps it, with the fault of a class that did
/// not load.
fn scanned(class: &ClassInfo, fault: Option<String>) -> Scanned {
    Scanned {
        name: class.name.clone(),
        vendor: class.vendor.clone(),
        categories: class.categories.clone(),
        class: class.id_text(),
        fault,
    }
}

/// Loads a bundle in this process and reads the parameters of each plugin
/// in the bundle, as the load of a score does. Calls `progress` with the
/// name of each class before its load, so a caller can report where the
/// read is. Answers the plugins that load and each class that does not,
/// with its fault. A bundle with no class that loads fails with the fault
/// of its first class. Runs on the plugin thread.
pub(crate) fn load_bundle(
    bundle: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<Plugin>, Vec<Scanned>), String> {
    let module = Arc::new(Module::load(bundle)?);
    let classes = module.classes();
    if classes.is_empty() {
        return Err("the bundle has no audio plugin".into());
    }
    let mut plugins = Vec::new();
    let mut failed = Vec::new();
    for class in classes {
        progress(&class.name);
        match Loaded::new(Arc::clone(&module), &class).map(|loaded| loaded.params()) {
            Ok(params) => plugins.push(Plugin::new(Arc::clone(&module), class, params)),
            Err(fault) => failed.push(scanned(&class, Some(fault))),
        }
    }
    match failed.first() {
        Some(first) if plugins.is_empty() => Err(first.fault.clone().unwrap_or_default()),
        _ => Ok((plugins, failed)),
    }
}

/// Makes one running plugin. `preset` is the content of a preset file. Runs
/// on the plugin thread.
fn build_instance(
    plugin: &Plugin,
    preset: Option<&[u8]>,
    key: InsertKey,
    sample_rate: u32,
    retire: Sender<Job>,
) -> Result<Instance, String> {
    let loaded = Loaded::new(Arc::clone(&plugin.module), &plugin.info)?;
    if let Some(file) = preset {
        loaded.set_preset(file, &plugin.info)?;
    }
    let running = Arc::clone(&plugin.running);
    loaded.activate(key, sample_rate, retire, running, plugin.params())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part of a name finds the plugin with the part as whole words, then
    /// at the start of a word, before a shorter name with the part inside a
    /// word.
    #[test]
    fn a_part_of_a_name_finds_whole_words_first() {
        let mut state = State::default();
        for name in [
            "Vital",
            "Talk Box",
            "TAL-NoiseMaker",
            "ValhallaSupermassive",
        ] {
            let bundle = PathBuf::from(format!("{name}.vst3"));
            let entry = Entry::new(name.to_owned(), bundle, Stage::Found);
            state.entries.push(entry);
        }
        let found = |name: &str| state.find(name).map(|row| state.entries[row].name.as_str());
        assert_eq!(found("TAL"), Some("TAL-NoiseMaker"));
        assert_eq!(found("talk"), Some("Talk Box"));
        assert_eq!(found("noise"), Some("TAL-NoiseMaker"));
        assert_eq!(found("supermassive"), Some("ValhallaSupermassive"));
        assert_eq!(found("ital"), Some("Vital"));
    }

    /// A panic in a load or a build is a failure with a reason. A panic of
    /// a thread with the locks of the host leaves the host in use.
    #[test]
    fn a_panic_leaves_the_host_in_use() {
        let failed: Result<(), String> = contained("build", || panic!("a build fault"));
        assert_eq!(failed, Err("the build panicked".into()));

        let host = Host::new();
        let faulty = host.clone();
        let fault = std::thread::spawn(move || {
            let _locks = (faulty.state(), locked(&faulty.shared.gate));
            panic!("a fault with the locks");
        });
        assert!(fault.join().is_err());
        assert!(host.shared.state.is_poisoned() && host.shared.gate.is_poisoned());
        assert!(host.plugins().is_empty());
        assert!(host.wait_idle_within(WAIT_LIMIT, || false));
    }

    /// A class that does not start leaves the other class of its bundle
    /// loaded, in a load in this process too. A later load of the bundle
    /// reads the preset folder of each class again, also of the class that
    /// stayed loaded. So a copy of that class that waits for its slot with a
    /// file saved again since its build ends in the load.
    #[test]
    fn a_class_that_fails_its_load_leaves_the_others_of_its_bundle() {
        let root = std::env::temp_dir().join(format!("rustel-vst3-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("plugin folder");
        rustel_vst3_fixture::install(&root);
        // SAFETY: the tests of this binary run one at a time, and the host
        // threads start after the variable.
        unsafe { std::env::set_var(rustel_vst3_fixture::BROKEN_TONE_ENV, "1") };
        let host = Host::new();
        host.scan(std::slice::from_ref(&root));
        host.wait_idle();
        let effect = host.resolve(rustel_vst3_fixture::NAME, true);
        let tone = host.resolve(rustel_vst3_fixture::TONE_NAME, true);
        // SAFETY: the loads are over.
        unsafe { std::env::remove_var(rustel_vst3_fixture::BROKEN_TONE_ENV) };
        assert!(matches!(effect, Resolved::Ready(..)));
        assert!(matches!(
            &tone,
            Resolved::Failed(reason) if reason == "the plugin processor did not start"
        ));

        // A new scan gives the class a new try. A copy of the effect waits
        // for its slot with the file `default`, and the file changes before
        // the load of the class. The watch of the preset folders reads them
        // 400 ms after the write, and the load comes first.
        host.rescan(std::slice::from_ref(&root));
        host.wait_idle();
        let presets = root.join("presets");
        let folder = presets.join(rustel_vst3_fixture::NAME);
        std::fs::create_dir_all(&folder).expect("preset folder");
        let file = folder.join("default.vstpreset");
        std::fs::write(&file, rustel_vst3_fixture::preset(0.5, 0.0)).expect("preset file");
        host.set_preset_folder(presets);
        let slot = InsertSlot::effect(1, 0);
        let parked = || host.prepared(rustel_vst3_fixture::NAME, None, false, 48_000, slot);
        host.prepare(rustel_vst3_fixture::NAME, None, false, 48_000, slot);
        host.wait_idle();
        let Prepared::Ready(key) = parked() else {
            panic!("no copy waits for its slot");
        };
        assert_ne!(key.preset, 0, "the copy of the file");
        let saved = rustel_vst3_fixture::preset_with_delay(0.5, 0.0, 0.25);
        std::fs::write(&file, saved).expect("preset file");
        let tone = host.resolve(rustel_vst3_fixture::TONE_NAME, true);
        assert!(matches!(tone, Resolved::Ready(..)));
        assert_eq!(parked(), Prepared::Unbuilt(key));
        drop(host);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A quiet copy whose plugin asked for a restart for a new latency does
    /// not park at its drop. It goes home in a job of the plugin thread,
    /// which turns the plugin off and on before the copy waits for its slot
    /// again.
    #[test]
    fn a_quiet_copy_with_a_new_latency_restarts_on_its_way_home() {
        let root = std::env::temp_dir().join(format!("rustel-vst3-latency-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("plugin folder");
        rustel_vst3_fixture::install(&root);
        let host = Host::new();
        host.scan(std::slice::from_ref(&root));
        host.wait_idle();
        let Resolved::Ready(plugin, _) = host.resolve(rustel_vst3_fixture::NAME, true) else {
            panic!("the fixture plugin did not load");
        };
        let slot = InsertSlot::effect(1, 0);
        let key = InsertKey { plugin, preset: 0 };
        let build = (key, 48_000, slot);
        let take = || host.insert(key, 48_000, slot, false);
        assert!(take().is_none(), "the build starts");
        host.wait_idle();
        let held = take().expect("the built copy");
        // The copy ran no block, so it is quiet. Its plugin asks for a
        // restart, as the handler of the copy marks the cell.
        let cell = Arc::clone(host.state().taken.get(&build).expect("the held copy"));
        cell.latency_changed.store(true, Ordering::Release);
        // A job holds the plugin thread, so the look reads the copy on its
        // way home.
        let (release, line) = mpsc::channel::<()>();
        host.enqueue(move || {
            let _ = line.recv();
        });
        drop(held);
        assert_eq!(cell.place(), Place::Returning, "no park at the drop");
        drop(release);
        host.wait_idle();
        assert!(!cell.latency_changed.load(Ordering::Acquire), "the restart");
        assert!(matches!(
            host.state().builds.get(&build),
            Some(Build::Ready(_))
        ));
        drop(host);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A build a render did not take yet keeps its place at the bound of
    /// the parked copies. A build for another slot ends one other build
    /// only.
    #[test]
    fn the_bound_ends_no_build_a_render_did_not_take_yet() {
        let build = |chain| {
            let key = InsertKey {
                plugin: 1,
                preset: 0,
            };
            (key, 48_000, InsertSlot::effect(chain, 0))
        };
        let mut state = State::default();
        for chain in 0..=MAX_PARKED as u64 {
            state
                .builds
                .insert(build(chain), Build::Failed(chain, String::new()));
        }
        state.rendering.insert(build(0));
        assert!(state.make_room(None));
        assert!(
            state.builds.contains_key(&build(0)),
            "the build of the render"
        );
        assert_eq!(state.builds.len(), MAX_PARKED);
    }
}
