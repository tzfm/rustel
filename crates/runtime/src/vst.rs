//! The plugin host of the process, and the numbers for a `.vst()` request.
//!
//! A plugin library loads one time in a process, so the process has one
//! host. The host starts when a score names a plugin, when the user opens
//! the plugin list, or with [`scan_in_background`]. Before this, the
//! process loads no plugin code.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustel_audio::{InsertControls, InsertSlot};
use rustel_transpiler::LaneStatement;
pub use rustel_vst3::{
    Host, ParamInfo, PluginInfo, Resolved, ScanProgress, Status, canonical, serve,
};

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
    #[cfg(test)]
    tests::HOST_CALLS.set(tests::HOST_CALLS.get() + 1);
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
    #[cfg(test)]
    tests::HOST_CALLS.set(tests::HOST_CALLS.get() + 1);
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

/// Reads all folders again: the scan reads each new bundle, and each
/// bundle with changed files. The read runs on the plugin thread: see
/// [`scanning`].
pub fn scan() {
    host().scan(&folders());
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

/// The quoted strings of a score text outside the comments. A call that
/// reads its name from a variable, as `.vst(fx)` after `const fx = 'ott'`,
/// has the name in a string: see [`whole_names`].
pub fn strings_in_source(source: &str) -> Vec<String> {
    let strings = crate::lint::string_literals(source).into_iter();
    let mut strings: Vec<String> = strings
        .map(|literal| source[literal.content].to_owned())
        .collect();
    strings.sort_unstable();
    strings.dedup();
    strings
}

/// The strings that are the whole name or the bundle name of a plugin:
/// see [`Host::whole_names`]. Such a string keeps its plugin loaded. A
/// part of a name keeps nothing, so `s('piano')` keeps no `Pianoteq 8`.
/// Empty before the host starts.
pub fn whole_names(strings: &[String]) -> Vec<String> {
    started().map_or_else(Vec::new, |host| host.whole_names(strings))
}

/// Unloads each plugin with no name in `keep` and no running copy: see
/// [`Host::unload_unused`]. Does nothing before the host starts, and waits
/// for nothing.
pub fn unload_unused(keep: &[String]) {
    if let Some(host) = started() {
        host.unload_unused(keep);
    }
}

/// True when an engine holds a copy of a plugin with no name in `keep`: see
/// [`Host::engine_holds_unkept`]. False before the host starts.
pub fn engine_holds_unkept(keep: &[String]) -> bool {
    started().is_some_and(|host| host.engine_holds_unkept(keep))
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

/// Whether the plugins need the process's main thread: on macOS, yes.
/// See [`install_main_service`] for the contract.
pub fn needs_the_main_thread() -> bool {
    rustel_vst3::needs_the_main_thread()
}

/// Registers the process's main thread as the server of the plugin
/// creation and release steps, on macOS. No-op when the service is
/// already installed or the caller is not the process's main thread.
pub fn install_main_service() {
    rustel_vst3::install_main_service();
}

/// Serves the plugin steps that arrived, on the calling thread: the main
/// thread of a program with the service installed calls this every few
/// milliseconds, also while it shuts down.
pub fn pump_main() {
    rustel_vst3::pump_main();
}

/// True while the host reads the plugin folders. The plugin list is not
/// complete before the end of the read.
pub fn scanning() -> bool {
    started().is_some_and(Host::scanning)
}

/// The numbers the audio engine carries for a `.vst()` request.
///
/// The host loads the plugin on first use and does not wait: `Ok(None)`
/// means the plugin loads now. A note drops a value the plugin does not
/// take and keeps its plugin: see [`rustel_vst3::Plugin::controls`]. The
/// reason is a notice of the conversion, so a conversion that drops its
/// notices says nothing.
pub(crate) fn controls(
    request: &rustel_voice::PluginRequest<'_>,
) -> Result<Option<InsertControls>, String> {
    let Some((key, loaded)) = host().key(request.name, request.preset, request.instrument)? else {
        return Ok(None);
    };
    let (controls, dropped) = loaded.controls(key, request.params);
    for reason in dropped {
        rustel_voice::plugin_value_skipped(request, &reason);
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

/// A lane of a score: a labeled statement, or a score with no label.
struct Lane {
    /// The number stays while an edit keeps the lane in the text.
    number: u64,
    label: String,
    body: String,
}

/// The lanes of the scores of one session.
///
/// An edit gives a lane a new place, a new label, a mute mark or a new
/// body. The lane keeps its number through such an edit: see [`pair`]. A
/// plugin note of a labeled statement carries the number, so the owner of
/// its chain keeps its lane: see [`ChainOwner`].
#[derive(Default)]
pub(crate) struct LaneTable {
    /// The lanes of the score in play, in text order.
    lanes: Vec<Lane>,
    /// The last text the session read that is not in play, with its lanes.
    /// A new lane then keeps its number from one try of an edit to the
    /// next.
    candidate: Option<(String, Vec<Lane>)>,
    /// The lanes of the score the output plays, until the output plays the
    /// score in play. A rollback puts that score back. Each of its lanes
    /// then keeps its number.
    audible: Vec<Lane>,
    /// True when the output plays the score in play.
    heard: bool,
    /// The number of the next new lane. No number comes a second time.
    next: u64,
}

impl LaneTable {
    /// Reads the lanes of a score text before its evaluation. Gives the
    /// lane number of each labeled statement, in text order, for
    /// [`rustel_transpiler::TranspileOptions::lane_numbers`]. A text with
    /// no scan gets no numbers and changes no lane, also when it goes in
    /// play.
    pub(crate) fn read(&mut self, source: &str) -> Vec<u64> {
        let Some(statements) = rustel_transpiler::lane_statements(source) else {
            self.candidate = None;
            return Vec::new();
        };
        let tried = self.candidate.as_ref();
        let tried = tried.map_or(&[][..], |(_, lanes)| lanes);
        let paired = pair(&statements, &self.lanes, tried, &self.audible);
        let lane = |(statement, number): (LaneStatement, Option<u64>)| Lane {
            number: number.unwrap_or_else(|| {
                self.next += 1;
                self.next - 1
            }),
            label: statement.label,
            body: statement.body,
        };
        let lanes: Vec<Lane> = statements.into_iter().zip(paired).map(lane).collect();
        // A score with no label has no statement that carries a number.
        let labeled = lanes.iter().filter(|lane| !lane.label.is_empty());
        let numbers = labeled.map(|lane| lane.number).collect();
        self.candidate = Some((source.to_owned(), lanes));
        numbers
    }

    /// The score with the text `source` goes in play: the lanes of the last
    /// read are now the lanes of the score in play. A read of a different
    /// text changes nothing here. A score with no read, such as a
    /// mini-notation score, has no lanes of its own. The lanes of the score
    /// before stay for a rollback when the output plays that score: see
    /// [`Self::mark_audible`].
    pub(crate) fn install(&mut self, source: &str) {
        let read = self.candidate.take_if(|(text, _)| text == source);
        if let Some((_, lanes)) = read {
            let before = std::mem::replace(&mut self.lanes, lanes);
            // The output plays the score before until it plays this one.
            if std::mem::take(&mut self.heard) {
                self.audible = before;
            }
        }
    }

    /// The output plays the score in play. A rollback then puts this score
    /// back, so the lanes of the score before go.
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn mark_audible(&mut self) {
        self.heard = true;
        self.audible.clear();
    }

    /// Forgets the last read. The label rewrite did not use its lane
    /// numbers, so no note carries them. A commit of that text then changes
    /// no lane of the score in play.
    pub(crate) fn forget_read(&mut self) {
        self.candidate = None;
    }
}

/// What rule 1, 2 or 3 of [`pair`] compares. A statement and a lane with
/// the same key fit the rule. `None` for a label that rule 3 does not pair.
fn rule_key<'a>(rule: u8, label: &'a str, body: &'a str) -> Option<(&'a str, &'a str)> {
    match rule {
        1 => Some((label, body)),
        2 => Some(("", body)),
        _ => {
            // `_lead` and `lead_` are the lane `lead` with a mute mark.
            let name = label.trim_matches('_');
            (name != "$").then_some((name, ""))
        }
    }
}

/// The most pairs of a statement and a lane that rule 4 of [`pair`]
/// compares: 512 statements with 512 lanes. The compare runs before each
/// evaluation, so its time has a limit. With more pairs, each statement
/// that rules 1 to 3 did not pair is a new lane.
const MAX_LANE_PAIRS: usize = 1 << 18;

/// Gives each statement of a new score text its lane.
///
/// `playing` has the lanes of the score in play. `tried` has the lanes of
/// the last text the session read. It gives only its new lanes: a text that
/// did not go in play moves no lane of the score in play. `audible` has the
/// lanes of the score the output plays, when that score is not in play. It
/// gives a lane by rule 1 only: a rollback reads the text of that score
/// again. The result has the number of the lane of each statement, or
/// `None` for a new lane. A statement takes a lane by the first rule that
/// fits:
///
/// ```text
///   1. the same label and the same body
///   2. the same body: the label is new, or has a new mark
///   3. the same label name, with no `_` at its ends: the body is new.
///      Not for `$`: this label names no lane.
///   4. the 2 bodies name the same plugin, or their equal start plus
///      their equal end is half of the shorter body or more
/// ```
///
/// Rules 1 to 3 pair in text order: see [`in_order`]. In rule 4 the best
/// pair goes first: the most plugin names in common, then the most equal
/// bytes, then the nearest place. An edited lane then keeps its number
/// when a new lane comes above it in the same edit. A statement with no
/// pair is a new lane, and a lane with no pair left the score.
fn pair(
    statements: &[LaneStatement],
    playing: &[Lane],
    tried: &[Lane],
    audible: &[Lane],
) -> Vec<Option<u64>> {
    let mut numbers: Vec<Option<u64>> = vec![None; statements.len()];
    // The numbers with a statement, and the numbers of the score in play.
    let mut taken = HashSet::new();
    let kept: HashSet<u64> = playing.iter().map(|lane| lane.number).collect();
    // True for a lane with no statement yet. `tried` gives its new lanes
    // only.
    let free = |taken: &HashSet<u64>, lane: &Lane, new_only: bool| {
        let kept = new_only && kept.contains(&lane.number);
        !(kept || taken.contains(&lane.number))
    };
    let known = [(playing, false), (tried, true)];
    for rule in 1..=3 {
        let heard = (rule == 1).then_some((audible, false));
        for (lanes, new_only) in known.into_iter().chain(heard) {
            // The places of the lanes and of the statements with no pair
            // yet, by key.
            let mut olds: HashMap<_, Vec<usize>> = HashMap::new();
            for (place, lane) in lanes.iter().enumerate() {
                let key = rule_key(rule, &lane.label, &lane.body);
                if let Some(key) = key.filter(|_| free(&taken, lane, new_only)) {
                    olds.entry(key).or_default().push(place);
                }
            }
            let mut news: HashMap<_, Vec<usize>> = HashMap::new();
            for (place, statement) in statements.iter().enumerate() {
                let key = rule_key(rule, &statement.label, &statement.body);
                let open = numbers[place].is_none();
                if let Some(key) = key.filter(|key| open && olds.contains_key(key)) {
                    news.entry(key).or_default().push(place);
                }
            }
            for (key, news) in news {
                for (new, old) in in_order(&news, &olds[&key]) {
                    numbers[new] = Some(lanes[old].number);
                    taken.insert(lanes[old].number);
                }
            }
        }
    }
    // Rule 4. The statements and the lanes with no pair yet.
    let open: Vec<usize> = (0..statements.len())
        .filter(|at| numbers[*at].is_none())
        .collect();
    let mut left = Vec::new();
    for (lanes, new_only) in known {
        let lanes = lanes.iter().enumerate();
        left.extend(lanes.filter(|(_, lane)| free(&taken, lane, new_only)));
    }
    let count = open.len().saturating_mul(left.len());
    if count == 0 || count > MAX_LANE_PAIRS {
        return numbers;
    }
    let left: Vec<(usize, &Lane, Vec<String>)> = left
        .into_iter()
        .map(|(place, lane)| (place, lane, plugin_names(&lane.body)))
        .collect();
    let mut pairs = Vec::new();
    for at in open {
        let statement = &statements[at];
        let names = plugin_names(&statement.body);
        for (place, lane, theirs) in &left {
            let shared = names.iter().filter(|name| theirs.contains(name)).count();
            let equal = equal_ends(&statement.body, &lane.body);
            let shorter = statement.body.len().min(lane.body.len());
            if shared > 0 || (shorter > 0 && 2 * equal >= shorter) {
                let nearest = place.abs_diff(at);
                let order = (std::cmp::Reverse(shared), std::cmp::Reverse(equal), nearest);
                pairs.push((order, at, lane.number));
            }
        }
    }
    // Pairs of the same order stay in text order.
    pairs.sort_by_key(|(order, ..)| *order);
    for (_, at, number) in pairs {
        if numbers[at].is_none() && taken.insert(number) {
            numbers[at] = Some(number);
        }
    }
    numbers
}

/// Pairs the statements and the lanes that fit one rule, each as its place
/// in its text. No pair crosses a pair, so lanes with equal text keep their
/// order: the k-th statement takes the k-th lane when the 2 lists have the
/// same length. If not, each place of the shorter list takes the nearest
/// place of the longer list that leaves a place for each place after it.
fn in_order(news: &[usize], olds: &[usize]) -> Vec<(usize, usize)> {
    let flip = news.len() > olds.len();
    let (short, long) = if flip { (olds, news) } else { (news, olds) };
    let mut pairs = Vec::with_capacity(short.len());
    let mut at = 0;
    for (count, place) in short.iter().enumerate() {
        let last = long.len() - (short.len() - count);
        // The longer list is in order: the next place is nearer until it
        // is past this one.
        while at < last && long[at + 1].abs_diff(*place) < long[at].abs_diff(*place) {
            at += 1;
        }
        pairs.push(if flip {
            (long[at], *place)
        } else {
            (*place, long[at])
        });
        at += 1;
    }
    pairs
}

/// The count of the bytes that 2 lane bodies have in common at their start
/// and at their end. No byte counts 2 times.
fn equal_ends(one: &str, other: &str) -> usize {
    let (one, other) = (one.as_bytes(), other.as_bytes());
    let equal = |(byte, same): &(&u8, &u8)| byte == same;
    let start = one.iter().zip(other).take_while(equal).count();
    let rest = one.len().min(other.len()) - start;
    let ends = one.iter().rev().zip(other.iter().rev());
    start + ends.take(rest).take_while(equal).count()
}

/// The plugin names in the text of a lane body, in their compare form.
fn plugin_names(body: &str) -> Vec<String> {
    let mut names: Vec<String> = names_in_source(body)
        .iter()
        .map(|name| canonical(name))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// A chain owns its copies independently of the output orbit and controls.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ChainOwner {
    /// `#` and the lane number for a note of a labeled statement: see
    /// [`LaneTable`]. The run-time name for a lane with no statement. Empty
    /// for a note with no lane.
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

#[derive(Clone, PartialEq)]
struct Chain {
    id: u64,
    /// The count of all uses at the last use of this chain: the order.
    used: u64,
    /// The score change at the last use.
    epoch: u64,
    seen: Instant,
}

/// The plugin chains of a score text by signature, each with its count.
#[derive(Clone, Default, PartialEq)]
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
/// chain as it was: a plugin call above moves the call number, one more
/// effect at the end makes the list longer, and a `.p("$")` call above
/// moves the name of a lane with no statement. A new copy of each plugin
/// for such an edit costs memory and load time, and the old copy plays on
/// for a time. So an owner with a new text takes the number of a chain that
/// the new score does not use:
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
///
/// Each session has a registry for its score in play, and the plan of a
/// session is a registry too: see [`Chains`]. Each render to a file has a
/// registry of its own: see [`RenderChains`].
#[derive(Clone, PartialEq)]
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

    /// Takes out the chains that left the score, after their time. Gives
    /// their numbers.
    fn settle(&mut self, now: Instant) -> Vec<u64> {
        let mut gone = Vec::new();
        let epoch = self.epoch;
        let due = now.saturating_duration_since(self.changed) >= CHAIN_GRACE;
        let unseen = |chain: &Chain| {
            let rest = now.saturating_duration_since(chain.seen);
            chain.epoch < epoch && rest >= CHAIN_UNSEEN_LIMIT
        };
        let left = due && !self.removed.is_empty();
        // This runs on each turn of the output: most turns end here.
        if !left && !self.owners.values().any(unseen) {
            return gone;
        }
        if left {
            for (signature, count) in std::mem::take(&mut self.removed) {
                for _ in 0..count {
                    let Some(owner) = self.left_the_text(&signature) else {
                        break;
                    };
                    let chain = self.owners.remove(&owner);
                    gone.extend(chain.map(|chain| chain.id));
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
            gone.extend(chain.map(|chain| chain.id));
        }
        gone
    }

    /// The chains the score has, the last used first.
    fn named(&self) -> Vec<u64> {
        let mut chains: Vec<&Chain> = self.owners.values().collect();
        chains.sort_by_key(|chain| std::cmp::Reverse(chain.used));
        chains.truncate(rustel_audio::MAX_INSERT_BUSES);
        chains.iter().map(|chain| chain.id).collect()
    }

    /// True when a chain here has the number `id`.
    fn has(&self, id: u64) -> bool {
        self.owners.values().any(|chain| chain.id == id)
    }

    /// Brings a plan up to date with `playing`, the registry of the score
    /// in play. The plan gets each chain that score made after the plan, as
    /// a chain the planned score did not use. A chain of the plan with the
    /// same owner gives way: an engine holds the copies of the chain in
    /// play. Gives each chain that gave way and that only the plan had.
    fn catch_up(&mut self, playing: &Self) -> Vec<u64> {
        let known: HashSet<u64> = self.owners.values().map(|chain| chain.id).collect();
        let before = self.epoch.saturating_sub(1);
        let mut ended = Vec::new();
        for (owner, chain) in &playing.owners {
            if known.contains(&chain.id) {
                continue;
            }
            let chain = Chain {
                epoch: chain.epoch.min(before),
                ..chain.clone()
            };
            let gave_way = self.owners.insert(owner.clone(), chain);
            let only_here = gave_way.filter(|chain| !playing.has(chain.id));
            ended.extend(only_here.map(|chain| chain.id));
        }
        self.accessed = self.accessed.max(playing.accessed);
        ended
    }
}

/// The chains of the scores of one session.
///
/// The session plans the chains of a score text before the text goes in
/// play: see [`plan`]. The plan is a registry of its own, so a text that
/// does not go in play changes no chain of the score in play. The names
/// source of the live output of the session reads the same chains: see
/// `Session::attach_plugins`.
pub(crate) struct Chains {
    /// The chains of the score in play.
    playing: ChainRegistry,
    /// The last text the session planned that is not in play, with the
    /// registry as the commit of that text leaves it.
    plan: Option<(String, ChainRegistry)>,
}

impl Chains {
    pub(crate) fn new() -> Self {
        Self {
            playing: ChainRegistry::new(),
            plan: None,
        }
    }

    /// Plans the chains of a score text that is not in play. `chains` are
    /// the plugin chains of the text. Gives the registry of the plan: a
    /// copy of the registry of the score in play, with the score change of
    /// the text. Each note of the text takes the number of its chain there.
    /// A session with a plan starts from that plan, so a new lane keeps its
    /// number from one try of an edit to the next. Gives also the chains
    /// the plan no longer has: see [`ChainRegistry::catch_up`].
    fn plan(
        &mut self,
        source: &str,
        chains: Option<Vec<(Signature, bool)>>,
        now: Instant,
    ) -> (&mut ChainRegistry, Vec<u64>) {
        let (mut planned, ended) = match self.plan.take() {
            Some((_, mut last)) => {
                let ended = last.catch_up(&self.playing);
                (last, ended)
            }
            None => (self.playing.clone(), Vec::new()),
        };
        planned.score_changed(chains, now);
        let (_, planned) = self.plan.insert((source.to_owned(), planned));
        (planned, ended)
    }

    /// A new score plays: see [`score_changed`]. Gives the chains of the
    /// plan that end with this change.
    fn score_changed(&mut self, source: Option<&str>, now: Instant) -> Vec<u64> {
        match self.plan.take_if(|(text, _)| Some(text.as_str()) == source) {
            // The plan made the score change of this text.
            Some((_, mut planned)) => {
                let ended = planned.catch_up(&self.playing);
                self.playing = planned;
                ended
            }
            None => {
                self.playing.score_changed(source.map(text_chains), now);
                Vec::new()
            }
        }
    }

    /// Drops the plan. Gives the chains only the plan made.
    fn forget(&mut self) -> Vec<u64> {
        let planned = self.plan.take().map(|(_, planned)| planned);
        let chains = planned.iter().flat_map(|planned| planned.owners.values());
        let made = chains.filter(|chain| !self.playing.has(chain.id));
        made.map(|chain| chain.id).collect()
    }

    /// Takes out the chains that left the score in play, after their time:
    /// see [`ChainRegistry::settle`]. A plan has each chain of the score in
    /// play, with the number the host knows. A chain that goes here would
    /// come back with the plan, after the host ended its copies. So no
    /// chain goes while the session has a plan.
    fn settle(&mut self, now: Instant) -> Vec<u64> {
        match self.plan {
            Some(_) => Vec::new(),
            None => self.playing.settle(now),
        }
    }
}

/// Takes the lock of the chains of a session. A panic of a thread with the
/// lock does not end the plugins: the next caller reads the chains as that
/// thread left them.
fn locked(chains: &Mutex<Chains>) -> std::sync::MutexGuard<'_, Chains> {
    chains.lock().unwrap_or_else(|error| error.into_inner())
}

/// The number of the chain of a note of the score in play of a session.
/// `chains` are the chains of that session.
pub(crate) fn live_chain(chains: &Mutex<Chains>, value: &serde_json::Value) -> Option<u64> {
    let owner = ChainOwner::from_value(value)?;
    Some(locked(chains).playing.id(owner, Instant::now()))
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

/// A new score plays in a session: `source` is its text, or `None` for a
/// score with no text. The chains of the score before stay for the new
/// score to take. When the session planned this text, the chains of the
/// plan are the chains of the score, so each note has the number the plan
/// gave: see [`plan`]. A plan of a different text stays.
pub(crate) fn score_changed(chains: &Mutex<Chains>, source: Option<&str>) {
    let ended = locked(chains).score_changed(source, Instant::now());
    end_chains(ended);
}

/// Plans the chains of the score text `source` before the text goes in
/// play. `haps` are notes of the text. Gives the plugin copies the notes
/// need, each for the chain its note has when the text is in play: see
/// [`ensure`]. The plan changes no chain of the score in play, and the
/// commit of the text takes the plan: see [`score_changed`]. A new plan
/// starts from the plan before. A new lane then keeps its chain, and so its
/// copies, from one try of an edit to the next.
///
/// No chain of the score in play goes while the session has a plan. The
/// session ends each plan: with the commit of its text, with a new plan, or
/// with [`forget_plan`].
// Only the session of a live output plans: see `Session::reload_live`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn plan(chains: &Mutex<Chains>, source: &str, haps: &[crate::HapJson]) -> Vec<Want> {
    let text = text_chains(source);
    let now = Instant::now();
    let mut chains = locked(chains);
    let (planned, ended) = chains.plan(source, Some(text), now);
    let needed = wants(haps, |value| {
        let owner = ChainOwner::from_value(value)?;
        Some(planned.id(owner, now))
    });
    drop(chains);
    end_chains(ended);
    needed
}

/// Drops the plan of a session: its text does not go in play. The host
/// ends the copies of the chains only the plan made.
// Only the session of a live output plans: see `Session::reload_live`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn forget_plan(chains: &Mutex<Chains>) {
    let made = locked(chains).forget();
    end_chains(made);
}

/// Ends the copies the host made ready for chains that no score has now:
/// see [`Host::end_chains`].
fn end_chains(chains: Vec<u64>) {
    if !chains.is_empty()
        && let Some(host) = started()
    {
        host.end_chains(chains);
    }
}

/// The chains the playing score has, for the audio output: see
/// `LiveScalarDevice::set_named_insert_chains`. A chain that left the score
/// is not in the list. The host then ends the copies it made ready for
/// that chain.
// Only the names source of a live output asks: see `attach`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn named_chains(chains: &Mutex<Chains>) -> Vec<u64> {
    let (named, left) = {
        let mut chains = locked(chains);
        let left = chains.settle(Instant::now());
        (chains.playing.named(), left)
    };
    end_chains(left);
    named
}

/// One plugin copy the notes of a score need: the slot of the copy, and the
/// plugin and the preset by name.
pub(crate) struct Want {
    instrument: bool,
    slot: InsertSlot,
    name: String,
    preset: Option<String>,
}

impl Want {
    /// The name of the plugin, as the score writes it.
    #[cfg(feature = "device-audio")]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// The plugin copies the notes of the haps need, each one time, in the
/// order of the notes. `chain_of` gives the number of the chain of a note
/// value: [`live_chain`] for a note of the score in play, and the registry
/// of a plan for a note of a text that is not in play.
pub(crate) fn wants(
    haps: &[crate::HapJson],
    mut chain_of: impl FnMut(&serde_json::Value) -> Option<u64>,
) -> Vec<Want> {
    let mut wants: Vec<Want> = Vec::new();
    for hap in haps {
        let crate::ValueJson::Raw(value) = &hap.value else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        let Some(chain) = chain_of(value) else {
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
            let known = wants
                .iter()
                .any(|want| (want.slot, want.name.as_str(), &want.preset) == (slot, name, &preset));
            if !known {
                wants.push(Want {
                    instrument,
                    slot,
                    name: name.to_owned(),
                    preset,
                });
            }
        }
    }
    wants
}

/// Makes the host build each copy in `wants` that is not ready, with no
/// wait, at the sample rate of the output. The builds run in the order of
/// the list, so a caller gives the copies of the lanes that sound first.
/// Gives the copies that are not ready: a plugin in its load, or a copy in
/// its build. A copy the host cannot make is not in the list: the note
/// reports the reason. A copy with a build in the line gets no second
/// build, so a caller looks as often as it needs.
pub(crate) fn ensure(wants: &[Want], sample_rate: u32) -> Vec<&Want> {
    let mut pending = Vec::new();
    for want in wants {
        let Want {
            instrument,
            slot,
            name,
            preset,
        } = want;
        let (host, preset) = (host(), preset.as_deref());
        match host.prepared(name, preset, *instrument, sample_rate, *slot) {
            rustel_vst3::Prepared::Ready(_) | rustel_vst3::Prepared::Unavailable(_) => {}
            // The build starts now, or behind the load of the plugin.
            rustel_vst3::Prepared::Unbuilt(_) | rustel_vst3::Prepared::Pending => {
                host.prepare(name, preset, *instrument, sample_rate, *slot);
                pending.push(want);
            }
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

/// Where the scan is, while it runs: see [`scan_in_background`].
pub fn scan_progress() -> Option<ScanProgress> {
    started().and_then(Host::scan_progress)
}

/// The plugins in their load now, one name for each bundle. Empty before
/// the host starts.
pub fn loads() -> Vec<String> {
    started().map(Host::loads).unwrap_or_default()
}

/// The problems of the plugin thread since the last call.
pub(crate) fn take_errors() -> Vec<String> {
    started().map(Host::take_errors).unwrap_or_default()
}

/// Gives a live output the plugins of the process. A note plays with no
/// effect until its plugin is ready. The output asks `chains`, the chains
/// of its session, for the chains the playing score has. See
/// `Session::attach_plugins`.
#[cfg(feature = "device-audio")]
pub(crate) fn attach(
    device: &rustel_audio::LiveScalarDevice,
    chains: &std::sync::Arc<Mutex<Chains>>,
) {
    device.set_insert_provider(std::sync::Arc::new(|key, sample_rate, slot| {
        started()?.insert(key, sample_rate, slot, false)
    }));
    let chains = std::sync::Arc::clone(chains);
    device.set_insert_names(std::sync::Arc::new(move || named_chains(&chains)));
}

#[cfg(test)]
mod tests {
    use super::*;

    // The calls of this thread to `host()` and `started()`. The host serves
    // the whole process, so a test reads the calls of its own thread.
    std::thread_local! {
        pub(super) static HOST_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

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

    /// Reads a score text and puts the score in play. Gives the lane number
    /// of each labeled statement.
    fn play(lanes: &mut LaneTable, score: &str) -> Vec<u64> {
        let numbers = lanes.read(score);
        lanes.install(score);
        numbers
    }

    /// A hap of the first cycle with the value of a note.
    fn hap(value: &serde_json::Value) -> crate::HapJson {
        let cycle = crate::SpanJson {
            begin: "0".into(),
            end: "1".into(),
        };
        crate::HapJson {
            whole: Some(cycle.clone()),
            part: cycle,
            value: crate::ValueJson::Raw(value.clone()),
            context: Vec::new(),
            has_onset: true,
            show: String::new(),
        }
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
    /// more effect at the end. The lanes here have no statement, so a lane
    /// above moves their names. The plan of an edit gives its chains before
    /// the edit is in play.
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

        // A `$` lane with no plugin above: each lane has a new name.
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
        assert!(registry.settle(later + CHAIN_UNSEEN_LIMIT).is_empty());
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
        assert!(registry.settle(later).is_empty());

        // The effect leaves again: a chain with fewer effects is a new
        // chain, and the chain before goes after its time.
        registry.score_changed(text(&[eq, serum, zyklop]), start);
        let dry = registry.id(owner("$2", &[(3, "ott")], Some((2, "zyklop"))), start);
        assert_ne!(dry, bass);
        assert!(registry.settle(start).is_empty(), "the chain has its time");
        assert_eq!(registry.settle(later), [bass]);
        assert!(!registry.named().contains(&bass));

        // A chain leaves the text: it goes, and the other chains stay,
        // also with no note since the edit.
        registry.score_changed(text(&[eq, zyklop]), start);
        assert_eq!(registry.settle(later), [lead]);
        assert!(registry.settle(later + CHAIN_UNSEEN_LIMIT).is_empty());
        assert_eq!(registry.named(), [dry, drums]);

        // The plan of a text that is not in play changes no chain of the
        // score in play. The text has one more lane. Its next try has one
        // more call above that lane: the 2 plans give the lane one number.
        let mut chains = Chains {
            playing: registry,
            plan: None,
        };
        let before = chains.playing.clone();
        let kept = owner("$2", &[(3, "ott")], Some((2, "zyklop")));
        let (planned, _) = chains.plan("a lane more", text(&[eq, zyklop, serum]), start);
        let new = planned.id(owner("$3", &[], Some((4, "serum"))), start);
        assert!(!before.has(new));
        assert_eq!(planned.id(kept.clone(), start), dry);
        let moved = owner("$4", &[], Some((5, "serum")));
        let again = || text(&[eq, eq, zyklop, serum]);
        let (planned, ended) = chains.plan("a call more", again(), start);
        assert_eq!(planned.id(moved.clone(), start), new);
        assert!(ended.is_empty() && chains.playing == before);

        // The score in play makes a chain for the new lane while the
        // session has the plan. An engine holds the copies of that chain:
        // the plan takes it, and the chain the plan made ends.
        let sounding = chains.playing.id(moved.clone(), start);
        let (planned, ended) = chains.plan("a call more", again(), start);
        assert_eq!(planned.id(moved.clone(), start), sounding);
        assert_eq!(ended, [new]);

        // A different text goes in play: the plan stays. The text of the
        // plan goes in play: each note has the number the plan gave, and a
        // chain the score in play made after the plan stays.
        let late = owner("$5", &[(6, "verb")], None);
        let other = chains.playing.id(late.clone(), start);
        assert!(chains.score_changed(Some("s('bd')"), start).is_empty());
        assert!(chains.plan.is_some());
        assert!(chains.score_changed(Some("a call more"), start).is_empty());
        assert!(chains.plan.is_none());
        assert_eq!(chains.playing.id(moved, start), sounding);
        assert_eq!(chains.playing.id(kept, start), dry);
        assert_eq!(chains.playing.id(late, start), other);
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
        assert!(registry.settle(long).is_empty());
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
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        assert_eq!(registry.owners.len(), 3);
        // The same with the owner as before: the lane is in a comment for a
        // moment. No lane at rest loses its chain.
        registry.score_changed(lanes(2), start);
        registry.score_changed(lanes(3), start);
        assert_eq!(
            registry.id(owner("$3", &[], Some((7, "serum"))), start),
            back
        );
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
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
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        assert_eq!(registry.settle(long), [hidden]);

        // A score with no text says nothing: each chain stays for the limit.
        let mut registry = ChainRegistry::new();
        registry.score_changed(None, start);
        let chain = registry.id(owner("", &[(0, "ott")], None), start);
        registry.score_changed(None, start);
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        assert_eq!(registry.settle(long), [chain]);
    }

    /// A chain with a new preset, or with its lane removed, leaves the
    /// text: its chain goes after its time. No chain goes while the session
    /// has a plan.
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
        assert!(registry.settle(later).is_empty());

        registry.score_changed(soft, start);
        let after = registry.id(soft_owner, start);
        assert_ne!(after, before, "a preset is a copy of its own");
        assert!(registry.settle(start).is_empty());

        // The session plans a text with one more preset. The plan has the
        // chain that left, so no chain goes while the session has the plan.
        let mut chains = Chains {
            playing: registry,
            plan: None,
        };
        let (hard, hard_owner) = preset("Hard");
        let (planned, _) = chains.plan("a preset more", hard, start);
        let made = planned.id(hard_owner, start);
        assert!(chains.settle(later).is_empty());
        // The session forgets the plan: the chain only the plan made ends,
        // and the chain that left goes.
        assert_eq!(chains.forget(), [made]);
        assert_eq!(chains.settle(later), [before]);
        assert_eq!(chains.playing.named(), [after]);
    }

    /// A lane keeps its number through an edit. A muted lane makes no note.
    /// A new muted lane changes no chain of a lane that plays. The chain of
    /// a muted lane serves the next lane that needs its plugins.
    #[test]
    fn a_muted_lane_takes_no_chain_from_a_lane_that_plays() {
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let start = Instant::now();
        let serum: &[(bool, &str)] = &[(true, "serum")];
        let lane = |lane: u64, call: u32| owner(&format!("#{lane}"), &[], Some((call, "serum")));
        let lead = r#"$: note("c2").vsti("serum")"#;
        assert_eq!(play(&mut lanes, lead), [0]);
        registry.score_changed(text(&[serum]), start);
        let chain = registry.id(lane(0, 0), start);

        // A new muted lane above with the same plugin: the lane that plays
        // keeps its number and its chain. Only its call number moves.
        let muted = format!("_$: note(\"e2\").vsti(\"serum\")\n{lead}");
        assert_eq!(play(&mut lanes, &muted), [1, 0]);
        registry.score_changed(text_with_mutes(&[serum, serum], 1), start);
        assert_eq!(registry.id(lane(0, 1), start), chain);
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());

        // The end of the mute: a mute mark changes no lane number. The lane
        // never had a chain, so its chain is new.
        let open = muted.replacen("_$", "$", 1);
        assert_eq!(play(&mut lanes, &open), [1, 0]);
        registry.score_changed(text(&[serum, serum]), start);
        let other = registry.id(lane(1, 0), start);
        assert_ne!(other, chain);
        assert_eq!(registry.id(lane(0, 1), start), chain);

        // The first lane has its mute again, and one more lane with the
        // plugin comes: the chain of the muted lane serves the new lane.
        let more = format!("{muted}\n$: note(\"g2\").vsti(\"serum\")");
        assert_eq!(play(&mut lanes, &more), [1, 0, 2]);
        registry.score_changed(text_with_mutes(&[serum, serum, serum], 1), start);
        assert_eq!(registry.id(lane(0, 1), start), chain);
        assert_eq!(registry.id(lane(2, 2), start), other);
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        // The end of that mute needs one chain more.
        assert_eq!(play(&mut lanes, &more.replacen("_$", "$", 1)), [1, 0, 2]);
        registry.score_changed(text(&[serum, serum, serum]), start);
        let third = registry.id(lane(1, 0), start);
        assert!(third != chain && third != other);

        // 2 lanes with equal text keep their order below a new lane, and an
        // edit of the first one leaves the number of the second one.
        let mut lanes = LaneTable::default();
        let (hat, twin) = (r#"$: s("hh")"#, r#"$: s("bd").vst("ott")"#);
        assert_eq!(play(&mut lanes, &format!("{twin}\n{twin}")), [0, 1]);
        assert_eq!(
            play(&mut lanes, &format!("{hat}\n{twin}\n{twin}")),
            [2, 0, 1]
        );
        let score = format!("{hat}\n$: s(\"bd*2\").vst(\"ott\")\n{twin}");
        assert_eq!(play(&mut lanes, &score), [2, 0, 1]);
        // An edit of a lane and a new lane with its plugin above, in one
        // evaluation: the edited lane keeps its number.
        let score = format!(
            "$: s(\"cp\").vst(\"ott\")\n{}",
            score.replace("bd*2", "bd*4")
        );
        assert_eq!(play(&mut lanes, &score), [3, 2, 0, 1]);
        // A new label, then a mute mark on it.
        let score = score.replace(r#"$: s("bd*4")"#, r#"kick: s("bd*4")"#);
        assert_eq!(play(&mut lanes, &score), [3, 2, 0, 1]);
        assert_eq!(
            play(&mut lanes, &score.replace("kick:", "_kick:")),
            [3, 2, 0, 1]
        );
        // A score with no label is one lane, and keeps it with a label.
        let mut lanes = LaneTable::default();
        let bare = r#"note("c2").vsti("serum")"#;
        assert!(play(&mut lanes, bare).is_empty());
        assert_eq!(play(&mut lanes, &format!("score: {bare}")), [0]);
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
        assert_eq!(registry.settle(start + CHAIN_GRACE), [before]);
        assert_eq!(registry.named(), [after]);
    }

    /// The score of a session with no live output is not the score that
    /// plays: an export in Studio changes no chain of the live output, and
    /// only its render calls the plugin host. Each session has chains of its
    /// own.
    #[test]
    fn a_session_with_no_live_output_leaves_the_live_chains() {
        let calls = HOST_CALLS.get();
        let score = r#"$: s("bd").vst("a plugin of the export")"#;
        // The chain of the first note, as the conversion of a live note
        // gives the number.
        let chain = |session: &mut crate::Session| {
            let events = session.schedule_audio_at(0.0, 48_000).expect("events");
            events[0].controls.insert_chain.expect("a chain")
        };
        let mut playing = crate::Session::new().expect("session");
        playing.evaluate(score).expect("score");
        let mut export = crate::Session::new().expect("session");
        export.evaluate(score).expect("score");
        assert_eq!(export.active_source(), Some(score));
        // The 2 sessions share no registry: the same note has 2 chains.
        assert_ne!(chain(&mut export), chain(&mut playing));
        // A warm and a count of the plugins of the export call no host,
        // also with a sample library.
        #[cfg(feature = "device-audio")]
        export.set_sample_library_for_test(crate::samples::SampleLibrary::empty().into());
        export.kick_sample_loads(2.0);
        let ceiling = Duration::from_millis(50);
        assert_eq!(export.plugins_pending(0.0, 2.0, ceiling), 0);
        // An edit of the export waits for no plugin, and a launch prepares
        // none.
        #[cfg(feature = "device-audio")]
        {
            let edit = format!("{score}\n$: s(\"hh\").vst(\"a plugin of the edit\")");
            let stopped = std::sync::atomic::AtomicBool::new(false);
            let reload = export.reload_live(&edit, false, &stopped, || 0.5);
            assert!(matches!(reload, Ok(crate::LiveReload::Installed(_))));
            let prepared = export.prepare_reload(score, false, 0.0, &stopped, || 0.6);
            assert!(!prepared.expect("prepare"));
        }
        assert_eq!(HOST_CALLS.get(), calls, "the export called the host");
    }

    /// A render has chains of its own. The plan of a text has chains of its
    /// own too. The plan gives the plugin copies its notes need.
    #[test]
    fn a_render_has_chains_of_its_own() {
        let value = serde_json::json!({
            "__rustel_lane": "render",
            "vst": [{"name": "Render Only", "__rustel_binding": "p0"}]
        });
        let note = crate::ValueJson::Raw(value.clone());
        let mut render = RenderChains::new();
        let chain = render.chain(&note).expect("a chain");
        assert_eq!(render.chain(&note), Some(chain));
        assert_eq!(render.chain(&crate::ValueJson::Null), None);
        // A session has a different number for the same note.
        let live = Mutex::new(Chains::new());
        let playing = live_chain(&live, &value).expect("a chain");
        assert_ne!(playing, chain);

        // The plan of a text gives each copy its notes need, one time. A
        // note of the score in play keeps its chain. A new note takes a
        // number of the plan, and makes no chain in the score in play.
        let mut new = value.clone();
        new["__rustel_lane"] = "plan".into();
        new["vsti"] = serde_json::json!({"name": "Tone", "__rustel_binding": "p1", "preset": 2});
        let needed = plan(&live, "a text", &[hap(&value), hap(&new), hap(&new)]);
        let slots: Vec<InsertSlot> = needed.iter().map(|want| want.slot).collect();
        let made = slots[1].chain;
        let (effect, tone) = (InsertSlot::effect(made, 0), InsertSlot::instrument(made));
        assert_eq!(slots, [InsertSlot::effect(playing, 0), effect, tone]);
        let tone = &needed[2];
        assert!(tone.instrument && !needed[1].instrument);
        assert_eq!(tone.name, "Tone");
        assert_eq!(tone.preset.as_deref(), Some("2"));
        assert!(!locked(&live).playing.has(made));
        forget_plan(&live);
        assert!(locked(&live).plan.is_none());
    }

    #[test]
    fn a_score_text_gives_the_plugin_names_of_its_calls_and_its_strings() {
        let source = r#"
            $: note("c2").vsti("Serum 2", { macro1: 0.5 }).vst( 'ott' )
            $: s("bd").vst(`valhalla supermassive`).myvst("no").vst(name)
            // .vst("ott") again, and .vsti("") with no name
        "#;
        assert_eq!(
            names_in_source(source),
            ["ott", "valhalla supermassive", "Serum 2"]
        );
        // Each string outside the comments counts, as a name in a variable. A
        // string in a comment does not.
        let variable = "const fx = 'Pro-Q 3' // 'Vital'\n$: s('bd').vst(fx) // .vst('OTT')";
        assert_eq!(names_in_source(variable), ["OTT"]);
        assert_eq!(strings_in_source(variable), ["Pro-Q 3", "bd"]);
    }

    /// The rules of [`pair`], each before the next one.
    #[test]
    fn a_statement_takes_its_lane_by_the_first_rule_that_fits() {
        // Rule 1 before rule 2: a mute mark stays with its lane when 2 lanes
        // with one body change places.
        let mut lanes = LaneTable::default();
        assert_eq!(play(&mut lanes, "$: s(\"bd\")\n_$: s(\"bd\")"), [0, 1]);
        assert_eq!(play(&mut lanes, "_$: s(\"bd\")\n$: s(\"bd\")"), [1, 0]);
        // Rule 2 before rule 3: the body is the lane when 2 lanes change
        // labels.
        let mut lanes = LaneTable::default();
        let score = "lead: note(\"c e\")\nbass: note(\"c1\")";
        assert_eq!(play(&mut lanes, score), [0, 1]);
        let score = "lead: note(\"c1\")\nbass: note(\"c e\")";
        assert_eq!(play(&mut lanes, score), [1, 0]);
        // Rule 3: a new body under the label name, with a mute mark at
        // each end. The label `$` is no name.
        let score = "_lead: s(\"hh*8\")\nbass_: s(\"cp\")\n$: note(\"g\")";
        assert_eq!(play(&mut lanes, score), [1, 0, 2]);
        let score = score.replace("note(\"g\")", "s(\"sd*4\").gain(.5)");
        assert_eq!(play(&mut lanes, &score), [1, 0, 3]);

        // Rule 4: a lane with half of its text is the same lane. A lane
        // with less is a new lane.
        let mut lanes = LaneTable::default();
        assert_eq!(play(&mut lanes, r#"$: note("c e g").lpf(800)"#), [0]);
        assert_eq!(play(&mut lanes, r#"$: note("c e g a").lpf(800)"#), [0]);
        assert_eq!(play(&mut lanes, r#"$: s("hh*8").gain(.5)"#), [1]);
        assert_eq!(equal_ends("aaaa", "aaaaaa"), 4);
        // A text with no code has no lane of the score before.
        assert!(play(&mut lanes, "// later").is_empty());
        assert_eq!(play(&mut lanes, r#"$: s("hh*8").gain(.5)"#), [3]);
        // The pair with the most plugin names goes before the pair with the
        // most equal bytes. A name ignores upper case.
        let mut lanes = LaneTable::default();
        let score = r#"$: note("c e").vsti("serum").vst("ott")"#;
        assert_eq!(play(&mut lanes, score), [0]);
        let score = "$: s(\"bd\").vsti(\"Serum\").vst(\"OTT\")\n$: note(\"c e\").vsti(\"serum\")";
        assert_eq!(play(&mut lanes, score), [0, 1]);
        // Then the nearest place: the last lane takes the lane at its place.
        let mut lanes = LaneTable::default();
        let score = "$: note(\"c\")\n$: note(\"e\")\n$: s(\"sd\")";
        assert_eq!(play(&mut lanes, score), [0, 1, 2]);
        let score = "$: s(\"hh\")\n$: note(\"e\")\n$: s(\"cp\")\n$: note(\"c\")";
        assert_eq!(play(&mut lanes, score), [3, 1, 2, 0]);

        // A text that does not go in play: its new lane keeps its number at
        // the next read, also with an edit.
        let mut lanes = LaneTable::default();
        let (lead, clap) = (
            r#"lead: note("c2").vsti("serum")"#,
            r#"$: s("cp").vst("ott")"#,
        );
        assert_eq!(play(&mut lanes, lead), [0]);
        let tried = format!("{lead}\n{clap}");
        assert_eq!(lanes.read(&tried), [0, 1]);
        assert_eq!(lanes.read(&tried.replace("cp", "cp*2")), [0, 1]);
        // A score with no read takes no lanes of a different text.
        lanes.install("bd sd");
        assert_eq!(lanes.lanes.len(), 1);
        assert_eq!(play(&mut lanes, &tried), [0, 1]);
        // A text with no scan changes no lane when it goes in play.
        let deep = format!(
            "$: {}",
            "(".repeat(rustel_transpiler::MAX_SOURCE_NESTING + 1)
        );
        assert!(play(&mut lanes, &deep).is_empty());
        assert_eq!(play(&mut lanes, &tried), [0, 1]);
        // A text that does not go in play moves no lane of the score in
        // play: the lane that comes back keeps its number.
        let tried = format!("{lead}\n$: s(\"hh\")");
        assert_eq!(lanes.read(&tried), [0, 1]);
        let back = format!("{tried}\n{}", clap.replace("cp", "cp*4"));
        assert_eq!(lanes.read(&back), [0, 2, 1]);
        // A read the label rewrite did not use changes no lane of the score
        // in play: the lane this read did not see keeps its number.
        lanes.read(lead);
        lanes.forget_read();
        lanes.install(lead);
        assert_eq!(lanes.read(&format!("{lead}\n{clap}")), [0, 1]);

        // The output plays a score. 2 scores after it are not audible: an
        // edit of a lane, then a score with no such lane. A rollback gives
        // each lane of the first score its number.
        let mut lanes = LaneTable::default();
        let (kick, serum) = (r#"$: s("bd")"#, r#"$: note("c2").vsti("serum")"#);
        let score = format!("{kick}\n{serum}");
        assert_eq!(play(&mut lanes, &score), [0, 1]);
        lanes.mark_audible();
        assert_eq!(play(&mut lanes, &score.replace("c2", "c2 e2")), [0, 1]);
        assert_eq!(play(&mut lanes, kick), [0]);
        assert_eq!(play(&mut lanes, &score), [0, 1]);
        // The output plays the score with no such lane: the lane is new.
        assert_eq!(play(&mut lanes, kick), [0]);
        lanes.mark_audible();
        assert_eq!(play(&mut lanes, &score), [0, 2]);

        // An edit of each lane of a long score keeps each number. Past the
        // limit of the compare, each statement is a new lane.
        let long = |count: usize, tail: &str| -> String {
            let lane = |at: usize| format!("$: s(\"bd\").n({at}){tail}\n");
            (0..count).map(lane).collect()
        };
        for (count, first) in [(400, 0), (600, 600)] {
            let mut lanes = LaneTable::default();
            play(&mut lanes, &long(count, ""));
            let numbers = play(&mut lanes, &long(count, ".gain(.5)"));
            let kept: Vec<u64> = (first..first + count as u64).collect();
            assert_eq!(numbers, kept);
        }
    }
}
