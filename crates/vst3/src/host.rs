//! The plugin list, the plugin thread, and the names a score uses.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustel_audio::{
    InsertControls, InsertKey, InsertParam, InsertProvider, InsertSlot, MAX_INSERT_PARAMS,
    OrbitInsert,
};

use crate::cache::{self, Cache, Scanned};
use crate::instance::{Instance, Loaded};
use crate::module::{ClassInfo, Module};
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
/// its plugins finds all of them.
const MAX_PARKED: usize = rustel_audio::INSERT_SLOTS;
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

#[derive(Default)]
struct State {
    /// The number of a plugin is its place here plus 1. Rows are only added.
    entries: Vec<Entry>,
    /// The number of a preset is its place here plus 1.
    presets: Vec<(u32, PathBuf)>,
    /// The answers to the preset names the scores asked for, by plugin
    /// number and compare form of the name.
    known_presets: HashMap<(u32, String), KnownPreset>,
    /// The preset files of each plugin, by the compare form of its name.
    preset_lists: HashMap<String, PresetList>,
    builds: HashMap<(InsertKey, u32, InsertSlot), Build>,
    /// The builds an engine took: the mark of each is alive while the
    /// engine holds the plugin.
    taken: HashMap<(InsertKey, u32, InsertSlot), std::sync::Weak<()>>,
    /// The prepared plugins with a build in the line behind a load or a
    /// read of the preset folder: see [`Host::prepare`].
    preparing: Vec<Prepare>,
    /// The chains that ended: see [`Host::end_chains`]. No chain number
    /// comes a second time, so a chain here gets no new build. An engine
    /// holds the copy of such a chain until the end of its tail.
    ended: HashSet<u64>,
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

    /// True while an engine holds the plugin it took for this build.
    fn held(&self, build: &(InsertKey, u32, InsertSlot)) -> bool {
        let alive = |held: &std::sync::Weak<()>| held.strong_count() > 0;
        self.taken.get(build).is_some_and(alive)
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

/// Reads the preset files of a plugin from the folder with its name under
/// `root`: name and path, sorted by name. This reads the disk, so the
/// plugin thread runs it.
fn read_presets(root: Option<&Path>, plugin: &str) -> Vec<(String, PathBuf)> {
    let wanted = canonical(plugin);
    let folder = root
        .and_then(|root| std::fs::read_dir(root).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| canonical(&name.to_string_lossy()) == wanted)
        });
    let Some(folder) = folder else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vstpreset"))
        })
        .filter_map(|path| Some((path.file_stem()?.to_string_lossy().into_owned(), path)))
        .collect();
    files.sort();
    files
}

