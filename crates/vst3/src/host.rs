//! The plugin list, the plugin thread, and the names a score uses.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use rustel_audio::{InsertKey, InsertProvider, OrbitInsert};

use crate::cache::{self, Cache, Scanned};
use crate::instance::{Instance, Loaded};
use crate::module::{ClassInfo, Module};
use crate::worker::{RemoteInstance, Worker};
use crate::{Job, ParamInfo, canonical, scan};

/// The program that runs one bundle in a process of its own: a worker. A
/// plugin with a fault, at its load or in the middle of a set, then ends
/// its worker and not the host. The host runs `program`, `args`, then the
/// bundle path, and the program calls [`crate::serve`] with the path.
///
/// With no worker program, the host loads each bundle in its own process,
/// and a plugin fault ends the host.
#[derive(Clone)]
pub struct WorkerProgram {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// A bundle with a worker that ended this many times does not start again
/// before the next read of the folders.
const MAX_WORKER_ENDS: u32 = 3;
/// The number of a plugin for the audio engine has the row of the plugin in
/// its low bits. The bits above count the workers of the bundle that ended,
/// so a plugin in a new worker has a new number, and the engine takes a new
/// copy in place of the copy in the worker that ended.
const ROW_BITS: u32 = 20;
/// The plugins the host keeps ready for a slot that did not take them yet.
/// One score needs one for each slot at most, so a start that waits for
/// its plugins finds all of them.
const MAX_PARKED: usize = rustel_audio::INSERT_SLOTS;
/// A prepared plugin waits behind the read of the folders and the load:
/// its build goes to the end of the line this many times at most.
const PREPARE_TRIES: u32 = 4;

/// What [`Host::prepare`] builds: the plugin and the preset by name, and
/// the slot.
struct Prepare {
    name: String,
    preset: Option<String>,
    instrument: bool,
    sample_rate: u32,
    slot: usize,
}

/// A caller waits this long at most for the plugin thread. A plugin that
/// hangs in a call keeps the thread, and the caller goes on with no plugin.
const WAIT_LIMIT: Duration = Duration::from_secs(120);
/// The host looks again for a preset file it did not find after this time.
const PRESET_RETRY: Duration = Duration::from_secs(2);

/// Where a loaded plugin runs.
enum Source {
    /// In this process.
    Here(Arc<Module>, ClassInfo),
    /// In a worker, at this place of the bundle.
    Worker(Arc<Worker>, usize),
}

/// A loaded plugin: its class and its parameters.
pub struct Plugin {
    source: Source,
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
    fn new(source: Source, class: Scanned, params: Vec<ParamInfo>) -> Self {
        let keys = params
            .iter()
            .map(|param| (param.key.clone(), param.id))
            .collect();
        Self {
            source,
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

    pub(crate) fn vendor(&self) -> &str {
        &self.class.vendor
    }

    pub(crate) fn categories(&self) -> &str {
        &self.class.categories
    }

    /// The plugin as the scan cache keeps the plugin.
    fn scanned(&self) -> Scanned {
        self.class.clone()
    }

    /// True for a plugin in the worker with this serial number.
    fn runs_in(&self, worker: u64) -> bool {
        match &self.source {
            Source::Worker(mine, _) => mine.serial() == worker,
            Source::Here(..) => false,
        }
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
    /// The process number of the worker of the plugin. `None` for a plugin
    /// that is not loaded, and for a plugin in the process of the host.
    pub process: Option<u32>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// wait for.
    Unavailable,
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
    /// The count of ended workers at the last end of the worker of this
    /// plugin: see [`ROW_BITS`].
    generation: u32,
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
            generation: 0,
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

/// A plugin copy the host keeps for a slot: in this process, or in a
/// worker.
enum Copy {
    Here(Box<Instance>),
    Remote(Box<RemoteInstance>),
}

impl Copy {
    /// The place of the copy in the order the host built them.
    fn order(&self) -> u64 {
        match self {
            Self::Here(instance) => instance.order(),
            Self::Remote(instance) => instance.order(),
        }
    }

    fn set_order(&mut self, order: u64) {
        match self {
            Self::Here(instance) => instance.set_order(order),
            Self::Remote(instance) => instance.set_order(order),
        }
    }

    /// Marks the copy as held by an engine: see [`Instance::hold`].
    fn hold(&mut self) -> Weak<()> {
        match self {
            Self::Here(instance) => instance.hold(),
            Self::Remote(instance) => instance.hold(),
        }
    }

    fn insert(self) -> Box<dyn OrbitInsert> {
        match self {
            Self::Here(instance) => instance,
            Self::Remote(instance) => instance,
        }
    }

    /// Ends the copy now. Only the plugin thread calls this.
    fn end_here(self) {
        match self {
            Self::Here(instance) => instance.end_here(),
            Self::Remote(instance) => drop(instance),
        }
    }
}

enum Build {
    Running,
    Ready(Copy),
    Failed,
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
    builds: HashMap<(InsertKey, u32, usize), Build>,
    /// The builds an engine took: the mark of each is alive while the
    /// engine holds the plugin.
    taken: HashMap<(InsertKey, u32, usize), std::sync::Weak<()>>,
    preset_root: Option<PathBuf>,
    worker: Option<WorkerProgram>,
    /// The workers that ended so far, for the plugin numbers: see
    /// [`ROW_BITS`].
    generation: u32,
    /// How many times the worker of a bundle ended since the last read of
    /// the folders.
    ends: HashMap<PathBuf, u32>,
    /// The plugins built so far. The count gives each one its order.
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
    fn number(&self, row: usize) -> u32 {
        (self.entries[row].generation << ROW_BITS) | (row as u32 + 1)
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
        }
    }

    /// True while an engine holds the plugin it took for this build.
    fn held(&self, build: &(InsertKey, u32, usize)) -> bool {
        let alive = |held: &std::sync::Weak<()>| held.strong_count() > 0;
        self.taken.get(build).is_some_and(alive)
    }

    fn find(&self, name: &str) -> Option<usize> {
        let wanted = canonical(name);
        if wanted.is_empty() {
            return None;
        }
        let present = || {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.present)
        };
        if let Some((index, _)) = present().find(|(_, entry)| entry.key == wanted) {
            return Some(index);
        }
        if let Some((index, _)) = present().find(|(_, entry)| entry.stem == wanted) {
            return Some(index);
        }
        // A part of a name finds the plugin with the shortest name that
        // has the part.
        let partial = present()
            .filter(|(_, entry)| entry.key.contains(&wanted) || entry.stem.contains(&wanted))
            .min_by_key(|(_, entry)| entry.key.len());
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
        let state = std::mem::take(self.state.get_mut().expect("plugin list"));
        let _ = self.jobs.send(Box::new(move || drop(state)));
    }
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
        self.shared.state.lock().expect("plugin list")
    }

    /// Puts a job in the line of the plugin thread. The receiver gets a
    /// value when the job is done. After [`Host::shutdown`] the job does
    /// not run, and the receiver reads as disconnected.
    fn enqueue(&self, job: impl FnOnce() + Send + 'static) -> mpsc::Receiver<()> {
        let (done, finished) = mpsc::sync_channel::<()>(1);
        let _gate = self.shared.gate.lock().expect("job line");
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
            let _gate = self.shared.gate.lock().expect("job line");
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

    /// Sets the program that runs each bundle in a process of its own: see
    /// [`WorkerProgram`]. A bundle that is loaded stays where it runs.
    pub fn set_worker(&self, program: Option<WorkerProgram>) {
        self.state().worker = program;
    }

    /// The folder with one folder of `.vstpreset` files for each plugin.
    pub fn set_preset_folder(&self, folder: PathBuf) {
        self.state().preset_root = Some(folder);
    }

    /// Reads the folders again, on the plugin thread: a folder on a slow
    /// disk does not hold the caller. New bundles join the list. A plugin
    /// that failed gets a new try. Plugin numbers stay as they are.
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
            }
        }
        // A bundle with a worker that ended gets a new try.
        state.ends.clear();
        state
            .builds
            .retain(|_, build| !matches!(build, Build::Failed));
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
    /// time, each in a worker, on a thread of its own. The read finds the
    /// plugins of the bundle, so the list has their names and kinds before
    /// the first load. The scan needs a [`WorkerProgram`], and starts again
    /// after each read of the folders. No caller waits: see
    /// [`Host::scan_progress`].
    pub fn scan_plugins(&self) {
        self.state().scan.wanted = true;
        // After the read of the folders that is in the line now.
        let host = self.clone();
        self.enqueue(move || host.scan_soon());
    }

