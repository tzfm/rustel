//! The plugin host of the process, and the numbers for a `.vst()` request.
//!
//! A plugin library loads one time in a process, so the process has one
//! host. The host starts when a score names a plugin, when the user opens
//! the plugin list, or with [`scan_in_background`]. Before this, the
//! process loads no plugin code.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustel_audio::{InsertControls, InsertKey, InsertParam, InsertSlot, MAX_INSERT_PARAMS};
pub use rustel_vst3::{Host, ParamInfo, PluginInfo, Resolved, Status, canonical, serve};

/// Folders with plugins, in place of the standard VST3 folders of the
/// system. The form is the form of `PATH`.
pub const FOLDERS_ENV: &str = "RUSTEL_VST3_PATH";
/// The folder in the data folder with one folder of presets for each plugin.
pub const PRESETS_DIRECTORY_NAME: &str = "vst";

static HOST: OnceLock<Host> = OnceLock::new();
static USER_FOLDERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static PINNED_FOLDERS: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
static WORKER: Mutex<Option<rustel_vst3::WorkerProgram>> = Mutex::new(None);

/// The folders the host reads with no setting from the user: the folders of
/// [`FOLDERS_ENV`] when the variable is set, and the standard VST3 folders
/// of the system when not.
pub fn standard_folders() -> Vec<PathBuf> {
    if let Some(pinned) = PINNED_FOLDERS.lock().expect("plugin folders").clone() {
        return pinned;
    }
    match std::env::var_os(FOLDERS_ENV) {
        Some(listed) => std::env::split_paths(&listed)
            .filter(|path| !path.as_os_str().is_empty())
            .collect(),
        None => rustel_vst3::default_folders(),
    }
}

/// The folders the user added.
pub fn user_folders() -> Vec<PathBuf> {
    USER_FOLDERS.lock().expect("plugin folders").clone()
}

/// Every folder the host reads: the folders the user added, then
/// [`standard_folders`].
pub fn folders() -> Vec<PathBuf> {
    let mut folders = user_folders();
    folders.extend(standard_folders());
    folders
}

/// Test support: these folders take the place of [`standard_folders`], so a
/// test never reads the plugins of the machine.
#[cfg(any(test, feature = "test-support"))]
pub fn pin_standard_folders(folders: Vec<PathBuf>) {
    *PINNED_FOLDERS.lock().expect("plugin folders") = Some(folders);
    if let Some(host) = started() {
        host.scan(&self::folders());
        host.wait_idle();
    }
}

/// The folder with the preset folders: `~/.rustel/vst`.
pub fn presets_folder() -> Option<PathBuf> {
    crate::config_dir::canonical().map(|data| data.join(PRESETS_DIRECTORY_NAME))
}

/// The host, started on first use.
pub fn host() -> &'static Host {
    HOST.get_or_init(|| {
        let host = Host::new();
        if let Some(presets) = presets_folder() {
            host.set_preset_folder(presets);
        }
        host.set_worker(WORKER.lock().expect("worker program").clone());
        host.scan(&folders());
        host
    })
}

/// Sets the program that loads a plugin bundle in a process of its own for
/// the scan: the host runs `program`, `args`, then the bundle path, and the
/// program calls [`serve`] with the path. A bundle with a fault at its load
/// then ends that process and not this one. With no such program, the host
/// runs no scan.
pub fn set_worker(program: PathBuf, args: Vec<std::ffi::OsString>) {
    let worker = rustel_vst3::WorkerProgram { program, args };
    *WORKER.lock().expect("worker program") = Some(worker.clone());
    if let Some(host) = started() {
        host.set_worker(Some(worker));
    }
}

/// The host, if a score or the user asked for a plugin before.
pub fn started() -> Option<&'static Host> {
    HOST.get()
}

/// Sets the folders the user added, and reads all folders again. The read
/// runs on the plugin thread: see [`scanning`].
pub fn set_user_folders(folders: Vec<PathBuf>) {
    *USER_FOLDERS.lock().expect("plugin folders") = folders;
    if let Some(host) = started() {
        host.scan(&self::folders());
    }
}

/// Reads all folders again, with an empty scan cache: the scan reads each
/// bundle that is not loaded again, a bundle that failed too. The read
/// runs on the plugin thread: see [`scanning`].
pub fn rescan() {
    host().rescan(&folders());
}

/// The plugin names a score text asks for with `.vst("name")` or
/// `.vsti("name")`, as the text writes them. A name in a comment counts
/// too: the list says which plugins to keep loaded, and one more does no
/// harm.
pub fn names_in_source(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for call in ["vst(", "vsti("] {
        let mut rest = source;
        while let Some(at) = rest.find(call) {
            let before = rest[..at].chars().next_back();
            rest = &rest[at + call.len()..];
            // `myvst(` is a different call.
            if before.is_some_and(|char| char.is_alphanumeric() || char == '_') {
                continue;
            }
            let argument = rest.trim_start();
            let Some(quote) = argument
                .chars()
                .next()
                .filter(|char| "\"'`".contains(*char))
            else {
                continue;
            };
            if let Some(end) = argument[1..].find(quote) {
                let name = &argument[1..1 + end];
                if !name.is_empty() && !names.iter().any(|known| known == name) {
                    names.push(name.to_owned());
                }
            }
        }
    }
    names
}

/// Unloads each plugin with no name in `keep` and no running copy: see
/// [`Host::unload_unused`]. Does nothing before the host starts, and waits
/// for nothing.
pub fn unload_unused(keep: &[String]) {
    if let Some(host) = started() {
        host.unload_unused(keep);
    }
}

/// How long the end of the process waits for the plugin thread.
const EXIT_LIMIT: Duration = Duration::from_secs(10);

/// Unloads each plugin and waits for the plugin thread, [`EXIT_LIMIT`] at
/// most. A program calls this as its last step, after its outputs ended. A
/// plugin library that ends with the process while the plugin thread still
/// works in the library stops the process with a fault.
pub fn shutdown() {
    if let Some(host) = started() {
        host.shutdown(EXIT_LIMIT);
    }
}

/// True while the host reads the plugin folders. The plugin list is not
/// complete before the end of the read.
pub fn scanning() -> bool {
    started().is_some_and(Host::scanning)
}