/// The preset files of one plugin as the host last read them.
#[derive(Default)]
struct PresetList {
    files: Vec<(String, PathBuf)>,
    /// The time of the last read. `None` before the end of the first read.
    read_at: Option<Instant>,
    /// A read is in the line of the plugin thread.
    reading: bool,
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
    pub fn set_preset_folder(&self, folder: PathBuf) {
        self.state().preset_root = Some(folder);
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
        // The next request reads each preset folder again.
        for list in state.preset_lists.values_mut() {
            list.read_at = list
                .read_at
                .map(|at| at.checked_sub(PRESET_RETRY).unwrap_or(at));
        }
        drop(state);
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
            .filter(|entry| entry.present)
            .map(|entry| {
                let plugin = match &entry.stage {
                    Stage::Ready(plugin) => Some(plugin),
                    _ => None,
                };
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

    /// The preset files of a plugin as last read, and a new read on the
    /// plugin thread when the last one is older than [`PRESET_RETRY`]. No
    /// caller waits for the disk.
    fn preset_list(&self, plugin: &str) -> Option<Vec<(String, PathBuf)>> {
        let key = canonical(plugin);
        let mut state = self.state();
        let root = state.preset_root.clone();
        let list = state.preset_lists.entry(key.clone()).or_default();
        let fresh = list.read_at.is_some_and(|at| at.elapsed() < PRESET_RETRY);
        if !fresh && !list.reading {
            list.reading = true;
            let host = self.clone();
            let plugin = plugin.to_owned();
            self.enqueue(move || host.store_presets(&plugin, root.as_deref()));
        }
        list.read_at.map(|_| list.files.clone())
    }

    /// Reads the preset folder of a plugin and keeps the result. Runs on
    /// the plugin thread.
    fn store_presets(&self, plugin: &str, root: Option<&Path>) {
        let files = read_presets(root, plugin);
        let mut state = self.state();
        let list = state.preset_lists.entry(canonical(plugin)).or_default();
        *list = PresetList {
            files,
            read_at: Some(Instant::now()),
            reading: false,
        };
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
                // A score asks for a preset right after the load, so the
                // preset folders are read here, on the plugin thread.
                let root = state.preset_root.clone();
                drop(state);
                let lists: Vec<_> = plugins
                    .iter()
                    .map(|plugin| read_presets(root.as_deref(), plugin.name()))
                    .collect();
                state = self.state();
                for (plugin, files) in plugins.iter().zip(lists) {
                    let list = PresetList {
                        files,
                        read_at: Some(Instant::now()),
                        reading: false,
                    };
                    state.preset_lists.insert(canonical(plugin.name()), list);
                }
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
        state.taken.iter().any(|(build, held)| {
            held.strong_count() > 0
                && row_of(build.0.plugin)
                    .and_then(|row| state.entries.get(row))
                    .is_some_and(|entry| !kept.contains(&entry.bundle.as_path()))
        })
    }

    /// The names in `names` that are the whole name or the bundle name of a
    /// plugin. A part of a name is not: `piano` is no name of `Pianoteq 8`.
    pub fn whole_names(&self, names: &[String]) -> Vec<String> {
        let state = self.state();
        let whole = |name: &&String| state.find_whole(&canonical(name)).is_some();
        names.iter().filter(whole).cloned().collect()
    }

    /// Ends the chains in `chains`: no score has these chains now. Each
    /// copy the host made ready for a slot of the chains ends, so no engine
    /// takes it. A build in the line for such a slot makes no copy, and no
    /// later call starts one.
    pub fn end_chains(&self, chains: Vec<u64>) {
        let mut state = self.state();
        state.ended.extend(&chains);
        let of_chain = move |build: &(InsertKey, u32, InsertSlot)| chains.contains(&build.2.chain);
        // A ready copy ends on the plugin thread. A build in the line
        // finds no entry: see `Host::finish`.
        state
            .builds
            .retain(|build, built| matches!(built, Build::Ready(_)) || !of_chain(build));
        drop(state);
        let host = self.clone();
        self.enqueue(move || {
            let copies = host.state().take_ready(of_chain);
            for copy in copies {
                copy.end_here();
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
                        Stage::Ready(plugin) => Some(plugin),
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
            state
                .known_presets
                .insert(wanted, KnownPreset::MissingSince(Instant::now()));
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
            None => 0,
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
    /// call returns the plugin: this is for a render to a file. With no
    /// wait, the first call starts the work and a later call returns the
    /// plugin: this is for live play, where the caller has no time to lose.
    pub fn insert(
        &self,
        key: InsertKey,
        sample_rate: u32,
        slot: InsertSlot,
        wait: bool,
    ) -> Option<Box<dyn OrbitInsert>> {
        let build = (key, sample_rate, slot);
        let take = |state: &mut State| match state.builds.remove(&build) {
            Some(Build::Ready(mut copy)) => {
                state.taken.retain(|_, held| held.strong_count() > 0);
                state.taken.insert(build, copy.hold());
                Some(copy as Box<dyn OrbitInsert>)
            }
            Some(other) => {
                state.builds.insert(build, other);
                None
            }
            None => None,
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
            let claimed = Self::claim(&mut state, build)?;
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
            None if state.held(&build) => Prepared::Ready(key),
            None => Prepared::Unbuilt(key),
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
        if let Some(claimed) = Self::claim_prepared(&mut state, build) {
            let host = self.clone();
            self.enqueue(move || host.finish(build, claimed));
        }
    }

    /// Puts the build of a prepared plugin in the line of the plugin
    /// thread. The plugin thread does one job at a time, so the build runs
    /// after the read of the folders or the load that is in the line now.
    /// A build that finds the plugin not ready goes to the end of the line
    /// again, `tries` times at most.
    fn prepare_soon(&self, request: Prepare, tries: u32) {
        let host = self.clone();
        self.enqueue(move || {
            let resolved = host.resolve(&request.name, false);
            if matches!(resolved, Resolved::Pending) && tries > 0 {
                return host.prepare_soon(request, tries - 1);
            }
            // The request leaves the line here. A later call is a new one.
            host.state().preparing.retain(|waiting| *waiting != request);
            let Resolved::Ready(plugin, loaded) = resolved else {
                return;
            };
            if loaded.is_instrument() != request.instrument {
                return;
            }
            let preset = match &request.preset {
                None => 0,
                Some(preset) => {
                    // This is the plugin thread: read the folder here.
                    let root = host.state().preset_root.clone();
                    host.store_presets(loaded.name(), root.as_deref());
                    match host.preset(plugin, preset) {
                        FoundPreset::Number(number) => number,
                        _ => return,
                    }
                }
            };
            let build = (
                InsertKey { plugin, preset },
                request.sample_rate,
                request.slot,
            );
            let claimed = Self::claim_prepared(&mut host.state(), build);
            if let Some(claimed) = claimed {
                host.finish(build, claimed);
            }
        });
    }

    /// [`Host::claim`] for a prepared plugin. `None` also when an engine
    /// holds the plugin of the slot: a second copy would stay parked.
    fn claim_prepared(
        state: &mut State,
        build: (InsertKey, u32, InsertSlot),
    ) -> Option<(Arc<Plugin>, Option<PathBuf>)> {
        if state.held(&build) {
            return None;
        }
        Self::claim(state, build)
    }

    /// Marks a build as started and gives what the build needs. `None`
    /// means the build exists already, the chain of its slot ended, or the
    /// host has no such plugin.
    fn claim(
        state: &mut State,
        build: (InsertKey, u32, InsertSlot),
    ) -> Option<(Arc<Plugin>, Option<PathBuf>)> {
        if state.builds.contains_key(&build) || state.chain_ended(&build) {
            return None;
        }
        let key = build.0;
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
        // A plugin prepared for a score that no longer asks for it stays
        // parked. Bound copies, failures and queued builds together, and
        // forget old failures before discarding a usable copy.
        while state.builds.len() >= MAX_PARKED {
            let oldest = state
                .builds
                .iter()
                .filter_map(|(build, built)| match built {
                    Build::Failed(order, _) => Some((*build, (0, *order))),
                    Build::Ready(copy) => Some((*build, (1, copy.order()))),
                    Build::Running => None,
                })
                .min_by_key(|(_, order)| *order)
                .map(|(build, _)| build);
            let oldest = oldest?;
            state.builds.remove(&oldest);
        }
        state.builds.insert(build, Build::Running);
        Some((plugin, preset))
    }

    /// Builds the plugin of a claimed build. Runs on the plugin thread. A
    /// build with no entry makes no copy: the chain of its slot ended, and
    /// [`Host::end_chains`] took the entry. When the chain ends during the
    /// build, the copy ends here.
    fn finish(
        &self,
        build: (InsertKey, u32, InsertSlot),
        (plugin, preset): (Arc<Plugin>, Option<PathBuf>),
    ) {
        let waits = |state: &State| matches!(state.builds.get(&build), Some(Build::Running));
        if !waits(&self.state()) {
            return;
        }
        let retire = self.shared.jobs.clone();
        let (key, rate) = (build.0, build.1);
        let preset = preset
            .map(|path| {
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
            })
            .transpose();
        let built = preset.and_then(|preset| {
            let build = || build_instance(&plugin, preset.as_deref(), key, rate, retire);
            contained("build", build)
        });
        let mut state = self.state();
        if !waits(&state) {
            drop(state);
            if let Ok(copy) = built {
                Box::new(copy).end_here();
            }
            return;
        }
        state.built += 1;
        let result = match built {
            Ok(mut copy) => {
                copy.set_order(state.built);
                Build::Ready(Box::new(copy))
            }
            Err(error) => {
                let name = plugin.name();
                state.errors.push(format!("vst {name}: {error}"));
                Build::Failed(state.built, format!("{name} did not start: {error}"))
            }
        };
        state.builds.insert(build, result);
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
    /// loaded, in a load in this process too.
    #[test]
    fn a_class_that_fails_its_load_leaves_the_others_of_its_bundle() {
        let root = std::env::temp_dir().join(format!("rustel-vst3-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("plugin folder");
        rustel_vst3_fixture::install(&root);
        // SAFETY: no other test of this binary loads the fixture, and the
        // host threads start after the variable.
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
        drop(host);
        let _ = std::fs::remove_dir_all(&root);
    }
}