    /// The bundles the scan read and the bundles of the scan in all, while
    /// the scan runs.
    pub fn scan_progress(&self) -> Option<(usize, usize)> {
        let state = self.state();
        let done = state.scan.done;
        state
            .scan
            .running
            .then(|| (done, done + state.unscanned().len()))
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
        let host = self.clone();
        let thread = std::thread::Builder::new().name("rustel-vst3-scan".into());
        if thread.spawn(move || host.run_scan()).is_err() {
            state.scan.running = false;
        }
    }

    /// The scan thread: one bundle after the other in a worker, until each
    /// bundle has its plugin names or the host ends.
    fn run_scan(&self) {
        // A scan that ends for any reason, a panic too, leaves the flag
        // down, so a caller that waits for the scan goes on. The ordinary
        // end clears the flag in the lock that finds no more work, so a new
        // scan thread can start right after: the guard must not clear the
        // flag of that thread.
        struct Ended<'a>(&'a Host, bool);
        impl Drop for Ended<'_> {
            fn drop(&mut self) {
                if let (false, Ok(mut state)) = (self.1, self.0.shared.state.lock()) {
                    state.scan.running = false;
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
                next
            };
            let Some((bundle, program)) = next else {
                ended.1 = true;
                return;
            };
            let stamp = cache::stamp(&bundle);
            // The worker ends right after its answer: the scan loads no
            // plugin for use.
            let read = Worker::start(&program, &bundle, &self.shared.closed).map(|(_, plugins)| {
                let scanned = |plugin: crate::worker::RemotePlugin| Scanned {
                    name: plugin.name,
                    vendor: plugin.vendor,
                    categories: plugin.categories,
                };
                plugins.into_iter().map(scanned).collect::<Vec<_>>()
            });
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
                // so the user gets no notice.
                Err(reason) => {
                    for entry in &mut state.entries {
                        if entry.bundle == bundle && matches!(entry.stage, Stage::Found) {
                            entry.stage = Stage::Failed(reason.clone());
                        }
                    }
                    false
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
        let _one_writer = self.shared.saving.lock().expect("cache file");
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
                    process: plugin.and_then(|plugin| match &plugin.source {
                        Source::Worker(worker, _) => Some(worker.id()),
                        Source::Here(..) => None,
                    }),
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
            let program = state.worker.clone();
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
                Some(self.enqueue(move || host.finish_load(index, bundle, program)))
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

    /// Loads the bundle of one row and stores the plugins: in a worker when
    /// the host has a worker program, in this process when not. Runs on the
    /// plugin thread.
    fn finish_load(&self, index: usize, bundle: PathBuf, program: Option<WorkerProgram>) {
        let start = Instant::now();
        let stamp = cache::stamp(&bundle);
        let loaded = match &program {
            Some(program) => {
                Worker::start(program, &bundle, &self.shared.closed).map(|(worker, plugins)| {
                    self.watch(&worker);
                    let plugin = |(place, plugin): (usize, crate::worker::RemotePlugin)| {
                        let class = Scanned {
                            name: plugin.name,
                            vendor: plugin.vendor,
                            categories: plugin.categories,
                        };
                        let source = Source::Worker(Arc::clone(&worker), place);
                        Plugin::new(source, class, plugin.params)
                    };
                    plugins.into_iter().enumerate().map(plugin).collect()
                })
            }
            None => load_bundle(&bundle),
        };
        let loaded = loaded.map(|mut plugins: Vec<Plugin>| {
            for plugin in &mut plugins {
                plugin.load_time = start.elapsed();
            }
            plugins
        });
        let mut state = self.state();
        match loaded {
            Ok(plugins) => {
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
                let scanned: Vec<Scanned> = plugins.iter().map(Plugin::scanned).collect();
                state.name_rows(&bundle, &scanned);
                // A row that is loaded keeps its plugin: the copies in the
                // audio engine run in the worker of that plugin. A plugin
                // with no row to take ends after the lock: the end of the
                // last plugin of a worker ends the worker.
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
            Err(error) => {
                let name = state.entries[index].name.clone();
                state.errors.push(format!("vst {name}: {error}"));
                for entry in &mut state.entries {
                    if entry.bundle == bundle && matches!(entry.stage, Stage::Loading) {
                        entry.stage = Stage::Failed(error.clone());
                    }
                }
            }
        }
    }

    /// Looks at a worker until the worker is gone: a worker that ended, or
    /// gave a copy no answer, takes its plugins out of the list on the
    /// plugin thread.
    fn watch(&self, worker: &Arc<Worker>) {
        let (host, worker) = (Arc::downgrade(&self.shared), Arc::downgrade(worker));
        let thread = std::thread::Builder::new().name("rustel-vst3-watch".into());
        let _ = thread.spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(20));
                // The host unloaded the bundle in the ordinary way.
                let Some(live) = worker.upgrade() else {
                    return;
                };
                if !live.ended() {
                    continue;
                }
                let serial = live.serial();
                drop(live);
                if let Some(shared) = host.upgrade() {
                    let host = Host { shared };
                    let ended = host.clone();
                    host.enqueue(move || ended.worker_ended(serial));
                }
                return;
            }
        });
    }