/// The numbers the audio engine carries for a `.vst()` request.
///
/// The host loads the plugin on first use and does not wait: `Ok(None)`
/// means the plugin loads now.
pub(crate) fn controls(
    request: &rustel_voice::PluginRequest<'_>,
) -> Result<Option<InsertControls>, String> {
    let (plugin, loaded) = match host().resolve(request.name, false) {
        Resolved::Ready(plugin, loaded) => (plugin, loaded),
        Resolved::Pending => return Ok(None),
        Resolved::Missing => {
            return Err(format!("no plugin has the name '{}'", request.name));
        }
        Resolved::Failed(reason) => {
            return Err(format!("{} did not load: {reason}", request.name));
        }
    };
    let name = loaded.name();
    match (request.instrument, loaded.is_instrument()) {
        (true, false) => return Err(format!("{name} is an effect: use .vst()")),
        (false, true) => return Err(format!("{name} is an instrument: use .vsti()")),
        _ => {}
    }
    let preset = match request.preset {
        None => 0,
        Some(preset) => match host().preset(plugin, preset) {
            rustel_vst3::FoundPreset::Number(number) => number,
            // The host reads the preset folder now.
            rustel_vst3::FoundPreset::Pending => return Ok(None),
            rustel_vst3::FoundPreset::Missing => {
                return Err(format!("{name} has no preset with the name '{preset}'"));
            }
        },
    };
    let mut controls = InsertControls::new(InsertKey { plugin, preset });
    for (param, value) in request.params {
        let id = loaded
            .param(param)
            .ok_or_else(|| format!("{name} has no parameter with the name '{param}'"))?;
        let value = *value as f32;
        if !controls.push(InsertParam { id, value }) {
            return Err(format!(
                "a note carries {MAX_INSERT_PARAMS} values for a plugin at most"
            ));
        }
    }
    Ok(Some(controls))
}

/// A plugin call of a score: true for `.vsti()`, and the compare form of
/// the plugin name.
type Call = (bool, String);

/// The plugins of a chain: each effect in call order, then the instrument.
/// A plugin is its call and its preset.
type Signature = Vec<(Call, Option<String>)>;

/// The signature of a chain of a score text: see
/// [`crate::lint::plugin_chains`]. A note of the chain gives the same
/// signature: see [`ChainOwner::signature`].
fn text_signature(calls: &[crate::lint::NamedPlugin]) -> Signature {
    let plugin = |call: &crate::lint::NamedPlugin| {
        let name = canonical(&call.name);
        ((call.instrument, name), call.preset.clone())
    };
    let effects = calls.iter().filter(|call| !call.instrument);
    let effects = effects.take(rustel_audio::EFFECT_CHAIN);
    let instrument = calls.iter().rev().find(|call| call.instrument);
    effects.chain(instrument).map(plugin).collect()
}

/// The signature of each plugin chain of a score text. True marks the chain
/// of a muted lane: the lane makes no note, and keeps the chain it had
/// before its mute.
fn text_chains(source: &str) -> Vec<(Signature, bool)> {
    let chains = crate::lint::plugin_chains(source).into_iter();
    let signature = |chain: crate::lint::PluginChain| (text_signature(&chain.calls), chain.muted);
    chains.map(signature).collect()
}

/// A chain owns its copies independently of the output orbit and controls.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ChainOwner {
    lane: String,
    plugins: [Option<PluginOwner>; rustel_audio::EFFECT_CHAIN + 1],
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PluginOwner {
    binding: Option<String>,
    name: String,
    preset: Option<String>,
}

impl PluginOwner {
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            binding: value
                .get("__rustel_binding")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            name: value.get("name")?.as_str()?.to_owned(),
            preset: preset_name(value),
        })
    }

    /// True for a call of the same plugin with the same preset: one copy
    /// serves the two calls.
    fn same_plugin(&self, other: &Self) -> bool {
        canonical(&self.name) == canonical(&other.name) && self.preset == other.preset
    }
}

fn preset_name(value: &serde_json::Value) -> Option<String> {
    match value.get("preset")? {
        serde_json::Value::String(name) => Some(name.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

impl ChainOwner {
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        let mut plugins = std::array::from_fn(|_| None);
        if let Some(effects) = value.get("vst").and_then(serde_json::Value::as_array) {
            for (slot, plugin) in plugins
                .iter_mut()
                .take(rustel_audio::EFFECT_CHAIN)
                .zip(effects)
            {
                *slot = PluginOwner::from_value(plugin);
            }
        }
        plugins[rustel_audio::EFFECT_CHAIN] = value.get("vsti").and_then(PluginOwner::from_value);
        plugins.iter().any(Option::is_some).then(|| Self {
            lane: value
                .get("__rustel_lane")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            plugins,
        })
    }

    /// The plugins of the chain, as a score text shows them.
    fn signature(&self) -> Signature {
        let plugin = |(stage, plugin): (usize, &Option<PluginOwner>)| {
            let plugin = plugin.as_ref()?;
            let call = (stage == rustel_audio::EFFECT_CHAIN, canonical(&plugin.name));
            Some((call, plugin.preset.clone()))
        };
        self.plugins.iter().enumerate().filter_map(plugin).collect()
    }

    /// True when `later` is this chain with zero or more effects at its
    /// end. Each copy of this chain then serves `later` at the same place.
    fn grows_to(&self, later: &Self) -> bool {
        let mut stages = self.plugins.iter().zip(&later.plugins).enumerate();
        stages.all(|(stage, pair)| match pair {
            (Some(mine), Some(theirs)) => mine.same_plugin(theirs),
            (None, None) => true,
            (None, Some(_)) => stage < rustel_audio::EFFECT_CHAIN,
            (Some(_), None) => false,
        })
    }
}

/// A chain that left the score text has this long before it goes. In this
/// time the new score takes each chain it still has.
const CHAIN_GRACE: Duration = Duration::from_secs(5);
/// A chain with no note of the score for this long goes, when the text does
/// not account for the chain: a text does not show each chain a score
/// makes.
const CHAIN_UNSEEN_LIMIT: Duration = Duration::from_secs(600);

struct Chain {
    id: u64,
    /// The count of all uses at the last use of this chain: the order.
    used: u64,
    /// The score change at the last use.
    epoch: u64,
    seen: Instant,
}

/// What the plugin host does after a score change: see
/// [`ChainRegistry::settle`].
#[derive(Debug, Default, PartialEq)]
struct Settled {
    /// The chains that left the score.
    left: Vec<u64>,
    /// The plugin calls of the score text by name, each with its count, at
    /// the first settle after a change of a score with a text.
    calls: Option<Vec<(String, usize)>>,
}

/// The plugin chains of a score text by signature, each with its count.
#[derive(Default)]
struct Wanted {
    /// The chains of all lanes.
    all: HashMap<Signature, usize>,
    /// The chains of the lanes with no mute.
    live: HashMap<Signature, usize>,
}

/// A new chain number. One count serves each registry of the process, so
/// the host never has one number for two chains.
fn new_chain() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(rustel_audio::MAX_ORBITS as u64);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The chains of a score, each with its number for the audio engine and
/// the host.
///
/// An edit of a score often gives a chain a new owner text and leaves the
/// chain as it was: a `$:` lane above moves the lane number, a plugin call
/// above moves the call number, and one more effect at the end makes the
/// list longer. A new copy of each plugin for such an edit costs memory and
/// load time, and the old copy plays on for a time. So an owner with a new
/// text takes the number of a chain that the new score does not use:
///
/// ```text
///   1. a chain with the same plugins, when the chains with these plugins
///      are not fewer than the lanes of the text with them
///   2. a chain that left the text and grows to the chain of the owner
///   3. a new number
/// ```
///
/// With fewer chains than lanes, the owner is a new lane: a lane with a
/// rest keeps its chain. A muted lane makes no note, so its chain serves
/// the next lane that needs these plugins.
///
/// A chain that left the text goes after [`CHAIN_GRACE`], when no owner
/// took it.
struct ChainRegistry {
    owners: HashMap<ChainOwner, Chain>,
    accessed: u64,
    /// The count of score changes.
    epoch: u64,
    changed: Instant,
    /// The plugin chains of the score text. `None` for a score with no
    /// text.
    wanted: Option<Wanted>,
    /// The chains the changes took out of the text, still here.
    removed: HashMap<Signature, usize>,
    /// The host did not get the calls of the text after the last change.
    unsettled: bool,
}

impl ChainRegistry {
    const CAPACITY: usize = 4096;

    fn new() -> Self {
        Self {
            owners: HashMap::new(),
            accessed: 0,
            epoch: 0,
            changed: Instant::now(),
            wanted: None,
            removed: HashMap::new(),
            unsettled: false,
        }
    }

    /// A new score plays. `chains` are the plugin chains of its text, each
    /// with true for a muted lane.
    fn score_changed(&mut self, chains: Option<Vec<(Signature, bool)>>, now: Instant) {
        let wanted = chains.map(|chains| {
            let mut wanted = Wanted::default();
            for (chain, muted) in chains {
                if !muted {
                    *wanted.live.entry(chain.clone()).or_default() += 1;
                }
                *wanted.all.entry(chain).or_default() += 1;
            }
            wanted
        });
        self.epoch += 1;
        self.changed = now;
        self.unsettled = true;
        match (&self.wanted, &wanted) {
            (Some(before), Some(after)) => {
                for (chain, count) in &before.all {
                    let stays = after.all.get(chain).copied().unwrap_or(0);
                    if *count > stays {
                        *self.removed.entry(chain.clone()).or_default() += count - stays;
                    }
                }
            }
            // With no text on one side, the text says nothing.
            _ => self.removed.clear(),
        }
        self.wanted = wanted;
    }

    /// The count of the chains with a signature.
    fn count(&self, signature: &Signature) -> usize {
        let same = |owner: &&ChainOwner| owner.signature() == *signature;
        self.owners.keys().filter(same).count()
    }

    /// The number of the chain of an owner.
    fn id(&mut self, owner: ChainOwner, now: Instant) -> u64 {
        self.accessed += 1;
        let (used, epoch) = (self.accessed, self.epoch);
        if let Some(chain) = self.owners.get_mut(&owner) {
            (chain.used, chain.epoch, chain.seen) = (used, epoch, now);
            return chain.id;
        }
        let id = self.take_unused(&owner).unwrap_or_else(new_chain);
        // Only the text of an owner leaves here. The callback owns the
        // copies and their tails.
        if self.owners.len() == Self::CAPACITY {
            let oldest = self.owners.iter().min_by_key(|(_, chain)| chain.used);
            let oldest = oldest.map(|(owner, _)| owner.clone());
            self.owners.remove(&oldest.expect("full registry"));
        }
        let chain = Chain {
            id,
            used,
            epoch,
            seen: now,
        };
        self.owners.insert(owner, chain);
        id
    }

    /// Takes the number of a chain the score did not use since its last
    /// change, for an owner with a new text: see [`ChainRegistry`]. A chain
    /// of the same lane goes first.
    fn take_unused(&mut self, owner: &ChainOwner) -> Option<u64> {
        let signature = owner.signature();
        let lanes = self.wanted.as_ref();
        let lanes = lanes.and_then(|wanted| wanted.live.get(&signature));
        let same = self.count(&signature) >= lanes.copied().unwrap_or(0);
        let fits = |old: &ChainOwner| {
            let old = old.signature();
            match old == signature {
                true => same,
                false => self.removed.get(&old).is_some_and(|count| *count > 0),
            }
        };
        let plugins = |owner: &ChainOwner| owner.plugins.iter().flatten().count();
        let unused = self
            .owners
            .iter()
            .filter(|(old, chain)| chain.epoch < self.epoch && old.grows_to(owner) && fits(old));
        let found = unused
            .min_by_key(|(old, chain)| {
                let more = plugins(owner) - plugins(old);
                (more, old.lane != owner.lane, chain.id)
            })
            .map(|(old, _)| old.clone())?;
        let left = found.signature();
        if left != signature
            && let Some(count) = self.removed.get_mut(&left)
        {
            *count -= 1;
        }
        self.owners.remove(&found).map(|chain| chain.id)
    }

    /// The chain that goes for one chain with `signature` that left the
    /// text: a chain the score does not use, when the chains with its
    /// plugins are more than the text has. A chain with the signature goes
    /// first. A text does not show each chain, so the next choice is a
    /// chain with the plugins of the signature among its own. The chain
    /// with the last use before the change goes first: a chain at rest for
    /// a long time is more likely a lane with a rest.
    fn left_the_text(&self, signature: &Signature) -> Option<ChainOwner> {
        let wanted = |chain: &Signature| {
            let wanted = self.wanted.as_ref();
            let wanted = wanted.and_then(|wanted| wanted.all.get(chain));
            wanted.copied().unwrap_or(0)
        };
        let unused = self
            .owners
            .iter()
            .filter(|(_, chain)| chain.epoch < self.epoch);
        let extra = unused.filter_map(|(owner, chain)| {
            let own = owner.signature();
            let has = signature.iter().all(|plugin| own.contains(plugin));
            let order = (own != *signature, std::cmp::Reverse(chain.used));
            (has && self.count(&own) > wanted(&own)).then_some((order, owner))
        });
        let found = extra.min_by_key(|(order, _)| *order);
        found.map(|(_, owner)| owner.clone())
    }

    /// Takes out the chains that left the score, after their time.
    fn settle(&mut self, now: Instant) -> Settled {
        let mut settled = Settled::default();
        let epoch = self.epoch;
        let due = now.saturating_duration_since(self.changed) >= CHAIN_GRACE;
        let unseen = |chain: &Chain| {
            let rest = now.saturating_duration_since(chain.seen);
            chain.epoch < epoch && rest >= CHAIN_UNSEEN_LIMIT
        };
        let left = due && !self.removed.is_empty();
        let report = due && self.unsettled;
        // This runs on each turn of the output: most turns end here.
        if !left && !report && !self.owners.values().any(unseen) {
            return settled;
        }
        if left {
            for (signature, count) in std::mem::take(&mut self.removed) {
                for _ in 0..count {
                    let Some(owner) = self.left_the_text(&signature) else {
                        break;
                    };
                    let chain = self.owners.remove(&owner);
                    settled.left.extend(chain.map(|chain| chain.id));
                }
            }
        }
        // The plugin calls of the text, and the calls of the chains here.
        let mut calls: HashMap<Call, usize> = HashMap::new();
        for (signature, count) in self.wanted.iter().flat_map(|wanted| &wanted.all) {
            for (call, _) in signature {
                *calls.entry(call.clone()).or_default() += count;
            }
        }
        let mut held: HashMap<Call, usize> = HashMap::new();
        for (call, _) in self.owners.keys().flat_map(ChainOwner::signature) {
            *held.entry(call).or_default() += 1;
        }
        // A lane with a mute or a long rest keeps its chain: the text has
        // its calls. A chain goes when the chains with one of its plugins
        // are more than the calls of that plugin.
        let old = |owner: &ChainOwner, chain: &Chain| {
            let more = |(call, _): &(Call, Option<String>)| {
                held[call] > calls.get(call).copied().unwrap_or(0)
            };
            unseen(chain) && owner.signature().iter().any(more)
        };
        let owners = self
            .owners
            .iter()
            .filter(|(owner, chain)| old(owner, chain));
        let owners: Vec<ChainOwner> = owners.map(|(owner, _)| owner.clone()).collect();
        for owner in owners {
            let chain = self.owners.remove(&owner);
            settled.left.extend(chain.map(|chain| chain.id));
        }
        // The host ends its spare copies one time after each change, when
        // the chains that left are known.
        if report {
            self.unsettled = false;
            let mut names: Vec<(String, usize)> = Vec::new();
            for ((_, name), count) in calls {
                match names.iter_mut().find(|(known, _)| *known == name) {
                    Some((_, total)) => *total += count,
                    None => names.push((name, count)),
                }
            }
            settled.calls = self.wanted.is_some().then_some(names);
        }
        settled
    }

    /// The chains the score has, the last used first.
    fn named(&self) -> Vec<u64> {
        let mut chains: Vec<&Chain> = self.owners.values().collect();
        chains.sort_by_key(|chain| std::cmp::Reverse(chain.used));
        chains.truncate(rustel_audio::MAX_INSERT_BUSES);
        chains.iter().map(|chain| chain.id).collect()
    }

    /// The signature of each chain with its number in `chains`.
    fn signatures(&self, chains: &[u64]) -> Vec<Signature> {
        let listed = |(_, chain): &(&ChainOwner, &Chain)| chains.contains(&chain.id);
        let owners = self.owners.iter().filter(listed);
        owners.map(|(owner, _)| owner.signature()).collect()
    }
}

/// The chains of the score on the live output of the process.
fn chains() -> std::sync::MutexGuard<'static, ChainRegistry> {
    static CHAINS: OnceLock<Mutex<ChainRegistry>> = OnceLock::new();
    CHAINS
        .get_or_init(|| Mutex::new(ChainRegistry::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// The number of the chain of a note of the live output.
pub(crate) fn live_chain(value: &serde_json::Value) -> Option<u64> {
    let owner = ChainOwner::from_value(value)?;
    Some(chains().id(owner, Instant::now()))
}

/// The chains of one render to a file. A render has chains of its own: its
/// notes take no chain of the score on the live output.
pub(crate) struct RenderChains {
    chains: ChainRegistry,
    start: Instant,
}

impl RenderChains {
    pub(crate) fn new() -> Self {
        Self {
            chains: ChainRegistry::new(),
            start: Instant::now(),
        }
    }

    /// The number of the chain of a note with a plugin.
    pub(crate) fn chain(&mut self, value: &crate::ValueJson) -> Option<u64> {
        let crate::ValueJson::Raw(value) = value else {
            return None;
        };
        let owner = ChainOwner::from_value(value)?;
        Some(self.chains.id(owner, self.start))
    }

    /// Gives the event of a note the number of its chain.
    pub(crate) fn number(
        &mut self,
        mut event: rustel_audio::OnsetEvent,
        value: &crate::ValueJson,
    ) -> rustel_audio::OnsetEvent {
        event.controls.insert_chain = self.chain(value);
        event
    }
}

/// A new score plays on the live output: `source` is its text, or `None`
/// for a score with no text. The chains of the score before stay for the
/// new score to take.
pub(crate) fn score_changed(source: Option<&str>) {
    if let Some(host) = started() {
        host.score_changed();
    }
    chains().score_changed(source.map(text_chains), Instant::now());
}

/// The chains the playing score has, for the audio output: see
/// `LiveScalarDevice::set_named_insert_chains`. A chain that left the score
/// is not in the list. The host then ends the copies it made ready for
/// that chain, and the spare copies no chain of the score needs.
pub fn named_chains() -> Vec<u64> {
    let (named, settled) = {
        let mut chains = chains();
        let settled = chains.settle(Instant::now());
        (chains.named(), settled)
    };
    if let Some(host) = started() {
        if !settled.left.is_empty() {
            host.end_chains(settled.left);
        }
        if let Some(calls) = settled.calls {
            host.keep_spares(calls);
        }
    }
    named
}

/// One plugin request of a note: the kind, the slot, the plugin name and
/// the preset name.
struct Request<'a> {
    instrument: bool,
    slot: InsertSlot,
    name: &'a str,
    preset: Option<String>,
}

/// The plugin requests of the haps, each one time.
fn requests(haps: &[crate::HapJson]) -> Vec<Request<'_>> {
    let mut asked: Vec<Request<'_>> = Vec::new();
    for hap in haps {
        let crate::ValueJson::Raw(value) = &hap.value else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        let Some(chain) = live_chain(value) else {
            continue;
        };
        let instrument = object.get("vsti").map(|plugin| {
            let slot = rustel_audio::instrument_slot(chain);
            (true, slot, plugin)
        });
        // The effects are a list, in chain order.
        let effects = object.get("vst").and_then(serde_json::Value::as_array);
        let effects = effects
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(stage, plugin)| {
                let slot = rustel_audio::effect_slot(chain, stage);
                (false, slot, plugin)
            });
        for (instrument, slot, plugin) in effects.take(rustel_audio::EFFECT_CHAIN).chain(instrument)
        {
            let Some(name) = plugin.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let preset = preset_name(plugin);
            let known = asked.iter().any(|request| {
                (request.slot, request.name, &request.preset) == (slot, name, &preset)
            });
            if !known {
                asked.push(Request {
                    instrument,
                    slot,
                    name,
                    preset,
                });
            }
        }
    }
    asked
}

/// Loads the plugins the haps name and prepares each copy for its chain,
/// with no wait, so a plugin is ready before its first note plays.
pub(crate) fn prepare(haps: &[crate::HapJson], sample_rate: u32) {
    for request in requests(haps) {
        host().prepare(
            request.name,
            request.preset.as_deref(),
            request.instrument,
            sample_rate,
            request.slot,
        );
    }
}

/// The plugin requests of the haps that are not ready for their notes: a
/// plugin that loads, or a copy for a chain that is not built. `holds`
/// says if the output has the plugin on the slot already. A request the
/// host cannot serve does not count: the note reports the reason. The call
/// starts the work for each request it counts, and waits for nothing.
pub(crate) fn pending(
    haps: &[crate::HapJson],
    sample_rate: u32,
    holds: impl Fn(InsertSlot, InsertKey) -> bool,
) -> usize {
    let host = host();
    let mut pending = 0;
    for request in requests(haps) {
        let Request {
            instrument,
            slot,
            name,
            preset,
        } = request;
        match host.prepared(name, preset.as_deref(), instrument, sample_rate, slot) {
            rustel_vst3::Prepared::Ready(_) | rustel_vst3::Prepared::Unavailable => {}
            rustel_vst3::Prepared::Unbuilt(key) if holds(slot, key) => {}
            rustel_vst3::Prepared::Unbuilt(_) => {
                host.prepare(name, preset.as_deref(), instrument, sample_rate, slot);
                pending += 1;
            }
            rustel_vst3::Prepared::Pending => pending += 1,
        }
    }
    pending
}

/// Starts the host and the read of each plugin bundle with no plugin names
/// in the scan cache, as a DAW scans its plugins. The work runs on threads
/// of the host and in processes of their own, so the caller goes on at
/// once. The file `scan.json` in [`presets_folder`] keeps the result: a
/// bundle with the same files is not read again.
pub fn scan_in_background() {
    host().scan_plugins();
}

/// The bundles the scan read and the bundles of the scan in all, while
/// the scan runs: see [`scan_in_background`].
pub fn scan_progress() -> Option<(usize, usize)> {
    started().and_then(Host::scan_progress)
}

/// The plugins in their load now, one name for each bundle. Empty before
/// the host starts.
pub fn loads() -> Vec<String> {
    started().map(Host::loads).unwrap_or_default()
}

/// Prepares unassigned copies for a candidate score. Once it is evaluated,
/// its chains take these copies without disturbing the playing score.
pub fn loading(plugin: &crate::lint::NamedPlugin, sample_rate: u32, copies: usize) -> bool {
    matches!(
        host().warm(
            &plugin.name,
            plugin.preset.as_deref(),
            plugin.instrument,
            sample_rate,
            copies,
        ),
        rustel_vst3::Prepared::Pending | rustel_vst3::Prepared::Unbuilt(_)
    )
}

/// The plugin calls of the text `incoming` that need a new plugin copy when
/// the score `playing` gives way to it on the live output, each with the
/// count of the copies.
///
/// An edit that adds no chain to the text needs no copy. For each other
/// edit, a chain with a copy serves a chain of `incoming` with the same
/// plugins, also with one or more effects at its end: then only the calls
/// of those effects are new. A muted lane that played before has such a
/// chain, so the end of its mute needs no copy. A chain of the text with no
/// copy yet counts as new, and the spare copy the host has for it serves.
pub fn new_calls(playing: Option<&str>, incoming: &str) -> Vec<(crate::lint::NamedPlugin, usize)> {
    new_calls_among(playing, incoming, || {
        let held = started().map(Host::chains_with_copies);
        chains().signatures(&held.unwrap_or_default())
    })
}

/// [`new_calls`], with the signature of each chain that has a copy from
/// `real`.
fn new_calls_among(
    playing: Option<&str>,
    incoming: &str,
    real: impl FnOnce() -> Vec<Signature>,
) -> Vec<(crate::lint::NamedPlugin, usize)> {
    use crate::lint::NamedPlugin;
    let live = |source: &str| -> Vec<Vec<NamedPlugin>> {
        let chains = crate::lint::plugin_chains(source).into_iter();
        let live = chains.filter(|chain| !chain.muted);
        live.map(|chain| chain.calls).collect()
    };
    let incoming = live(incoming);
    // A text does not show each chain of a score. With no new chain in the
    // text, the chains of the score stay as they are.
    let before = playing.map(live).unwrap_or_default();
    let mut before: Vec<Signature> = before.iter().map(|chain| text_signature(chain)).collect();
    let mut added = false;
    for chain in &incoming {
        let signature = text_signature(chain);
        match before.iter().position(|old| *old == signature) {
            Some(at) => {
                before.swap_remove(at);
            }
            None => added = true,
        }
    }
    if !added {
        return Vec::new();
    }
    // A chain with the same plugins serves first.
    let mut real = real();
    let mut open = Vec::new();
    for chain in incoming {
        let signature = text_signature(&chain);
        match real.iter().position(|old| *old == signature) {
            Some(at) => {
                real.swap_remove(at);
            }
            None => open.push((chain, signature)),
        }
    }
    let effects = |chain: &Signature| {
        chain
            .iter()
            .filter(|((instrument, _), _)| !instrument)
            .count()
    };
    let plugin = |call: &NamedPlugin| text_signature(std::slice::from_ref(call));
    let mut new: Vec<(NamedPlugin, usize)> = Vec::new();
    for (chain, signature) in open {
        // Then the chain with the most effects of this chain: the same
        // effects from the start, and the same instrument.
        let count = effects(&signature);
        let serves = |old: &Signature| {
            let held = effects(old);
            held <= count && old[..held] == signature[..held] && old[held..] == signature[count..]
        };
        let found = (0..real.len())
            .filter(|at| serves(&real[*at]))
            .max_by_key(|at| effects(&real[*at]));
        let held = found.map(|at| effects(&real.swap_remove(at)));
        let chain_effects = chain.iter().filter(|call| !call.instrument);
        let chain_effects = chain_effects.take(rustel_audio::EFFECT_CHAIN);
        let instrument = chain.iter().rev().find(|call| call.instrument);
        let calls: Vec<&NamedPlugin> = match held {
            Some(held) => chain_effects.skip(held).collect(),
            None => chain_effects.chain(instrument).collect(),
        };
        for call in calls {
            match new
                .iter_mut()
                .find(|(known, _)| plugin(known) == plugin(call))
            {
                Some((_, count)) => *count += 1,
                None => new.push((call.clone(), 1)),
            }
        }
    }
    new
}

/// The problems of the plugin thread since the last call.
pub(crate) fn take_errors() -> Vec<String> {
    started().map(Host::take_errors).unwrap_or_default()
}

/// Gives a live output the plugins of the process. A note plays with no
/// effect until its plugin is ready. See `Session::attach_plugins`.
#[cfg(feature = "device-audio")]
pub(crate) fn attach(device: &rustel_audio::LiveScalarDevice) {
    device.set_insert_provider(std::sync::Arc::new(|key, sample_rate, slot| {
        started()?.insert(key, sample_rate, slot, false)
    }));
    device.set_insert_names(std::sync::Arc::new(named_chains));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chain owner: the lane, the effects and the instrument, each plugin
    /// as its call number and its name.
    fn owner(lane: &str, effects: &[(u32, &str)], instrument: Option<(u32, &str)>) -> ChainOwner {
        let plugin = |(call, name): &(u32, &str)| serde_json::json!({"name": name, "__rustel_binding": format!("p{call}")});
        let mut value = serde_json::json!({"__rustel_lane": lane});
        if !effects.is_empty() {
            value["vst"] = effects.iter().map(plugin).collect();
        }
        if let Some(instrument) = &instrument {
            value["vsti"] = plugin(instrument);
        }
        ChainOwner::from_value(&value).unwrap()
    }

    /// The chains of a score text: each chain as its effects, then its
    /// instrument. The first `muted` chains are chains of a muted lane.
    fn text_with_mutes(chains: &[&[(bool, &str)]], muted: usize) -> Option<Vec<(Signature, bool)>> {
        let plugin = |(instrument, name): &(bool, &str)| ((*instrument, canonical(name)), None);
        let chain = |(at, chain): (usize, &&[(bool, &str)])| {
            (chain.iter().map(plugin).collect(), at < muted)
        };
        Some(chains.iter().enumerate().map(chain).collect())
    }

    fn text(chains: &[&[(bool, &str)]]) -> Option<Vec<(Signature, bool)>> {
        text_with_mutes(chains, 0)
    }

    #[test]
    fn plugin_ownership_separates_patterns_and_survives_control_edits() {
        let mut registry = ChainRegistry::new();
        let now = Instant::now();
        let value = serde_json::json!({
            "__rustel_lane": "$0",
            "orbit": 1,
            "vst": [{"name": "Delay", "__rustel_binding": "p1", "mix": 0.2}]
        });
        let owner = |value: &serde_json::Value| ChainOwner::from_value(value).unwrap();
        let first = registry.id(owner(&value), now);
        let mut edited = value.clone();
        edited["orbit"] = 2.into();
        edited["vst"][0]["mix"] = 0.8.into();
        assert_eq!(registry.id(owner(&edited), now), first);
        // In one score, each lane, call and preset has a chain of its own.
        edited["__rustel_lane"] = "$1".into();
        assert_ne!(registry.id(owner(&edited), now), first);
        edited = value.clone();
        edited["vst"][0]["__rustel_binding"] = "p2".into();
        assert_ne!(registry.id(owner(&edited), now), first);
        edited = value.clone();
        edited["vst"][0]["preset"] = "Long".into();
        assert_ne!(registry.id(owner(&edited), now), first);
        assert!(ChainOwner::from_value(&serde_json::json!({"s": "sine"})).is_none());
    }

    #[test]
    fn plugin_registry_bounds_metadata_without_reusing_live_ids() {
        let mut registry = ChainRegistry::new();
        let now = Instant::now();
        let effect = |binding: usize| owner("", &[(binding as u32, "Delay")], None);
        let first = registry.id(effect(0), now);
        for binding in 1..=ChainRegistry::CAPACITY {
            registry.id(effect(binding), now);
        }
        assert_eq!(registry.owners.len(), ChainRegistry::CAPACITY);
        assert!(registry.id(effect(0), now) > first);
        assert_eq!(registry.owners.len(), ChainRegistry::CAPACITY);
    }

    /// An edit that leaves a plugin call as it is keeps the chain of the
    /// call, and so its copies: a lane above, a mute, a call above, one
    /// more effect at the end.
    #[test]
    fn an_edit_keeps_the_chains_the_new_score_still_has() {
        let mut registry = ChainRegistry::new();
        let start = Instant::now();
        let later = start + CHAIN_GRACE;
        let serum: &[(bool, &str)] = &[(true, "serum")];
        let zyklop: &[(bool, &str)] = &[(false, "ott"), (true, "zyklop")];
        let eq: &[(bool, &str)] = &[(false, "eq")];
        registry.score_changed(text(&[serum, zyklop]), start);
        let lead = registry.id(owner("$0", &[], Some((0, "serum"))), start);
        let bass = registry.id(owner("$1", &[(2, "ott")], Some((1, "zyklop"))), start);
        assert_ne!(lead, bass);
        assert_eq!(registry.named(), [bass, lead]);

        // A `$:` lane with no plugin above: each lane has a new number.
        registry.score_changed(text(&[serum, zyklop]), start);
        assert_eq!(
            registry.id(owner("$1", &[], Some((0, "serum"))), start),
            lead
        );
        let moved = owner("$2", &[(2, "ott")], Some((1, "zyklop")));
        assert_eq!(registry.id(moved, start), bass);

        // The first lane with a mute: the lanes after it move up, and the
        // muted lane keeps its chain for the end of the mute.
        registry.score_changed(text_with_mutes(&[serum, zyklop], 1), start);
        let moved = owner("$0", &[(2, "ott")], Some((1, "zyklop")));
        assert_eq!(registry.id(moved, start), bass);
        assert!(registry.settle(later + CHAIN_UNSEEN_LIMIT).left.is_empty());
        assert_eq!(registry.named(), [bass, lead]);
        registry.score_changed(text(&[serum, zyklop]), start);
        assert_eq!(
            registry.id(owner("$0", &[], Some((0, "serum"))), start),
            lead
        );
        let back = owner("$1", &[(2, "ott")], Some((1, "zyklop")));
        assert_eq!(registry.id(back, start), bass);

        // One more plugin call above: each later call has a new number.
        registry.score_changed(text(&[eq, serum, zyklop]), start);
        let drums = registry.id(owner("$0", &[(0, "eq")], None), start);
        assert!(drums != lead && drums != bass);
        assert_eq!(
            registry.id(owner("$1", &[], Some((1, "serum"))), start),
            lead
        );
        let moved = owner("$2", &[(3, "ott")], Some((2, "zyklop")));
        assert_eq!(registry.id(moved, start), bass);

        // One more effect at the end of a chain: the chain stays, and only
        // the new effect needs a copy.
        let wet: &[(bool, &str)] = &[(false, "ott"), (false, "verb"), (true, "zyklop")];
        registry.score_changed(text(&[eq, serum, wet]), start);
        let longer = owner("$2", &[(3, "ott"), (4, "verb")], Some((2, "zyklop")));
        assert_eq!(registry.id(longer, start), bass);
        assert!(registry.settle(later).left.is_empty());

        // The effect leaves again: a chain with fewer effects is a new
        // chain, and the chain before goes after its time.
        registry.score_changed(text(&[eq, serum, zyklop]), start);
        let dry = registry.id(owner("$2", &[(3, "ott")], Some((2, "zyklop"))), start);
        assert_ne!(dry, bass);
        assert!(
            registry.settle(start).left.is_empty(),
            "the chain has its time"
        );
        assert_eq!(registry.settle(later).left, [bass]);
        assert!(!registry.named().contains(&bass));

        // A chain leaves the text: it goes, and the other chains stay,
        // also with no note since the edit.
        registry.score_changed(text(&[eq, zyklop]), start);
        assert_eq!(registry.settle(later).left, [lead]);
        assert!(registry.settle(later + CHAIN_UNSEEN_LIMIT).left.is_empty());
        assert_eq!(registry.named(), [dry, drums]);
    }

    /// A chain that is new in the text takes no chain of a lane with a
    /// rest. A lane with a rest keeps its chain through each edit. A chain
    /// the text does not show goes after a long time.
    #[test]
    fn a_new_chain_takes_no_chain_of_a_lane_with_a_rest() {
        let mut registry = ChainRegistry::new();
        let start = Instant::now();
        let long = start + CHAIN_GRACE + CHAIN_UNSEEN_LIMIT;
        let lanes = |count: usize| {
            let serum: &[(bool, &str)] = &[(true, "serum")];
            text(&vec![serum; count])
        };
        registry.score_changed(lanes(2), start);
        let first = registry.id(owner("$0", &[], Some((0, "serum"))), start);
        let rest = registry.id(owner("$1", &[], Some((1, "serum"))), start);

        registry.score_changed(lanes(3), start);
        assert_eq!(
            registry.id(owner("$0", &[], Some((0, "serum"))), start),
            first
        );
        let new = registry.id(owner("$2", &[], Some((2, "serum"))), start);
        assert!(new != first && new != rest);
        // The lane with the rest plays again a long time after.
        assert!(registry.settle(long).left.is_empty());
        assert_eq!(
            registry.id(owner("$1", &[], Some((1, "serum"))), long),
            rest
        );

        // A chain that left the text and came back before the end of its
        // time is a chain of the score again.
        registry.score_changed(lanes(2), start);
        registry.score_changed(lanes(3), start);
        let back = registry.id(owner("$3", &[], Some((7, "serum"))), start);
        assert!([first, rest, new].contains(&back));
        assert!(registry.settle(start + CHAIN_GRACE).left.is_empty());
        assert_eq!(registry.owners.len(), 3);
        // The same with the owner as before: the lane is in a comment for a
        // moment. No lane at rest loses its chain.
        registry.score_changed(lanes(2), start);
        registry.score_changed(lanes(3), start);
        assert_eq!(
            registry.id(owner("$3", &[], Some((7, "serum"))), start),
            back
        );
        assert!(registry.settle(start + CHAIN_GRACE).left.is_empty());
        assert_eq!(registry.owners.len(), 3);

        // A chain from a call the text does not show: the chains with the
        // plugin are more than its calls. Such a chain goes after a long
        // time with no note.
        let mut registry = ChainRegistry::new();
        registry.score_changed(lanes(1), start);
        let shown = registry.id(owner("$0", &[], Some((0, "serum"))), start);
        let hidden = registry.id(owner("$1", &[], Some((1, "serum"))), start);
        registry.score_changed(lanes(1), start);
        assert_eq!(
            registry.id(owner("$0", &[], Some((0, "serum"))), start),
            shown
        );
        assert!(registry.settle(start + CHAIN_GRACE).left.is_empty());
        assert_eq!(registry.settle(long).left, [hidden]);

        // A score with no text says nothing: each chain stays for the limit.
        let mut registry = ChainRegistry::new();
        registry.score_changed(None, start);
        let chain = registry.id(owner("", &[(0, "ott")], None), start);
        registry.score_changed(None, start);
        let settled = registry.settle(start + CHAIN_GRACE);
        assert_eq!((settled.left.len(), settled.calls), (0, None));
        assert_eq!(registry.settle(long).left, [chain]);
    }

    /// A chain with a new preset, or with its lane removed, leaves the
    /// text: its chain goes after its time. The host gets the calls of the
    /// text one time after each change.
    #[test]
    fn a_chain_that_left_the_text_goes_after_its_time() {
        let mut registry = ChainRegistry::new();
        let start = Instant::now();
        let later = start + CHAIN_GRACE;
        let preset = |name: &str| -> (Option<Vec<(Signature, bool)>>, ChainOwner) {
            let call = ((true, canonical("serum")), Some(name.to_owned()));
            let value = serde_json::json!({
                "vsti": {"name": "Serum", "__rustel_binding": "p0", "preset": name}
            });
            (
                Some(vec![(vec![call], false)]),
                ChainOwner::from_value(&value).unwrap(),
            )
        };
        let (dark, dark_owner) = preset("Dark");
        let (soft, soft_owner) = preset("Soft");
        registry.score_changed(dark, start);
        let before = registry.id(dark_owner, start);
        assert_eq!(
            registry.settle(later).calls,
            Some(vec![("serum".into(), 1)])
        );
        assert_eq!(registry.settle(later), Settled::default());

        registry.score_changed(soft, start);
        let after = registry.id(soft_owner, start);
        assert_ne!(after, before, "a preset is a copy of its own");
        assert_eq!(registry.settle(start), Settled::default());
        let settled = registry.settle(later);
        assert_eq!(settled.left, [before]);
        assert_eq!(settled.calls, Some(vec![("serum".into(), 1)]));
        assert_eq!(registry.named(), [after]);
    }

    /// A muted lane makes no note. A new muted lane changes no chain of a
    /// lane that plays. The chain of a muted lane serves the next lane that
    /// needs its plugins.
    #[test]
    fn a_muted_lane_takes_no_chain_from_a_lane_that_plays() {
        let mut registry = ChainRegistry::new();
        let start = Instant::now();
        let serum: &[(bool, &str)] = &[(true, "serum")];
        let lane = |lane: &str, call: u32| owner(lane, &[], Some((call, "serum")));
        registry.score_changed(text(&[serum]), start);
        let lead = registry.id(lane("$0", 0), start);

        // A new muted lane above with the same plugin: its call moves the
        // call number of the lane that plays, and that lane keeps its
        // chain.
        registry.score_changed(text_with_mutes(&[serum, serum], 1), start);
        assert_eq!(registry.id(lane("$0", 1), start), lead);
        assert!(registry.settle(start + CHAIN_GRACE).left.is_empty());

        // The end of the mute: the lane never had a chain, so its chain is
        // new. The lane that plays keeps its chain.
        registry.score_changed(text(&[serum, serum]), start);
        let other = registry.id(lane("$0", 0), start);
        assert_ne!(other, lead);
        assert_eq!(registry.id(lane("$1", 1), start), lead);

        // The first lane has its mute again, and one more lane with the
        // plugin comes: the 2 chains serve the 2 lanes that play.
        registry.score_changed(text_with_mutes(&[serum, serum, serum], 1), start);
        let (first, second) = (
            registry.id(lane("$0", 1), start),
            registry.id(lane("$1", 2), start),
        );
        assert_ne!(first, second);
        assert!([lead, other].contains(&first) && [lead, other].contains(&second));
        assert!(registry.settle(start + CHAIN_GRACE).left.is_empty());
        // The end of that mute needs one chain more.
        registry.score_changed(text(&[serum, serum, serum]), start);
        let third = registry.id(lane("$0", 0), start);
        assert!(third != lead && third != other);
    }

    /// A text does not show a chain with its calls in 2 statements. Such a
    /// chain goes when one of its calls leaves the text and the new score
    /// does not use the chain.
    #[test]
    fn a_chain_the_text_does_not_show_goes_with_its_call() {
        let mut registry = ChainRegistry::new();
        let start = Instant::now();
        let serum: &[(bool, &str)] = &[(true, "serum")];
        let ott: &[(bool, &str)] = &[(false, "ott")];
        let verb: &[(bool, &str)] = &[(false, "verb")];
        registry.score_changed(text(&[serum, ott]), start);
        let before = registry.id(owner("$0", &[(1, "ott")], Some((0, "serum"))), start);
        registry.score_changed(text(&[serum, verb]), start);
        let after = registry.id(owner("$0", &[(1, "verb")], Some((0, "serum"))), start);
        assert_ne!(after, before);
        assert_eq!(registry.settle(start + CHAIN_GRACE).left, [before]);
        assert_eq!(registry.named(), [after]);
    }

    /// The score of a session with no live output is not the score that
    /// plays: an export in Studio changes no chain of the live output.
    #[test]
    fn a_session_with_no_live_output_leaves_the_live_chains() {
        let changes = chains().epoch;
        let mut session = crate::Session::new().expect("session");
        let score = r#"$: s("bd").vst("a plugin of the export")"#;
        session.evaluate(score).expect("score");
        assert_eq!(session.active_source(), Some(score));
        assert_eq!(chains().epoch, changes);
    }

    #[test]
    fn a_render_has_chains_of_its_own() {
        let value = serde_json::json!({
            "__rustel_lane": "render",
            "vst": [{"name": "Render Only", "__rustel_binding": "p0"}]
        });
        let owner = ChainOwner::from_value(&value).unwrap();
        let note = crate::ValueJson::Raw(value.clone());
        let mut render = RenderChains::new();
        let chain = render.chain(&note).expect("a chain");
        assert_eq!(render.chain(&note), Some(chain));
        assert_eq!(render.chain(&crate::ValueJson::Null), None);
        assert!(!chains().owners.contains_key(&owner));
        // The live output has a different number for the same note.
        let live = live_chain(&value).expect("a chain");
        assert_ne!(live, chain);
        assert!(chains().owners.contains_key(&owner));
    }

    #[test]
    fn only_the_new_calls_of_a_text_need_a_copy() {
        let playing = r#"
            $: note("c2").vsti("Serum 2", { macro1: 0.5 }).vst("ott")
            _$: s("bd").vst("valhalla supermassive")
        "#;
        let serum = ((true, "serum2".to_owned()), None);
        let ott = ((false, "ott".to_owned()), None);
        let valhalla = ((false, "valhallasupermassive".to_owned()), None);
        // The text and a note name a chain in the same way.
        let lead: Signature = vec![ott, serum];
        let parked: Signature = vec![valhalla];
        assert_eq!(
            text_chains(playing),
            [(lead.clone(), false), (parked.clone(), true)]
        );
        // `real` has the chains with a copy.
        let new = |real: &[&Signature], incoming: &str| -> Vec<(String, usize)> {
            let real: Vec<Signature> = real.iter().map(|chain| (*chain).clone()).collect();
            let new = new_calls_among(Some(playing), incoming, || real);
            let name = |(call, copies): (crate::lint::NamedPlugin, usize)| (call.name, copies);
            new.into_iter().map(name).collect()
        };
        let named = |calls: &[(&str, usize)]| -> Vec<(String, usize)> {
            let call = |(name, copies): &(&str, usize)| ((*name).to_owned(), *copies);
            calls.iter().map(call).collect()
        };
        // With no new chain in the text, no call is new: a value, an orbit,
        // a lane above. This holds also while no chain has a copy.
        let same = r#"
            $: s("hh*8")
            _$: s("bd").vst("valhalla supermassive")
            $: note("c3").vsti("serum 2", { macro1: 0.9 }).vst("OTT").orbit(2)
        "#;
        for real in [&[][..], &[&lead][..]] {
            assert!(new(real, playing).is_empty());
            assert!(new(real, same).is_empty());
        }
        // The end of a mute: the lane has a copy only when it played before
        // its mute.
        let unmuted = r#"
            $: note("c2").vsti("Serum 2").vst("ott")
            $: s("bd").vst("valhalla supermassive")
        "#;
        assert!(new(&[&lead, &parked], unmuted).is_empty());
        assert_eq!(
            new(&[&lead], unmuted),
            named(&[("valhalla supermassive", 1)])
        );
        // One more effect at the end of the chain.
        let longer = r#"$: note("c2").vsti("Serum 2").vst("ott").vst("verb")"#;
        assert_eq!(new(&[&lead], longer), named(&[("verb", 1)]));
        // A chain of the text with no copy yet counts too.
        assert_eq!(
            new(&[], longer),
            named(&[("ott", 1), ("verb", 1), ("Serum 2", 1)])
        );
        // One more lane with the same plugins, and a lane with a preset.
        let more = r#"
            $: note("c2").vsti("Serum 2").vst("ott")
            $: note("e2").vsti("Serum 2").vst("ott")
            $: note("g2").vsti("Serum 2", { preset: "Dark" })
        "#;
        assert_eq!(
            new(&[&lead], more),
            named(&[("ott", 1), ("Serum 2", 1), ("Serum 2", 1)])
        );
        // A chain with fewer effects, or with a different effect, is new.
        let fewer = r#"$: note("c2").vsti("Serum 2")"#;
        assert_eq!(new(&[&lead], fewer), named(&[("Serum 2", 1)]));
        let other = r#"$: note("c2").vsti("Serum 2").vst("verb")"#;
        assert_eq!(new(&[&lead], other), named(&[("verb", 1), ("Serum 2", 1)]));
        // Each arm of a stack is a chain.
        let stack = r#"$: stack(note("c").vsti("Serum 2"), note("e").vsti("Zyklop")).vst("ott")"#;
        assert_eq!(new(&[&lead], stack), named(&[("ott", 1), ("Zyklop", 1)]));
        // With no score in play, each call of a lane that plays is new.
        let all = new_calls_among(None, playing, Vec::new);
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|(_, copies)| *copies == 1));
    }

    #[test]
    fn a_score_text_gives_the_plugin_names_of_its_calls() {
        let source = r#"
            $: note("c2").vsti("Serum 2", { macro1: 0.5 }).vst( 'ott' )
            $: s("bd").vst(`valhalla supermassive`).myvst("no").vst(name)
            // .vst("ott") again, and .vsti("") with no name
        "#;
        assert_eq!(
            names_in_source(source),
            ["ott", "valhalla supermassive", "Serum 2"]
        );
    }
}