    /// Looks at each worker now, with no wait for its watcher thread: an
    /// ended worker leaves the list on the plugin thread. A caller that
    /// reports the problems of the host right after uses this, then
    /// [`Host::wait_idle`].
    pub fn check_workers(&self) {
        // Process checks run with no lock on the plugin list. A worker
        // busy with the build of a copy checks its own process.
        let workers: Vec<Arc<Worker>> = {
            let state = self.state();
            let worker = |entry: &Entry| match &entry.stage {
                Stage::Ready(plugin) => match &plugin.source {
                    Source::Worker(worker, _) => Some(Arc::clone(worker)),
                    Source::Here(..) => None,
                },
                _ => None,
            };
            state.entries.iter().filter_map(worker).collect()
        };
        for worker in workers {
            if worker.ended() {
                let (host, serial) = (self.clone(), worker.serial());
                self.enqueue(move || host.worker_ended(serial));
            }
        }
    }

    /// Takes the plugins of an ended worker out of the list. The rows go
    /// back to the state before the load with a new plugin number, so the
    /// next note starts a new worker, and the audio engine takes a new copy
    /// in place of the copy with no worker. Runs on the plugin thread.
    fn worker_ended(&self, worker: u64) {
        // The look at the process waits a moment for its exit status, so
        // the look runs with no lock on the list.
        let process = self.state().entries.iter().find_map(|entry| {
            let Stage::Ready(plugin) = &entry.stage else {
                return None;
            };
            match &plugin.source {
                Source::Worker(process, _) if process.serial() == worker => {
                    Some(Arc::clone(process))
                }
                _ => None,
            }
        });
        let Some(process) = process else {
            return;
        };
        let how = process.end();
        let mut state = self.state();
        let rows: Vec<usize> = (0..state.entries.len())
            .filter(|row| match &state.entries[*row].stage {
                Stage::Ready(plugin) => plugin.runs_in(worker),
                _ => false,
            })
            .collect();
        let Some(first) = rows.first() else {
            return;
        };
        let (name, bundle) = {
            let entry = &state.entries[*first];
            (entry.name.clone(), entry.bundle.clone())
        };
        // A plugin number is never the number of 2 workers: with no count
        // left, the plugins stay off.
        let last = (1 << (32 - ROW_BITS)) - 1;
        let counted = state.generation < last;
        state.generation = (state.generation + 1).min(last);
        let generation = state.generation;
        let ends = {
            let ends = state.ends.entry(bundle).or_default();
            *ends += 1;
            *ends
        };
        let stays_off = ends >= MAX_WORKER_ENDS || !counted;
        let mut plugins = Vec::new();
        for row in &rows {
            let entry = &mut state.entries[*row];
            let next = if stays_off {
                Stage::Failed(format!("{how}, {ends} times"))
            } else {
                Stage::Found
            };
            if let Stage::Ready(plugin) = std::mem::replace(&mut entry.stage, next) {
                plugins.push(plugin);
            }
            entry.generation = generation;
        }
        // A copy for a slot has the old number: no note asks for it again.
        let of_rows = |build: &(InsertKey, u32, usize)| {
            row_of(build.0.plugin).is_some_and(|row| rows.contains(&row))
        };
        let builds: Vec<Build> = {
            let keys: Vec<_> = state.builds.keys().copied().filter(of_rows).collect();
            let builds = keys.iter().filter_map(|key| state.builds.remove(key));
            builds.collect()
        };
        let again = if stays_off {
            "The plugin stays off until the next read of the plugin folders"
        } else {
            "The next note starts the plugin again"
        };
        state.errors.push(format!("vst {name}: {how}. {again}"));
        drop(state);
        // The copies and the worker end outside the lock. A worker with no
        // answer still runs: its process ends here.
        drop(builds);
        process.kill();
        self.end_plugins(plugins);
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
        let parked: Vec<Copy> = {
            let mut state = self.state();
            let rows = unkept(&state);
            let builds: Vec<_> = state
                .builds
                .iter()
                .filter(|(build, _)| row_of(build.0.plugin).is_some_and(|row| rows.contains(&row)))
                .filter(|(_, build)| matches!(build, Build::Ready(_)))
                .map(|(build, _)| *build)
                .collect();
            builds
                .into_iter()
                .filter_map(|build| match state.builds.remove(&build) {
                    Some(Build::Ready(instance)) => Some(instance),
                    _ => None,
                })
                .collect()
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
                        row_of(build.0.plugin) == Some(**row) && !matches!(built, Build::Failed)
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
            Stage::Ready(plugin) => Resolved::Ready(state.number(index), Arc::clone(plugin)),
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

    /// [`Host::insert`] as the provider the audio engine takes.
    pub fn provider(&self, wait: bool) -> Arc<InsertProvider> {
        let host = self.clone();
        Arc::new(move |key, sample_rate, orbit| host.insert(key, sample_rate, orbit, wait))
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
        slot: usize,
        wait: bool,
    ) -> Option<Box<dyn OrbitInsert>> {
        let build = (key, sample_rate, slot);
        let take = |state: &mut State| match state.builds.remove(&build) {
            Some(Build::Ready(mut copy)) => {
                state.taken.retain(|_, held| held.strong_count() > 0);
                state.taken.insert(build, copy.hold());
                Some(copy.insert())
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
        slot: usize,
    ) -> Prepared {
        let (plugin, loaded) = match self.resolve(name, false) {
            Resolved::Ready(plugin, loaded) => (plugin, loaded),
            Resolved::Pending => return Prepared::Pending,
            Resolved::Missing | Resolved::Failed(_) => return Prepared::Unavailable,
        };
        if loaded.is_instrument() != instrument {
            return Prepared::Unavailable;
        }
        let preset = match preset.map(|preset| self.preset(plugin, preset)) {
            None => 0,
            Some(FoundPreset::Number(number)) => number,
            Some(FoundPreset::Pending) => return Prepared::Pending,
            Some(FoundPreset::Missing) => return Prepared::Unavailable,
        };
        let key = InsertKey { plugin, preset };
        let build = (key, sample_rate, slot);
        let state = self.state();
        match state.builds.get(&build) {
            Some(Build::Ready(_)) => Prepared::Ready(key),
            Some(Build::Running) => Prepared::Pending,
            Some(Build::Failed) => Prepared::Unavailable,
            None if state.held(&build) => Prepared::Ready(key),
            None => Prepared::Unbuilt(key),
        }
    }

    /// Loads the plugin a score names and builds it for one slot, with no
    /// wait, so the plugin is ready before the first note asks. A later
    /// [`Host::insert`] for the same slot takes the plugin. A name the host
    /// cannot serve does nothing here: the note reports the reason.
    pub fn prepare(
        &self,
        name: &str,
        preset: Option<&str>,
        instrument: bool,
        sample_rate: u32,
        slot: usize,
    ) {
        if matches!(self.resolve(name, false), Resolved::Missing) {
            return;
        }
        let request = Prepare {
            name: name.to_owned(),
            preset: preset.map(str::to_owned),
            instrument,
            sample_rate,
            slot,
        };
        self.prepare_soon(request, PREPARE_TRIES);
    }

    /// Puts the build of a prepared plugin in the line of the plugin
    /// thread. The plugin thread does one job at a time, so the build runs
    /// after the read of the folders or the load that is in the line now.
    /// A build that finds the plugin not ready goes to the end of the line
    /// again, `tries` times at most.
    fn prepare_soon(&self, request: Prepare, tries: u32) {
        let host = self.clone();
        self.enqueue(move || {
            let (plugin, loaded) = match host.resolve(&request.name, false) {
                Resolved::Ready(plugin, loaded) => (plugin, loaded),
                Resolved::Pending if tries > 0 => return host.prepare_soon(request, tries - 1),
                _ => return,
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
            // An engine holds the plugin of the slot: a spare copy would
            // stay parked.
            if host.state().held(&build) {
                return;
            }
            let claimed = Self::claim(&mut host.state(), build);
            if let Some(claimed) = claimed {
                host.finish(build, claimed);
            }
        });
    }

    /// Marks a build as started and gives what the build needs. `None`
    /// means the build exists already, or the host has no such plugin.
    fn claim(
        state: &mut State,
        build: (InsertKey, u32, usize),
    ) -> Option<(Arc<Plugin>, Option<PathBuf>)> {
        if state.builds.contains_key(&build) {
            return None;
        }
        let key = build.0;
        let row = row_of(key.plugin)?;
        let entry = state.entries.get(row)?;
        // A number from before the end of a worker is the number of no
        // plugin.
        if state.number(row) != key.plugin {
            return None;
        }
        let Stage::Ready(plugin) = &entry.stage else {
            return None;
        };
        let plugin = Arc::clone(plugin);
        let preset = match key.preset {
            0 => None,
            number => Some(state.presets.get(number as usize - 1)?.1.clone()),
        };
        // A plugin prepared for a score that no longer asks for it stays
        // parked. Keep the newest few.
        while state
            .builds
            .values()
            .filter(|build| matches!(build, Build::Ready(_)))
            .count()
            >= MAX_PARKED
        {
            let oldest = state
                .builds
                .iter()
                .filter(|(_, build)| matches!(build, Build::Ready(_)))
                .min_by_key(|(_, build)| match build {
                    Build::Ready(copy) => copy.order(),
                    _ => u64::MAX,
                })
                .map(|(build, _)| *build);
            let Some(oldest) = oldest else { break };
            state.builds.remove(&oldest);
        }
        state.builds.insert(build, Build::Running);
        Some((plugin, preset))
    }

    /// Builds the plugin of a claimed build. Runs on the plugin thread.
    fn finish(
        &self,
        build: (InsertKey, u32, usize),
        (plugin, preset): (Arc<Plugin>, Option<PathBuf>),
    ) {
        let retire = self.shared.jobs.clone();
        let (key, rate) = (build.0, build.1);
        // The host reads the preset file: a worker needs no file path.
        let preset = preset
            .map(|path| {
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
            })
            .transpose();
        let built = preset.and_then(|preset| match &plugin.source {
            Source::Here(..) => build_instance(&plugin, preset.as_deref(), key, rate, retire)
                .map(|instance| Copy::Here(Box::new(instance))),
            Source::Worker(worker, place) => {
                let running = Arc::clone(&plugin.running);
                worker
                    .build(*place, preset, key, rate, running, &self.shared.closed)
                    .map(|instance| Copy::Remote(Box::new(instance)))
            }
        });
        let mut state = self.state();
        let result = match built {
            Ok(mut copy) => {
                state.built += 1;
                copy.set_order(state.built);
                Build::Ready(copy)
            }
            Err(error) => {
                state.errors.push(format!("vst {}: {error}", plugin.name()));
                Build::Failed
            }
        };
        state.builds.insert(build, result);
    }
}

/// The row of a plugin number: see [`ROW_BITS`].
fn row_of(number: u32) -> Option<usize> {
    ((number & ((1 << ROW_BITS) - 1)) as usize).checked_sub(1)
}

/// Loads a bundle in this process and reads the parameters of each plugin
/// in the bundle. Runs on the plugin thread.
pub(crate) fn load_bundle(bundle: &Path) -> Result<Vec<Plugin>, String> {
    let module = Arc::new(Module::load(bundle)?);
    let classes = module.classes();
    if classes.is_empty() {
        return Err("the bundle has no audio plugin".into());
    }
    classes
        .into_iter()
        .map(|class| {
            let params = Loaded::new(Arc::clone(&module), &class)?.params();
            let scanned = Scanned {
                name: class.name.clone(),
                vendor: class.vendor.clone(),
                categories: class.categories.clone(),
            };
            Ok(Plugin::new(
                Source::Here(Arc::clone(&module), class),
                scanned,
                params,
            ))
        })
        .collect()
}

/// Makes one running plugin in this process. `preset` is the content of a
/// preset file. Runs on the plugin thread.
pub(crate) fn build_instance(
    plugin: &Plugin,
    preset: Option<&[u8]>,
    key: InsertKey,
    sample_rate: u32,
    retire: Sender<Job>,
) -> Result<Instance, String> {
    let Source::Here(module, class) = &plugin.source else {
        return Err("the plugin runs in a worker".into());
    };
    let loaded = Loaded::new(Arc::clone(module), class)?;
    if let Some(file) = preset {
        loaded.set_preset(file, class)?;
    }
    let running = Arc::clone(&plugin.running);
    loaded.activate(key, sample_rate, retire, running, plugin.params())
}
