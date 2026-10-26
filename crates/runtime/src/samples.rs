//! Sample banks, soundfonts, and score-selected sample sources.
//!
//! Default bank manifests are pinned in `assets/sample-banks.json` by URL,
//! caller base, and SHA-256. The manifests are cached on disk and verified
//! against their pins. Background loaders fetch and decode sample files on
//! demand, then deliver them through [`SampleLibrary::take_ready`].
//!
//! Name resolution rules:
//! - `n` selects a sound: rounded half-up (`js_round`), NaN as 0, wrapped
//!   Euclidean-mod into the bank's length;
//! - array banks transpose from the hap's midi against C3=36, note-keyed
//!   banks pick the closest key and repitch by the difference;
//! - a sound that is not ready at onset time is skipped and can play on a
//!   later trigger once loaded.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, Weak, mpsc};
use std::time::{Duration, Instant};

use rustel_audio::{
    BUNDLED_BD_SAMPLE_ID, BUNDLED_BD_SAMPLE_IDENTITY, DecodedSample, SAMPLE_BANK_CAPACITY, SampleId,
};
use rustel_voice::{SampleLookup, SampleResolution, SpeechRequest};
use sha2::{Digest, Sha256};
use url::Url;

use crate::product;

mod access;
use access::*;
pub(crate) use access::{js_round, sound_index};
mod cache;
pub(crate) use cache::fetch_located;
use cache::*;
mod catalog;
use catalog::*;
mod loading;
use loading::*;
mod lookup;
use lookup::*;
mod manifests;
pub use cache::{
    clear_sample_cache, clear_score_sample_cache, sample_cache_dir, sample_cache_usage,
    score_sample_cache_dir,
};
pub use catalog::{
    deduplicated_global_sources, deduplicated_global_sources_with_owners, folder_of_spec,
    source_is_local,
};
use manifests::*;

mod codecs;
mod folders;
mod font_queue;
mod local_names;
mod manifest_queue;
mod soundfonts;
mod speech;
#[cfg(test)]
use codecs::decode_ogg;
use codecs::{codec_for, decode_guarded};
pub use folders::without_verbatim_prefix;
#[cfg(test)]
use folders::{
    PendingSampleDirectory, sample_scan_entry_bytes, sample_scan_path_bytes,
    sample_scan_stack_slots,
};
use folders::{folder_banks, local_file_url, scan_score_sample_folder, set_folder_banks};
pub use folders::{is_sample_audio, scan_sample_folder};
pub(crate) use folders::{sample_audio_kind, scan_sample_folder_with_limits};
use font_queue::FontQueue;
use manifest_queue::{ManifestJobs, ManifestQueue};
use soundfonts::load_font;
#[cfg(test)]
use soundfonts::{decode_font, fold_number_arithmetic};

/// How many sample-loader threads fetch at once. Enough to saturate a
/// typical link on bulk cache jobs without hammering the CDN; a live note
/// still jumps the queue and starts on the next free worker.
const SAMPLE_LOAD_WORKERS: usize = 6;

const MAX_SCORE_SAMPLE_MAP_BYTES: usize = 4 * 1024 * 1024;
/// Unique map-text bytes retained for source lookup and status history.
const MAX_SOURCE_TABLE_BYTES: usize = 32 * 1024 * 1024;
/// The most files a score's `samples()` maps may name, one map and the whole
/// session alike.
///
/// Naming a file costs one URL in the grant table. A slot in the decoded
/// bank is taken only when a sound plays, and the bank refuses there when it
/// is full. The limit is therefore not the decoded bank's capacity: a
/// library can name more files than the bank has slots, for example
/// `github:bubobubobubobubo/dough-waveforms` with 4,358 wavetables in 65
/// banks. The limit is the same as for the built-in manifests.
const MAX_SCORE_SAMPLE_FILES: usize = MAX_SAMPLE_MANIFEST_ENTRIES;
const MAX_SCORE_SAMPLE_SCAN_ENTRIES: usize = 65_536;

/// Maximum committed response bytes in the score-selected cache.
pub const SCORE_SAMPLE_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum number of score-selected responses kept on disk.
pub const SCORE_SAMPLE_CACHE_MAX_ENTRIES: usize = 4_096;
/// Maximum bytes one [`SampleLibrary`] may add during its lifetime.
pub const SCORE_SAMPLE_CACHE_SESSION_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum entries one [`SampleLibrary`] may add during its lifetime.
pub const SCORE_SAMPLE_CACHE_SESSION_MAX_ENTRIES: usize = 2_048;

const SCORE_CACHE_NAMESPACE: &str = "score";
const SCORE_CACHE_NO_LEGACY_MARKER: &str = ".score-cache-no-legacy";
// Persistent schema identity: product renames must not invalidate existing
// offline entries.
const SCORE_CACHE_KEY_DOMAIN_V1: &[u8] = b"strudel-score-cache-v1\0";

/// Host-granted resources that score-level `samples(...)` may use, and origin
/// grants for Hydra image sources.
///
/// An empty policy is deliberately inert: evaluating score text must not read
/// the working directory or choose a network destination. Hosts opt in with
/// exact HTTP(S) origins, one canonical local sample root, and/or the
/// strudel.cc parity default of [`Self::permit_public_cors_origins`].
/// Hydra images also require public HTTPS destinations and CORS consent,
/// including for explicitly granted origins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoreSampleAccess {
    local_root: Option<Arc<crate::sample_server::ServedRoot>>,
    allowed_origins: Vec<String>,
    open_public: bool,
}

/// The `Origin` this host presents when it requires CORS consent, and the
/// value a server's `Access-Control-Allow-Origin` answer is compared against.
/// strudel.cc is the origin whose reachable set this policy reproduces: a
/// server that would refuse the browser refuses us.
const CORS_REQUEST_ORIGIN: &str = "https://strudel.cc";

/// Whether an `Access-Control-Allow-Origin` answer consents to the read.
fn cors_consents(header: Option<&str>) -> bool {
    header
        .map(str::trim)
        .is_some_and(|value| value == "*" || value.eq_ignore_ascii_case(CORS_REQUEST_ORIGIN))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScoreFetchAccess {
    Remote {
        origin: String,
        /// Approved by the default policy rather than an exact grant: every
        /// response must consent via `Access-Control-Allow-Origin`.
        cors_required: bool,
    },
    Local {
        root: Arc<crate::sample_server::ServedRoot>,
    },
}

use crate::sample_fetch;

/// Compiled-in pin of the default bank manifests.
const PINNED_BANKS: &str = include_str!("../assets/sample-banks.json");

#[derive(Debug, serde::Deserialize)]
struct PinnedFonts {
    base: String,
    fonts: std::collections::BTreeMap<String, Vec<String>>,
}

/// Compiled-in pin of the General MIDI soundfont map.
const PINNED_GM_FONTS: &str = include_str!("../assets/gm-fonts.json");

/// Where the pinned packs live, by base prefix: the category each pack's
/// pin declares, lowercased with `source_prefix`'s trailing slash, longest
/// prefix first so a pack nested inside another's files wins. Read once
/// from the compiled-in pin; the pin is the only place a shipped pack's
/// shelf is named.
type DefaultCategories = Vec<(String, SoundCategory)>;

#[derive(Debug, serde::Deserialize)]
struct PinnedFile {
    sources: Vec<PinnedSource>,
    inline: PinnedInline,
}

#[derive(Debug, serde::Deserialize)]
struct PinnedSource {
    name: String,
    url: String,
    base: Option<String>,
    sha256: String,
    /// The browser shelf the pack's banks file under, as the pin says.
    /// None for a pin that only maps aliases onto another pack's banks:
    /// it brings no files, so it has nothing to file.
    category: Option<SoundCategory>,
}

#[derive(Debug, serde::Deserialize)]
struct PinnedInline {
    base: String,
    /// The shelf the inline banks file under, as the pin says.
    category: Option<SoundCategory>,
    banks: HashMap<String, Vec<String>>,
}

/// One of the packs the studio ships with, as the Sources page lists it
/// under the packs the player imported: a pinned manifest, the inline
/// banks, or the General MIDI soundfonts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultSource {
    /// The pack's short name, as the pin file names it.
    pub name: String,
    /// Where its list lives - the address its row shows.
    pub url: String,
    /// Where its files live: every url under this is the pack's, which is
    /// how its banks are told apart from the others' once they are all
    /// merged into one map.
    pub base: String,
}

/// What caching a shipped pack onto disk asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheRequest {
    /// Sounds the library knows under the pack right now.
    pub sounds: usize,
    /// Distinct files those sounds name.
    pub files: usize,
    /// Files put in the loader's line. The rest are on disk already, or
    /// were on their way for a note.
    pub queued: usize,
}

#[derive(Clone)]
enum Bank {
    /// Urls in file order; `n` indexes with round + euclidean wrap.
    Array(Vec<Arc<str>>),
    /// (midi, urls) per note key, in JSON key order for the closest-key
    /// reduce tie-break (`<` strictly-closer keeps the FIRST of equals).
    Notes(Vec<(f64, Vec<Arc<str>>)>),
}

/// Which rate a decoded file lands at.
///
/// An ordinary sample arrives at the context's rate. A wavetable decodes at
/// the FILE's rate so its frames stay aligned to the file: slicing
/// 2048-sample frames out of a resampled buffer would cut each one at the
/// wrong stride and play a different waveform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeRate {
    Context,
    Native,
}

impl DecodeRate {
    /// `registerSampleSource` sends every `wt_`-named bank to the wavetable
    /// loader; everything else goes through the sampler.
    fn for_sound(name: &str) -> Self {
        if name.starts_with("wt_") {
            Self::Native
        } else {
            Self::Context
        }
    }
}

enum UrlState {
    Loading,
    Ready {
        id: SampleId,
        duration_secs: f64,
    },
    /// When it failed, so a later ask knows whether it has rested.
    Failed {
        at: Instant,
    },
}

/// Where one sound of a score stands before it is asked to play.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundReadiness {
    /// Decoded and ready to sound on its first onset.
    Ready,
    /// Known, and on its way.
    Loading,
    /// Known, and its download or decode failed.
    Failed,
    /// No bank of that name.
    Unknown,
}

/// Where a browsable sound came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundOrigin {
    /// The pinned default manifests.
    Default,
    /// A `samples(…)` call in a score.
    Score,
    /// A General MIDI soundfont.
    Font,
    /// A sound the voice engine makes by itself: an oscillator, a noise,
    /// `supersaw`, `sbd`.
    Synth,
    /// The live audio input, `in`: one channel a voice, `in:1` the next.
    Input,
    /// Audio sitting in the open set's own folder, taken up without the
    /// score asking for it.
    Set,
    /// A folder or pack the player imported in Settings, which every set
    /// sees.
    Global,
}

/// What kind of sound a browser files an entry under: the pinned banks
/// by the shelf their pin declares, the synths, the soundfonts, and the
/// score's own `samples(…)`. A browser filters by these; twenty of them
/// would still be a list to pick from, not twenty keys to remember.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SoundCategory {
    Synth,
    Drums,
    Percussion,
    Piano,
    Orchestra,
    Wavetable,
    Dirt,
    #[serde(rename = "gm")]
    Font,
    Score,
    Set,
    /// A source the player imported in Settings.
    Mine,
    Other,
}

impl SoundCategory {
    /// Every category, in the order a chooser lists them.
    pub const ALL: [Self; 12] = [
        Self::Synth,
        Self::Drums,
        Self::Percussion,
        Self::Piano,
        Self::Orchestra,
        Self::Wavetable,
        Self::Dirt,
        Self::Font,
        Self::Score,
        Self::Set,
        Self::Mine,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Synth => "synth",
            Self::Drums => "drums",
            Self::Percussion => "percussion",
            Self::Piano => "piano",
            Self::Orchestra => "orchestra",
            Self::Wavetable => "wavetable",
            Self::Dirt => "dirt",
            Self::Font => "gm",
            Self::Score => "score",
            Self::Set => "set",
            Self::Mine => "mine",
            Self::Other => "other",
        }
    }

    /// The category of a pinned bank, read off the pin: each pack in
    /// `sample-banks.json` declares the browser shelf its banks belong
    /// on. A bank whose first url is under no declared pack's base files
    /// as Other.
    fn of_location(location: Option<&str>) -> Self {
        Self::of_location_with(location, default_categories())
    }

    /// [`Self::of_location`] against a pin's declared categories.
    fn of_location_with(location: Option<&str>, declared: &DefaultCategories) -> Self {
        let Some(url) = location else {
            return Self::Other;
        };
        let url = url.to_ascii_lowercase();
        declared
            .iter()
            .find(|(prefix, _)| url.starts_with(prefix.as_str()))
            .map(|(_, category)| *category)
            .unwrap_or(Self::Other)
    }
}

/// The upstream repository address of a file the pinned library holds on
/// the strudel CDN.
///
/// The CDN serves the files but refuses directory listings: a bank's address
/// there answers `403 Forbidden`. Each pinned manifest in
/// `assets/sample-banks.json` names the repository of its files as its
/// `_base`, and the table below repeats those repositories. The rewritten
/// address opens a page that renders. A URL outside the pinned CDN
/// collections is not rewritten.
pub fn upstream_file_url(url: &str) -> Option<String> {
    const CDN: &str = "https://strudel.b-cdn.net/";
    const RAW: &str = "https://raw.githubusercontent.com/";
    // The CDN folder is dropped, and what stands under it is the path
    // within the repository itself - root and branch, as each manifest's
    // `_base` spells them. Matched case-insensitively (the VCSL base is
    // uppercase), spelled back the way GitHub itself spells them.
    let path = url.strip_prefix(CDN)?;
    let (root, rest) = path.split_once('/')?;
    if rest.is_empty() {
        return None;
    }
    let repo = match root.to_ascii_lowercase().as_str() {
        "piano" => "felixroos/dough-samples/main/piano/",
        "vcsl" => "sgossner/VCSL/master/",
        "tidal-drum-machines" => "ritchse/tidal-drum-machines/main/",
        "uzu-drumkit" => "tidalcycles/uzu-drumkit/main/",
        "uzu-wavetables" => "tidalcycles/uzu-wavetables/main/",
        "mrid" => "yaxu/mrid/main/",
        "dirt-samples" => "tidalcycles/Dirt-Samples/master/",
        _ => return None,
    };
    Some(format!("{RAW}{repo}{rest}"))
}

/// One sound the library can play, for a browser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundEntry {
    pub name: String,
    /// How many numbered variants `name:n` can pick from.
    pub variants: usize,
    /// File stems for local variants, in the same order as `name:n`.
    /// Remote packs leave this empty: their implementation filenames are
    /// rarely useful, while a recording's filename is the name its owner
    /// chose on disk.
    pub variant_names: Vec<String>,
    pub origin: SoundOrigin,
    pub category: SoundCategory,
    /// The address of its first file - where the bank came from.
    pub location: Option<String>,
    /// The `samples(…)` import that brought a score's bank, as written -
    /// `github:user/repo` - for a browser to file it under. None for a
    /// bank from an inline map, and for everything not the score's.
    pub import: Option<String>,
}

/// A source the player added in Settings: a folder on disk, or a pack at a
/// URL. A pack may use the shorthands a score can write, such as
/// `github:tidalcycles/dirt-samples`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalSource {
    pub spec: String,
    /// Off keeps the row without its sounds: a pack you want back next
    /// week should not have to be typed again.
    pub enabled: bool,
}

/// How one of those sources stands, for the browser and the settings sheet
/// to show. A source is never fatal, so every failure is a state here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalSourceState {
    /// Its names are in.
    Ready { banks: usize },
    /// A pack whose manifest is still on its way.
    Loading,
    /// The folder or file is not there - an unplugged drive, a renamed
    /// path.
    Missing(String),
    /// It is there and would not read.
    Failed(String),
    /// Turned off in Settings.
    Off,
}

/// One row of [`SampleLibrary::adopt_global_sources`]'s answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalSourceReport {
    pub spec: String,
    pub state: GlobalSourceState,
}

/// Where a registration's names land.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BankLayer {
    /// A score's own `samples(…)`, and the open set's folder.
    Score,
    /// A pack the player added in Settings, under the adoption that asked
    /// for it: a list that lands after the next adoption is thrown away.
    Global { generation: u64 },
}

/// One row of Settings ▸ Samples, as the library holds it.
struct GlobalSlot {
    row: usize,
    spec: String,
    kind: GlobalKind,
    /// What the row brought. A pack keeps its last list while a new one is
    /// being fetched, so a refetch does not silence it in the meantime.
    banks: HashMap<String, Bank>,
    state: GlobalSourceState,
    /// A file was deleted while this folder was being walked, so the walk
    /// under way may still list it: its result is dropped and the folder
    /// is walked again.
    rewalk: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GlobalKind {
    Folder,
    Pack,
}

/// What became of a finished folder walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FolderFill {
    /// The row holds what the walk found.
    Filled,
    /// The row was replaced since the walk was asked for.
    Superseded,
    /// A file was deleted from the row during the walk: walk it again.
    Stale,
}

/// A loader failure, said once, with the `samples(…)` maps it leaves
/// failed, spelled as the library keeps them: none unless it is an
/// import's.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SampleFailure {
    pub message: String,
    pub maps: Vec<String>,
}

impl From<String> for SampleFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            maps: Vec::new(),
        }
    }
}

/// Where a `samples(…)` the score wrote stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceState {
    /// Asked for; its map is on its way.
    Loading,
    /// Its map is in, and the names it brought are the library's.
    Ready,
    /// Its map could not be read, and this is why.
    Failed(String),
}

/// Where a map stands, and since when: a failure rests from `since`
/// before it is asked for again.
struct Standing {
    state: SourceState,
    since: Instant,
    /// How many failures in a row this one ends; 0 for any other state.
    failures: u32,
}

impl Standing {
    fn now(state: SourceState) -> Self {
        Self {
            state,
            since: Instant::now(),
            failures: 0,
        }
    }

    fn failed(&self) -> bool {
        matches!(self.state, SourceState::Failed(_))
    }

    /// How long a failure rests before the background asks again:
    /// [`FAILED_RETRY_AFTER`], doubled for each failure in a row before it,
    /// up to [`FAILED_RETRY_LONGEST`].
    fn rest(&self) -> Duration {
        FAILED_RETRY_AFTER
            .saturating_mul(2u32.saturating_pow(self.failures.saturating_sub(1)))
            .min(FAILED_RETRY_LONGEST)
    }
}

/// One decoded webaudiofontdata zone: a pitched, possibly looping sample
/// keyed to a midi range.
#[derive(Clone)]
struct FontZone {
    key_lo: f64,
    key_hi: f64,
    /// `originalPitch - 100·coarseTune - fineTune` (cents).
    base_detune: f64,
    id: SampleId,
    duration_secs: f64,
    loop_secs: Option<(f64, f64)>,
}

enum FontState {
    Loading,
    Ready(Arc<Vec<FontZone>>),
    /// When it failed, so a later real ask knows whether it has rested.
    Failed {
        at: Instant,
    },
}

/// Explicit preloads cover a bank; live source scans are only guesses about
/// the next sound. Keep that distinction when a manifest delays the request.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum PrefetchIntent {
    #[default]
    Explicit,
    LiveWarm,
    /// Fetch onto disk so a later play does not wait on the network.
    /// Does not reserve a live-bank id or decode PCM.
    CacheDisk,
}

impl PrefetchIntent {
    /// Whether to report a name that this intent did not find.
    ///
    /// Only an explicit preload reports it. `preload("kik:1")` names a sound
    /// that does not exist, and the player can correct that.
    ///
    /// A live warm is a bet. It pairs each sound name with each `.bank()`
    /// name in the text, because the text does not show which bank holds
    /// which sound. Four banks and six sounds give twenty-four names, and
    /// most of them do not exist. `s("sawtooth")` is an oscillator, so no
    /// bank holds it. These misses are expected and are not reported.
    fn reports_unknown(self) -> bool {
        matches!(self, Self::Explicit)
    }

    fn priority(self) -> LoadPriority {
        match self {
            Self::Explicit => LoadPriority::Now,
            Self::LiveWarm | Self::CacheDisk => LoadPriority::Bet,
        }
    }
}

/// Why a sound was asked for, which is where it stands in the loader's line.
///
/// Several threads fetch at once, so order still matters: a bet placed on a
/// name as it was typed must not delay the sound a score is about to play.
/// What plays now goes first; among bets the newest first, because the name
/// just typed is the one meant and the one before it was probably on the way
/// there. Idle workers take the next job; a live note therefore starts as
/// soon as any one of them is free, while cache bets keep filling the rest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadPriority {
    /// About to play, or asked for by an update or a preload.
    Now,
    /// Typed, and probably meant.
    Bet,
}

enum LoadKind {
    /// Fetch, decode, and seat in the live sample bank.
    Install {
        id: SampleId,
        decode_rate: DecodeRate,
    },
    /// Fetch onto disk only. The live bank is left alone: caching the
    /// whole library is thousands of files, and seating them would
    /// exhaust the 2048-slot bank before an evening of browsing could
    /// hand any id back.
    Cache,
}

struct LoadJob {
    url: Arc<str>,
    kind: LoadKind,
}

/// The loader's line, in two parts, with the thread waiting on it.
struct LoadQueue {
    state: Mutex<LoadQueueState>,
    wake: Condvar,
}

#[derive(Default)]
struct LoadQueueState {
    now: VecDeque<LoadJob>,
    bets: VecDeque<LoadJob>,
    closed: bool,
}

#[derive(Clone, Copy)]
enum DecodedIdentity {
    Unknown,
    Current(u64),
    Forgotten,
}

/// Buckets in a remembered sample shape. Wider than any preview row a
/// terminal can offer, so the drawing samples down rather than up.
pub const SHAPE_BUCKETS: usize = 512;

/// Shapes kept at once. A browsing session decodes a few hundred sounds;
/// past this the oldest go, because a shape is only ever wanted for the
/// sound sounding right now. Half a megabyte at worst.
const MAX_REMEMBERED_SHAPES: usize = 4_096;

/// What each decoded sample looked like, oldest first.
#[derive(Default)]
struct SampleShapes {
    by_url: HashMap<Arc<str>, Arc<[u8]>>,
    order: VecDeque<Arc<str>>,
}

impl SampleShapes {
    fn remember(&mut self, url: Arc<str>, shape: Arc<[u8]>) {
        if self.by_url.insert(Arc::clone(&url), shape).is_none() {
            self.order.push_back(url);
        }
        while self.order.len() > MAX_REMEMBERED_SHAPES {
            if let Some(oldest) = self.order.pop_front() {
                self.by_url.remove(&oldest);
            }
        }
    }
}

/// A decoded sample as [`SHAPE_BUCKETS`] peaks, 0 through 255.
///
/// The peak of each bucket rather than its average: a drum hit averaged
/// over its own length is a flat line, and the transient is the part a
/// reader recognises.
fn shape_of(decoded: &DecodedSample) -> Arc<[u8]> {
    let frames = decoded.frames();
    let channels = usize::from(decoded.channels().max(1));
    let pcm = decoded.pcm();
    let mut shape = vec![0u8; SHAPE_BUCKETS];
    if frames == 0 {
        return shape.into();
    }
    for (bucket, slot) in shape.iter_mut().enumerate() {
        let from = bucket * frames / SHAPE_BUCKETS;
        let to = ((bucket + 1) * frames / SHAPE_BUCKETS)
            .max(from + 1)
            .min(frames);
        let peak = pcm[from * channels..to * channels]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        *slot = (peak.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    shape.into()
}

/// The install queue and its intended payload identities share one publication
/// lock. Taking the PCM does not erase intent: the device may refuse the first
/// install before the producer has even attempted later entries in that batch.
struct ReadySamples {
    samples: Vec<(SampleId, DecodedSample)>,
    identities: [DecodedIdentity; SAMPLE_BANK_CAPACITY],
}

#[derive(Default)]
struct SourceTables {
    sources: HashSet<String>,
    states: HashMap<String, Standing>,
    order: VecDeque<String>,
    bytes: usize,
}

impl SourceTables {
    fn note(&mut self, map: &str, state: SourceState) -> Option<Standing> {
        // Failed lookups may reach history after map validation refuses them.
        if map.len() > MAX_SCORE_SAMPLE_MAP_BYTES {
            return None;
        }
        let mut standing = Standing::now(state);
        if standing.failed() {
            standing.failures = self
                .states
                .get(map)
                .map_or(0, |previous| previous.failures)
                .saturating_add(1);
        }
        let known = self.states.contains_key(map) || self.sources.contains(map);
        if !known {
            self.order.push_back(map.to_owned());
            self.bytes = self.bytes.saturating_add(map.len());
        }
        if standing.state == SourceState::Ready {
            self.sources.insert(map.to_owned());
        }
        let previous = self.states.insert(map.to_owned(), standing);
        while self.bytes > MAX_SOURCE_TABLE_BYTES && self.order.len() > 1 {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(oldest.len());
            self.states.remove(&oldest);
            self.sources.remove(&oldest);
        }
        previous
    }

    fn forget_state(&mut self, map: &str) {
        self.states.remove(map);
        if !self.sources.contains(map) {
            self.order.retain(|known| known != map);
            self.bytes = self.order.iter().map(String::len).sum();
        }
    }
}

struct Shared {
    by_url: RwLock<HashMap<Arc<str>, UrlState>>,
    /// Words the `talkit` control rendered, by request. Each renders once and
    /// stays until [`SampleLibrary::forget_decoded`] lets it go; see
    /// `samples::speech`.
    speech: Mutex<HashMap<speech::SpeechKey, UrlState>>,
    /// URLs introduced by score text and the exact host grant attached to
    /// each one. Loader threads consult this again at I/O time so a later
    /// refactor cannot validate registration and then fetch with broader
    /// authority.
    score_sources: RwLock<HashMap<Arc<str>, ScoreFetchAccess>>,
    ready: Mutex<ReadySamples>,
    /// A peak envelope per decoded url: what a sample looks like, for a
    /// browser to draw a preview against.
    ///
    /// Kept here because this is the only place that ever sees the PCM.
    /// The decoded body goes straight to the device and the library does
    /// not retain it, so anything asking afterwards has nothing to
    /// measure. One byte a bucket, a fixed [`SHAPE_BUCKETS`] of them, and
    /// the oldest fall out past `MAX_REMEMBERED_SHAPES`.
    shapes: Mutex<SampleShapes>,
    /// Published source maps and registration status share one retention
    /// budget. Publication and eviction are atomic for loader and UI readers.
    source_tables: Mutex<SourceTables>,
    /// The `samples(...)` import each of the score's banks came from, by
    /// bank name, as the argument was written.
    bank_imports: RwLock<HashMap<String, Arc<str>>>,
    /// The banks the open set's own folder brought, by name, each with the
    /// bank it displaced - so the next set can take them back out and put
    /// back what was there first. A set's samples belong to that set, and
    /// leaving them behind would let one set's `kicks` play in another;
    /// dropping them by name alone would take a score's `kicks` with them,
    /// which the set never supplied.
    set_banks: RwLock<BTreeMap<String, Option<Bank>>>,
    /// Hidden filename addresses; kept out of the browser's bank rows.
    custom_file_names: RwLock<local_names::Names>,
    global_file_names: RwLock<local_names::Names>,
    next_id: AtomicU32,
    /// Ids the studio handed back - forgotten by every table, cleared from
    /// the bank, past the last frame anything queued could name them at -
    /// for the next reservation to take before the counter moves. Oldest
    /// first, so a reissued id is as far as it can be from its last owner.
    free_ids: Mutex<VecDeque<SampleId>>,
    /// The loader's line. Closed when this goes, which is what ends the
    /// loader threads.
    jobs: Arc<LoadQueue>,
    /// Files the loader workers have in hand - fetching or decoding - for
    /// a progress line to name. Empty between jobs.
    loading: Mutex<HashSet<Arc<str>>>,
    /// URLs a disk-cache job has claimed, from queueing until the fetch
    /// settles, so a second cache pass does not enqueue the same file twice.
    caching: Mutex<HashSet<Arc<str>>>,
    /// Host-trusted sample cache directory this library's loader writes.
    host_cache: PathBuf,
    /// `{base}/{font}.js` for General MIDI files, as the font worker fetches.
    font_base: String,
    /// gm soundfont FILES (webaudiofontdata), keyed by font file name.
    fonts: RwLock<HashMap<Arc<str>, FontState>>,
    /// What the player renamed an imported bank to, by the name the scan
    /// or the pack gave it. An overlay rather than a rewrite: the files are
    /// untouched and the old name is what comes back if the rename is
    /// removed.
    renames: RwLock<HashMap<String, String>>,
    /// Aliases written by current versions, keyed by source and original name.
    source_renames: RwLock<HashMap<(String, String), String>>,
    /// Stable collision aliases keyed by source and original bank name.
    auto_aliases: RwLock<HashMap<(String, String), String>>,
    /// Settings ▸ Samples, row by row, as the library holds them: what each
    /// row was, what it brought, and how it stands. The flat `global` map is
    /// rebuilt from these in row order whenever one of them changes, which
    /// is what makes "a later row wins a shared name" true for a folder
    /// against a pack and not only for two of a kind.
    global_slots: Mutex<Vec<GlobalSlot>>,
    /// Bumped by every adoption. A pack's manifest lands on the loader's
    /// own schedule, so one enqueued before a re-adoption must not fill a
    /// slot the re-adoption has since replaced - or removed.
    global_generation: std::sync::atomic::AtomicU64,
    /// Which imported source each name in `global` came of, for the
    /// browser to file it under.
    global_source_of: RwLock<HashMap<String, Arc<str>>>,
    /// The fetch policy a score gets, handed in by the host so an imported
    /// pack is granted no less than a score would be: the exact origin the
    /// player typed on top of it.
    import_policy: Mutex<Option<ScoreSampleAccess>>,
    font_jobs: Arc<FontQueue>,
    /// One loud log line per failed url, never per onset.
    failures: Mutex<Vec<SampleFailure>>,
    /// Full-screen clients own the terminal and suppress informational
    /// loader writes; failures still travel through `failures` above.
    direct_diagnostic_logging: AtomicBool,
    /// Persistent score-selected bytes have their own namespace and budget.
    /// One library belongs to one Session, so this also owns that Session's
    /// share of the process-independent cache allowance.
    score_cache: ScoreCache,
    /// Manifest batches queued or active. Included in `wait_until_idle` so an
    /// offline render cannot mistake "the bank map has not arrived" for an
    /// unknown sound.
    manifest_pending: AtomicUsize,
    /// The last cancellation check and every externally visible manifest
    /// publication share this gate. Once `SampleLibrary::drop` returns, a
    /// worker can finish staging bytes but cannot publish them, a bank map, or
    /// a deferred preload.
    publication: Arc<PublicationGate>,
    /// The rate decoded PCM is converted to, as an AudioContext converts every
    /// file it decodes to its own. Owners set it from the render or device rate
    /// before a score can ask for a sound; see [`SampleLibrary::set_render_rate`].
    render_rate: AtomicU32,
    /// Moves whenever something a still-loading sound can be waiting on
    /// settles: a sample or a soundfont leaves Loading (ready or failed), or
    /// a manifest batch finishes. Bumped AFTER the table it describes has
    /// changed, so a reader that samples it before asking for a sound and
    /// sees it unchanged afterwards knows the answer it got still stands.
    settled_epoch: AtomicU64,
}

impl Shared {
    fn note_settled(&self) {
        self.settled_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Record where `map` stands and return where it stood. A failure
    /// counts one more in a row after a failure.
    fn note_source_standing(&self, map: &str, state: SourceState) -> Option<Standing> {
        self.source_tables
            .lock()
            .expect("samples source tables")
            .note(map, state)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.jobs.close();
        self.font_jobs.close();
    }
}

/// How long a failed download rests before a real ask tries it again. A
/// connection that dropped for a moment should not cost a sound for the
/// rest of the set; a server that is down should not be knocked on every
/// onset either.
const FAILED_RETRY_AFTER: Duration = Duration::from_secs(5);

/// The longest a `samples("…")` import that keeps failing rests before the
/// background asks again: a few [`MANIFEST_BATCH_TIMEOUT`]s, so one whose
/// server never answers leaves the loader free most of the time.
const FAILED_RETRY_LONGEST: Duration = MANIFEST_BATCH_TIMEOUT.saturating_mul(5);

/// How long a manifest job may take, counted from when the worker takes it.
const MANIFEST_BATCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MANIFEST_EFFECTS: usize = 64;
const MAX_MANIFEST_EFFECT_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUED_PRELOADS: usize = SAMPLE_BANK_CAPACITY;
const MAX_QUEUED_PRELOAD_BYTES: usize = 256 * 1024;
const MANIFEST_QUEUE_WAIT_SLICE: Duration = Duration::from_millis(10);

/// What a producer is told when the manifest line does not take its job.
fn manifest_refusal(refused: &mpsc::TrySendError<Box<ManifestJob>>) -> String {
    match refused {
        mpsc::TrySendError::Full(_) => "sample manifest queue is full; registration refused",
        mpsc::TrySendError::Disconnected(_) => "sample manifest loader stopped",
    }
    .to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationKind {
    Cache,
    Defaults,
    Custom,
    Local,
    Preload,
}

struct PublicationGate {
    lock: Mutex<()>,
    cancelled: Arc<AtomicBool>,
    #[cfg(any(test, feature = "test-support"))]
    barrier: Mutex<Option<TestPublicationBarrier>>,
}

impl PublicationGate {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-support"))]
            barrier: Mutex::new(None),
        }
    }

    fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    fn enter<'a>(
        &'a self,
        kind: PublicationKind,
        budget: &sample_fetch::FetchBudget,
    ) -> Result<MutexGuard<'a, ()>, String> {
        #[cfg(any(test, feature = "test-support"))]
        self.wait_at_test_barrier(kind);
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = kind;
        let guard = self.lock.lock().expect("sample publication gate");
        budget.check()?;
        Ok(guard)
    }

    fn cancel(&self) {
        let _guard = self.lock.lock().expect("sample publication gate");
        self.cancelled.store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-support"))]
    fn install_test_barrier(
        &self,
        kind: PublicationKind,
    ) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (reached, reached_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let previous = self
            .barrier
            .lock()
            .expect("test publication barrier")
            .replace(TestPublicationBarrier {
                kind,
                reached,
                release: release_rx,
            });
        assert!(
            previous.is_none(),
            "only one publication barrier may be armed"
        );
        (reached_rx, release)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn wait_at_test_barrier(&self, kind: PublicationKind) {
        let barrier = {
            let mut slot = self.barrier.lock().expect("test publication barrier");
            if slot.as_ref().is_some_and(|barrier| barrier.kind == kind) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(barrier) = barrier {
            barrier.reached.send(()).expect("announce publication");
            barrier.release.recv().expect("release publication");
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
struct TestPublicationBarrier {
    kind: PublicationKind,
    reached: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
}

fn validate_manifest_effects<'a>(
    effects: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Result<(), String> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (map, base) in effects {
        count = count
            .checked_add(1)
            .ok_or_else(|| "samples() effect count overflowed".to_owned())?;
        if count > MAX_MANIFEST_EFFECTS {
            return Err(format!(
                "samples() registrations exceed the per-evaluation limit of {MAX_MANIFEST_EFFECTS}"
            ));
        }
        bytes = bytes
            .checked_add(map.len())
            .and_then(|bytes| bytes.checked_add(base.map_or(0, str::len)))
            .ok_or_else(|| "samples() effect byte count overflowed".to_owned())?;
        if bytes > MAX_MANIFEST_EFFECT_BYTES {
            return Err(format!(
                "samples() registrations exceed the per-evaluation byte limit of {MAX_MANIFEST_EFFECT_BYTES}"
            ));
        }
    }
    Ok(())
}

fn copy_manifest_text(text: &str) -> Result<String, String> {
    let mut copy = String::new();
    copy.try_reserve_exact(text.len())
        .map_err(|_| "samples() registration exceeds host memory".to_owned())?;
    copy.push_str(text);
    Ok(copy)
}

fn copy_preload_specs(names: &[String]) -> Result<Vec<String>, String> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for spec in names.iter().flat_map(|names| names.split_whitespace()) {
        count = count
            .checked_add(1)
            .ok_or_else(|| "preload effect count overflowed".to_owned())?;
        bytes = bytes
            .checked_add(spec.len())
            .ok_or_else(|| "preload effect byte count overflowed".to_owned())?;
        if count > MAX_QUEUED_PRELOADS || bytes > MAX_QUEUED_PRELOAD_BYTES {
            return Err(format!(
                "preloads queued behind sample manifests exceed the session limit of \
                 {MAX_QUEUED_PRELOADS} entries or {MAX_QUEUED_PRELOAD_BYTES} bytes"
            ));
        }
    }

    let mut copied = Vec::new();
    copied
        .try_reserve_exact(count)
        .map_err(|_| "preload effects exceed host memory".to_owned())?;
    for spec in names.iter().flat_map(|names| names.split_whitespace()) {
        let mut owned = String::new();
        owned
            .try_reserve_exact(spec.len())
            .map_err(|_| "preload effects exceed host memory".to_owned())?;
        owned.push_str(spec);
        copied.push(owned);
    }
    Ok(copied)
}

/// What a name resolves to, and the order of the search.
///
/// The order is: a score's own `samples(...)` and the open set's folder,
/// then the sources the player imported in Settings, then a General MIDI
/// font, then the pinned banks. This is the order playback uses. The library
/// and the manifest worker both resolve names through this function, so a
/// warm and a play cannot resolve one name to different banks. To add a
/// layer, add a step here.
fn look_up_named<T>(
    custom: &RwLock<HashMap<String, Bank>>,
    global: &RwLock<HashMap<String, Bank>>,
    gm: &HashMap<String, Vec<Arc<str>>>,
    banks: &RwLock<HashMap<String, Bank>>,
    shared: &Shared,
    name: &str,
    take: impl FnOnce(Named<'_>) -> T,
) -> Option<T> {
    // Exact spelling wins first, in layer order. If it misses, repeat that
    // same layer order case-insensitively. Imported and score banks are
    // published under their catalogue spelling only, whereas pinned maps
    // also keep lowercase aliases for their historical fast path.
    {
        let custom = custom.read().expect("custom banks");
        if let Some(bank) = custom.get(name) {
            return Some(take(Named::Bank(bank)));
        }
    }
    {
        let global = global.read().expect("global banks");
        if let Some(bank) = global.get(name) {
            return Some(take(Named::Bank(bank)));
        }
    }
    if let Some(fonts) = gm.get(name) {
        return Some(take(Named::Font(fonts)));
    }
    {
        let banks = banks.read().expect("default banks");
        if let Some(bank) = banks.get(name) {
            return Some(take(Named::Bank(bank)));
        }
    }

    let folded = name.to_lowercase();
    let folded_entry = |candidate: &str| candidate.to_lowercase() == folded;
    {
        let custom = custom.read().expect("custom banks");
        if let Some(bank) = custom.get(&folded).or_else(|| {
            custom
                .iter()
                .find_map(|(name, bank)| folded_entry(name).then_some(bank))
        }) {
            return Some(take(Named::Bank(bank)));
        }
    }
    {
        let global = global.read().expect("global banks");
        if let Some(bank) = global.get(&folded).or_else(|| {
            global
                .iter()
                .find_map(|(name, bank)| folded_entry(name).then_some(bank))
        }) {
            return Some(take(Named::Bank(bank)));
        }
    }
    if let Some(fonts) = gm.get(&folded).or_else(|| {
        gm.iter()
            .find_map(|(name, fonts)| folded_entry(name).then_some(fonts))
    }) {
        return Some(take(Named::Font(fonts)));
    }
    {
        let banks = banks.read().expect("default banks");
        if let Some(bank) = banks.get(&folded).or_else(|| {
            banks
                .iter()
                .find_map(|(name, bank)| folded_entry(name).then_some(bank))
        }) {
            return Some(take(Named::Bank(bank)));
        }
    }
    local_names::lookup(shared, name, take)
}

/// What warming a name turned out to mean: files to fetch, or font files
/// for the soundfont loader. Carried out of the lock rather than acted on
/// inside it.
enum Warm {
    Urls(Vec<Arc<str>>),
    /// A bank's files that the note picks between rather than `n`.
    Pitched(Vec<Arc<str>>),
    Fonts(Vec<Arc<str>>),
}

/// The four name tables [`look_up_named`] searches, each folded to
/// lowercase once, in its order.
struct FoldedNames(Vec<HashMap<String, String>>);

/// [`SampleLibrary::peek_ready_ids_of`], over one folding of the name
/// tables: see [`SampleLibrary::ready_ids`].
pub struct ReadyIds<'a> {
    library: &'a SampleLibrary,
    folded: Option<FoldedNames>,
}

/// Every variant, for the callers that ask about a name as a whole.
static EVERY_VARIANT: crate::sounds::Variants = crate::sounds::Variants::All;

enum Named<'a> {
    Bank(&'a Bank),
    /// The ordered font-file variants of a General MIDI name; `n` picks.
    Font(&'a [Arc<str>]),
}

#[derive(Clone)]
struct ManifestContext {
    banks: Arc<RwLock<HashMap<String, Bank>>>,
    custom: Arc<RwLock<HashMap<String, Bank>>>,
    global: Arc<RwLock<HashMap<String, Bank>>>,
    gm: Arc<HashMap<String, Vec<Arc<str>>>>,
    shared: Arc<Shared>,
}

enum ManifestWork {
    Defaults {
        sources: Vec<PinnedSource>,
        cache_dir: PathBuf,
    },
    /// Walk imported folders and fill their rows.
    ///
    /// A folder used to be walked inline, on whatever thread asked. That
    /// is fine for a dozen kits beside a score and ruinous for a real
    /// library: fifty thousand files on network-backed storage is minutes
    /// of a studio that answers nothing, on every drop, every Enter and
    /// every start. Packs have always landed on this worker; folders do
    /// now too, and their rows read `fetching…` in the meantime.
    Folders { specs: Vec<String>, generation: u64 },
    Custom {
        effects: Vec<(String, Option<String>)>,
        preloads: Vec<String>,
        intent: PrefetchIntent,
        access: ManifestAccess,
        continue_on_error: bool,
        /// Which map the names land in. A score's `samples(…)` and the set
        /// folder share one; the player's own Settings sources have their
        /// own, a step lower in the chain.
        layer: BankLayer,
    },
}

#[derive(Clone)]
enum ManifestAccess {
    Trusted,
    Score(ScoreSampleAccess),
}

struct ManifestJob {
    work: ManifestWork,
    /// How long the worker may spend on it, counted from when it takes it.
    timeout: Duration,
    completion: Option<mpsc::SyncSender<Result<(), String>>>,
}

pub struct SampleLibrary {
    banks: Arc<RwLock<HashMap<String, Bank>>>,
    /// Score-level `samples(...)` registrations, and the open set's own
    /// folder. Checked BEFORE everything else: registration overwrites, so
    /// the last user registration wins over anything prebaked.
    custom: Arc<RwLock<HashMap<String, Bank>>>,
    /// Sources the player added in Settings - folders on disk and packs at
    /// a URL - which every set sees. Under the open set's own folder, so a
    /// set can still keep its own `bd`, and over the pinned banks and the
    /// fonts, because a pack you went and imported is one you meant.
    global: Arc<RwLock<HashMap<String, Bank>>>,
    /// gm_* soundfont name → ordered font-file variants (pinned map; `n`
    /// selects the variant with the same round+euclid index as samples).
    gm: Arc<HashMap<String, Vec<Arc<str>>>>,
    shared: Arc<Shared>,
    manifest_queue: Arc<ManifestQueue>,
    /// Held across a producer's marks and its push, and across a preload's
    /// look at whether a manifest is on its way, so no other producer's job
    /// lands in between. Never held while waiting.
    manifest_order: Mutex<()>,
    /// The slot of each bus name a score sends to or reads from.
    bus_names: rustel_voice::BusNames,
}

/// Keeps the manifest worker on a job until it drops; see
/// [`SampleLibrary::hold_manifest_worker_for_test`].
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct ManifestWorkerHold {
    release: mpsc::SyncSender<()>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ManifestWorkerHold {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

impl Drop for SampleLibrary {
    fn drop(&mut self) {
        // Publication is cancelled immediately and dropping the owner never
        // joins a network thread. A blocking OS connect/read cannot be
        // interrupted by ureq; it remains bounded by the batch deadline, and
        // the checks surrounding it prevent either the active response or
        // queued work from committing after this point.
        self.shared.publication.cancel();
        self.manifest_queue.close();
    }
}

/// Where downloaded samples are kept: `RUSTEL_SAMPLE_CACHE` when it is set,
/// else `cache/samples` beside the configuration
/// (`~/.rustel/cache/samples`), so everything rustel keeps is in one folder.
fn cache_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os(product::SAMPLE_CACHE_ENV) {
        return PathBuf::from(explicit);
    }
    crate::config_dir::canonical()
        .unwrap_or_else(|| std::env::temp_dir().join(product::CACHE_DIRECTORY_NAME))
        .join(CACHE_DIRECTORY_BESIDE_CONFIG)
        .join(product::SAMPLES_DIRECTORY_NAME)
}

/// The folder the caches share beside the configuration.
const CACHE_DIRECTORY_BESIDE_CONFIG: &str = "cache";

#[derive(Clone, Copy, Debug)]
struct ScoreCacheLimits {
    global_bytes: u64,
    global_entries: usize,
    session_bytes: u64,
    session_entries: usize,
}

#[derive(Debug, Default)]
struct ScoreCacheUsage {
    bytes: u64,
    entries: usize,
}

#[derive(Debug)]
struct ScoreCache {
    base: PathBuf,
    limits: ScoreCacheLimits,
    session: Mutex<ScoreCacheUsage>,
}

/// A response's interpretation is part of its persistent identity.
///
/// One URL can legally be named as either a sample map or an audio file. A
/// validator for one kind must never reject and evict bytes cached for the
/// other kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScoreCacheKind {
    Manifest,
    Audio,
}

/// Whether the cache entry was admitted under CORS consent or an exact grant.
///
/// Trust mode is part of the persistent identity so a grant-only fetch (no
/// `Access-Control-Allow-Origin`) cannot later satisfy the public-https/CORS
/// policy for the same URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScoreCacheTrust {
    Cors,
    Grant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheAdmission {
    Stored,
    AlreadyPresent,
    Refused,
}

#[derive(Debug)]
struct CacheCommitError {
    error: std::io::Error,
    /// The final name is already visible, although its parent directory could
    /// not be synced. Quota accounting must not treat it as absent.
    published: bool,
}

static NEXT_CACHE_STAGE: AtomicUsize = AtomicUsize::new(0);

struct StagedCacheFile {
    path: PathBuf,
    published: bool,
}

#[derive(Clone, Copy)]
struct CacheFetchScope<'a> {
    budget: &'a sample_fetch::FetchBudget,
    publication: &'a PublicationGate,
}

/// Which decoder a sample URL asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Wav,
    Mp3,
    Ogg,
}

/// Take `count` slots of the sample bank: ids handed back through
/// [`SampleLibrary::release_ids`] first, oldest first, then fresh ones off
/// the counter. The ids need not run consecutively - a font keeps each
/// zone's own - and nothing is taken when the bank cannot seat the whole
/// request.
fn reserve_sample_ids(shared: &Shared, count: usize) -> Result<Vec<SampleId>, String> {
    let capacity = u32::try_from(SAMPLE_BANK_CAPACITY).expect("sample capacity fits u32");
    let mut free = shared.free_ids.lock().expect("free sample ids");
    let reused = free.len().min(count);
    let fresh =
        u32::try_from(count - reused).map_err(|_| "sample bank capacity exceeded".to_owned())?;
    let mut next = shared.next_id.load(Ordering::Relaxed);
    let first_fresh = loop {
        let Some(end) = next.checked_add(fresh) else {
            return Err("sample bank capacity exceeded".to_owned());
        };
        if end > capacity {
            return Err(format!(
                "sample bank capacity exceeded (maximum {} decoded assets)",
                SAMPLE_BANK_CAPACITY - 1
            ));
        }
        match shared
            .next_id
            .compare_exchange_weak(next, end, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break next,
            Err(observed) => next = observed,
        }
    };
    let mut ids: Vec<SampleId> = free.drain(..reused).collect();
    ids.extend((first_fresh..first_fresh + fresh).map(SampleId));
    Ok(ids)
}

/// A source-string shorthand. [`SHORTHANDS`] defines the supported forms;
/// [`read_source`] resolves them for both manifests and base URLs.
struct Shorthand {
    /// What a score writes, up to and including whatever ends it: `local:`
    /// and friends carry their colon, and `shabda/speech` is spelled with a
    /// slash and continues with a grammar of its own.
    prefix: &'static str,
    kind: ShorthandKind,
}

enum ShorthandKind {
    /// A folder beneath the root the host granted.
    LocalFolder,
    /// `user[/repo[/branch[/dir]]]` under raw.githubusercontent.
    GitHub,
    /// Another way of writing one of the above, with `{}` taking whatever
    /// followed the colon.
    Spelling(&'static str),
    /// `[/<language>/<gender>]:<words>` - the one shorthand with a grammar
    /// rather than a substitution. The address and both defaults still live
    /// on the row, so what changes when the service does is still a row.
    Speech {
        pattern: &'static str,
        language: &'static str,
        gender: &'static str,
    },
}

/// Every shorthand there is. A row, not a branch.
///
/// The longest matching prefix wins, so `shabda/speech` is read as itself
/// rather than as `shabda` with a strange tail, and the order rows are
/// written in carries no meaning.
static SHORTHANDS: &[Shorthand] = &[
    Shorthand {
        prefix: "local:",
        kind: ShorthandKind::LocalFolder,
    },
    Shorthand {
        prefix: "github:",
        kind: ShorthandKind::GitHub,
    },
    Shorthand {
        prefix: "bubo:",
        kind: ShorthandKind::Spelling("github:Bubobubobubobubo/dough-{}"),
    },
    // `samples('shabda:bass,kick')` searches Freesound by word and returns
    // a sample map from the Shabda service.
    Shorthand {
        prefix: "shabda:",
        kind: ShorthandKind::Spelling("https://shabda.ndre.gr/{}.json?strudel=1"),
    },
    // `samples('shabda/speech/en-US/m:music,vocode')` requests speech
    // samples, with optional language and gender settings before the colon.
    Shorthand {
        prefix: "shabda/speech",
        kind: ShorthandKind::Speech {
            pattern: "https://shabda.ndre.gr/speech/{words}.json\
                      ?gender={gender}&language={language}&strudel=1",
            language: "en-GB",
            gender: "f",
        },
    },
];

/// What a source string turned out to name.
enum SampleSource {
    /// A folder to scan, as written after the prefix and not yet trimmed
    /// or checked against the granted root.
    LocalFolder(String),
    /// A manifest or base URL to fetch.
    Url(String),
}

/// `githubPath`: `user[/repo[/branch[/dir…]]]` with repo
/// defaulting to "samples" and branch to "main".
const GITHUB_SAMPLE_MANIFEST: &str = "strudel.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleAudioKind {
    Wav,
    Mp3,
    Ogg,
}

pub(crate) const MAX_SAMPLE_MANIFEST_ENTRIES: usize = 16_384;
pub(crate) const MAX_SAMPLE_SCAN_ENTRIES: usize = 65_536;
pub(crate) const MAX_SAMPLE_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SAMPLE_SCAN_WORK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SampleScanLimits {
    pub(crate) examined_entries: usize,
    pub(crate) manifest_entries: usize,
    pub(crate) manifest_bytes: usize,
    pub(crate) working_bytes: usize,
}

#[derive(Debug)]
pub(crate) enum SampleFolderScanError {
    Io(String),
    Empty(String),
    Limit {
        resource: &'static str,
        limit: usize,
    },
}

/// What a prefetch started: the files it requested now, a request queued
/// behind manifests still loading, or nothing, for a name no bank knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchStatus {
    Requested(usize),
    Deferred,
    Unknown,
}

/// Whether a library starts the thread that fetches and decodes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Loading {
    /// The product, and every test whose subject is registration reaching a
    /// real loader.
    InBackground,
    /// Only for a test that reads the job queue itself.
    #[cfg(any(test, feature = "test-support"))]
    NotStarted,
}

impl SampleLibrary {
    /// Register score data with exactly one origin granted.
    ///
    /// The grant has to be real: with no origin the entry would be refused for
    /// want of permission, and a test asserting the ADDRESS guard would pass
    /// without ever reaching it.
    #[cfg(test)]
    fn register_custom_for_test(&self, origin: &str, map_json: &str) -> Result<(), String> {
        let mut access = ScoreSampleAccess::denied();
        access
            .permit_origin(origin)
            .expect("test origin is well formed");
        self.register_score_custom(map_json, None, &access)
    }

    /// Record `url` as decoded under `id`, the way a finished load does.
    /// For tests outside this module that need the state a host sees after
    /// a sample has been published and taken.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn remember_ready_for_test(&self, url: &str, id: SampleId) {
        let url: Arc<str> = Arc::from(url);
        self.banks
            .write()
            .expect("default banks")
            .insert(url.to_string(), Bank::Array(vec![url.clone()]));
        self.shared
            .by_url
            .write()
            .expect("sample url table")
            .insert(
                url,
                UrlState::Ready {
                    id,
                    duration_secs: 1.0,
                },
            );
    }

    /// Publish `decoded` for `url` under `id` the way a finished load does:
    /// Ready in the tables and waiting in the ready queue for a device to
    /// install it.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn publish_ready_for_test(&self, url: &str, id: SampleId, decoded: DecodedSample) {
        self.remember_ready_for_test(url, id);
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .push((id, decoded));
    }

    /// Leave the tombstone `forget_decoded` leaves, on an id the library
    /// never tabled - a retained sample installed straight into a device.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn forget_id_for_test(&self, id: SampleId) {
        self.shared.ready.lock().expect("ready samples").identities[id.0 as usize] =
            DecodedIdentity::Forgotten;
    }

    /// Whether anything has asked for `url` yet: loading, in, or failed.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn asked_for_test(&self, url: &str) -> bool {
        self.shared
            .by_url
            .read()
            .expect("sample url table")
            .contains_key(&Arc::from(url))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn knows_ready_for_test(&self, url: &str) -> bool {
        matches!(
            self.shared
                .by_url
                .read()
                .expect("sample url table")
                .get(&Arc::from(url)),
            Some(UrlState::Ready { .. })
        )
    }

    /// An empty library with live loaders, for tests that exercise
    /// registration without touching the pinned banks or the network.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn empty() -> Self {
        Self::with_background_loaders(HashMap::new(), Vec::new(), HashMap::new(), String::new())
            .expect("empty library")
    }

    /// An empty library whose loader thread was never started.
    ///
    /// For a test that asserts the content of the job queue. `empty()` runs
    /// the real loader, which pops jobs, so a test that reads the queue would
    /// race the loader for the same job.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn empty_without_loading() -> Self {
        Self::with_background_loaders_at(
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            String::new(),
            cache_dir(),
            Loading::NotStarted,
        )
        .expect("empty library")
    }

    /// A real lookup that stays Loading without submitting any loader work.
    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn with_loading_sample_for_test(name: &str) -> Self {
        let library = Self::empty();
        let url: Arc<str> = Arc::from("held-sample-for-test.wav");
        library
            .shared
            .by_url
            .write()
            .expect("sample url table")
            .insert(Arc::clone(&url), UrlState::Loading);
        library
            .custom
            .write()
            .expect("custom banks")
            .insert(name.to_owned(), Bank::Array(vec![url]));
        assert!(
            library.shared.jobs.try_pop().is_none(),
            "no sample job was submitted"
        );
        assert_eq!(library.manifests_pending(), 0);
        library
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn fail_loading_sample_for_test(&self) {
        let url: Arc<str> = Arc::from("held-sample-for-test.wav");
        let mut by_url = self.shared.by_url.write().expect("sample url table");
        assert!(matches!(by_url.get(&url), Some(UrlState::Loading)));
        by_url.insert(url, UrlState::Failed { at: Instant::now() });
        drop(by_url);
        self.shared.note_settled();
        assert!(self.shared.jobs.try_pop().is_none());
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn finish_loading_sample_for_test(&self) -> SampleId {
        self.finish_loading_sample_frames_for_test(480)
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn finish_loading_sample_frames_for_test(&self, frames: usize) -> SampleId {
        self.finish_loading_url_for_test(Arc::from("held-sample-for-test.wav"), frames)
    }

    /// A bank keyed by note, each key's file Loading without any loader
    /// work until [`Self::finish_note_key_for_test`].
    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn with_loading_note_bank_for_test(name: &str, keys: &[f64]) -> Self {
        let library = Self::empty();
        let notes = keys
            .iter()
            .map(|key| {
                let url: Arc<str> = Arc::from(format!("note-key-{key}.wav"));
                library
                    .shared
                    .by_url
                    .write()
                    .expect("sample url table")
                    .insert(Arc::clone(&url), UrlState::Loading);
                (*key, vec![url])
            })
            .collect();
        library
            .custom
            .write()
            .expect("custom banks")
            .insert(name.to_owned(), Bank::Notes(notes));
        library
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn finish_note_key_for_test(&self, key: f64) -> SampleId {
        self.finish_loading_url_for_test(Arc::from(format!("note-key-{key}.wav")), 480)
    }

    /// Banks of one file each that nobody has asked for yet, on a library
    /// whose loader never starts, so what an ask queues stays readable
    /// through [`Self::queued_loads_for_test`].
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_unasked_banks_for_test(names: &[&str]) -> Self {
        let library = Self::empty_without_loading();
        for name in names {
            library.custom.write().expect("custom banks").insert(
                (*name).to_owned(),
                Bank::Array(vec![Arc::from(format!("unasked-{name}.wav"))]),
            );
        }
        library
    }

    /// The loads queued to play now, and those queued as bets.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn queued_loads_for_test(&self) -> (usize, usize) {
        let state = self.shared.jobs.state.lock().expect("sample load queue");
        (state.now.len(), state.bets.len())
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    fn finish_loading_url_for_test(&self, url: Arc<str>, frames: usize) -> SampleId {
        assert!(matches!(
            self.shared
                .by_url
                .read()
                .expect("sample url table")
                .get(&url),
            Some(UrlState::Loading)
        ));
        let id = reserve_sample_ids(&self.shared, 1).expect("sample slot")[0];
        let decoded =
            DecodedSample::from_parts(48_000, 1, vec![0.25; frames]).expect("decoded sample");
        let duration_secs = decoded.frames() as f64 / f64::from(decoded.sample_rate());
        self.shared
            .ready
            .lock()
            .expect("ready samples")
            .push((id, decoded));
        self.shared
            .by_url
            .write()
            .expect("sample url table")
            .insert(url, UrlState::Ready { id, duration_secs });
        self.shared.note_settled();
        assert!(self.shared.jobs.try_pop().is_none());
        id
    }

    /// Keep the manifest worker on a job of its own until the hold drops:
    /// a busy loader, for tests that must not reach the network.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn hold_manifest_worker_for_test(&self) -> ManifestWorkerHold {
        let (reached, release) = self
            .shared
            .publication
            .install_test_barrier(PublicationKind::Custom);
        self.enqueue_manifest_work_async(ManifestWork::Custom {
            effects: vec![(r#"{"held":"http://127.0.0.1:9/held.wav"}"#.to_owned(), None)],
            preloads: Vec::new(),
            intent: PrefetchIntent::Explicit,
            access: ManifestAccess::Trusted,
            continue_on_error: true,
            layer: BankLayer::Score,
        })
        .expect("the held job");
        reached
            .recv_timeout(Duration::from_secs(30))
            .expect("the worker took the held job");
        ManifestWorkerHold { release }
    }

    /// Queue `specs` as one evaluation's `samples("…")` batch, each map
    /// marked loading as such a batch's are, on `budget` in place of the
    /// minute a batch gets.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn register_samples_batch_for_test(&self, specs: &[&str], budget: Duration) {
        let effects = specs
            .iter()
            .map(|spec| (serde_json::to_string(spec).expect("a string"), None))
            .collect::<Vec<_>>();
        for (map, _) in &effects {
            self.mark_source_loading(map);
        }
        self.enqueue_manifest_work_for(
            ManifestWork::Custom {
                effects,
                preloads: Vec::new(),
                intent: PrefetchIntent::Explicit,
                access: ManifestAccess::Trusted,
                continue_on_error: true,
                layer: BankLayer::Score,
            },
            budget,
        )
        .expect("the batch");
    }
}

/// The last segment of a url, as a person would name the file: what comes
/// after the final `/`, before any `?` or `#`, with percent-escapes undone.
fn file_name_of_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut out = Vec::with_capacity(name.len());
    let mut rest = name.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%'
            && let [hi, lo, ..] = tail
            && let (Some(hi), Some(lo)) =
                (char::from(*hi).to_digit(16), char::from(*lo).to_digit(16))
        {
            out.push((hi * 16 + lo) as u8);
            rest = &tail[2..];
        } else {
            out.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| name.to_owned())
}

/// Queue an unloaded font once, or retry a rested failure for a real ask.
/// A speculative request that is now needed for playback moves up the line;
/// bets never spend retries and fresh failures retain their cooldown.
fn queue_font(shared: &Shared, font: &Arc<str>, priority: LoadPriority) {
    let mut table = shared.fonts.write().expect("font table");
    let ask = match table.get(font) {
        None => true,
        Some(FontState::Failed { at }) => {
            priority == LoadPriority::Now && at.elapsed() >= FAILED_RETRY_AFTER
        }
        Some(FontState::Loading | FontState::Ready(_)) => false,
    };
    if ask {
        table.insert(font.clone(), FontState::Loading);
        shared.font_jobs.push(font.clone(), priority);
    } else if priority == LoadPriority::Now && matches!(table.get(font), Some(FontState::Loading)) {
        shared.font_jobs.promote(font);
    }
}

impl SampleLibrary {
    /// The lookup behind [`SampleLookup::resolve`], with a say in where a
    /// fetch it starts joins the loader's line.
    fn resolve_with_priority(
        &self,
        s: &str,
        n: f64,
        midi: f64,
        priority: LoadPriority,
    ) -> SampleResolution {
        enum Chosen {
            Font(SampleResolution),
            Sample(Arc<str>, f64),
            Empty,
        }
        let chosen = self.look_up(s, |named| {
            let bank = match named {
                Named::Font(fonts) => {
                    return Chosen::Font(self.resolve_soundfont(fonts, n, midi, priority));
                }
                Named::Bank(bank) => bank,
            };
            let Some((url, transpose)) = pick_from_bank(bank, n, midi) else {
                return Chosen::Empty;
            };
            Chosen::Sample(url, transpose)
        });
        let (url, transpose) = match chosen {
            Some(Chosen::Font(resolution)) => return resolution,
            Some(Chosen::Sample(url, transpose)) => (url, transpose),
            Some(Chosen::Empty) => return SampleResolution::Failed,
            None => {
                return if self.shared.manifest_pending.load(Ordering::Acquire) != 0 {
                    SampleResolution::Loading
                } else {
                    SampleResolution::Unknown
                };
            }
        };
        match ensure_loading_shared(&self.shared, &url, DecodeRate::for_sound(s), priority) {
            SampleResolution::Found {
                id, duration_secs, ..
            } => SampleResolution::Found {
                id,
                transpose,
                duration_secs,
                loop_secs: None,
                envelope_peak: 1.0,
                soundfont: false,
            },
            other => other,
        }
    }
}

impl SampleLookup for SampleLibrary {
    fn resolve(&self, s: &str, n: f64, midi: f64) -> SampleResolution {
        // A sound the engine asks for is one about to play.
        self.resolve_with_priority(s, n, midi, LoadPriority::Now)
    }

    fn bus_names(&self) -> Option<&rustel_voice::BusNames> {
        Some(&self.bus_names)
    }

    #[cfg(feature = "vst")]
    fn insert(
        &self,
        request: &rustel_voice::PluginRequest<'_>,
    ) -> Result<Option<rustel_audio::InsertControls>, String> {
        crate::vst::controls(request)
    }

    fn insert_speech(&self, request: &SpeechRequest<'_>) -> Result<SampleResolution, String> {
        speech::speak(&self.shared, request)
    }
}

#[cfg(test)]
use loading::tests::live_warm_tests;

#[cfg(test)]
use manifests::tests::manifest_congestion_tests;

#[cfg(test)]
use lookup::tests::variant_tests;

#[cfg(test)]
use manifests::tests::manifest_worker_tests;

#[cfg(test)]
use loading::tests::codec_tests;

#[cfg(test)]
pub(crate) mod wait_tests {
    //! Offline sample waits stop without making the loader finish its work.

    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use super::SampleLibrary;

    thread_local! {
        static WAIT_ENTERED: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
    }

    /// Observe a pending sample wait on this thread after its cancellation guard.
    pub(crate) fn watch_next_wait() -> Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        WAIT_ENTERED.with(|slot| {
            assert!(slot.replace(Some(sender)).is_none(), "wait already watched");
        });
        receiver
    }

    pub(super) fn entering_wait() {
        WAIT_ENTERED.with(|slot| {
            if let Some(sender) = slot.borrow_mut().take() {
                let _ = sender.send(());
            }
        });
    }

    fn assert_empty_render_cancels(cancel_during_wait: bool) {
        let library = SampleLibrary::empty();
        let hold = library.hold_manifest_worker_for_test();
        let cancellation = AtomicBool::new(!cancel_during_wait);
        let folder = tempfile::tempdir().expect("render directory");
        let path = folder.path().join("cancelled.wav");
        let (finished, completion) = mpsc::channel();
        let (watching, watch) = mpsc::channel();

        let (result, entered, pending) = std::thread::scope(|scope| {
            scope.spawn(|| {
                if cancel_during_wait {
                    watching.send(watch_next_wait()).expect("wait observer");
                }
                let rendered = crate::render::write_scalar_wav_controlled_with_dispatch(
                    &path,
                    48_000,
                    0.1,
                    &[],
                    0.5,
                    Some(&library),
                    false,
                    rustel_audio::RenderControl {
                        cancelled: Some(&cancellation),
                        ..Default::default()
                    },
                    rustel_audio::WavSampleFormat::Pcm16,
                    rustel_audio::DspDispatch::portable(),
                    128,
                    None,
                );
                let _ = finished.send(rendered);
            });
            let entered = if cancel_during_wait {
                Some(
                    watch
                        .recv_timeout(Duration::from_secs(2))
                        .and_then(|entered| entered.recv_timeout(Duration::from_secs(2))),
                )
            } else {
                None
            };
            cancellation.store(true, Ordering::Relaxed);
            let result = completion.recv_timeout(Duration::from_secs(2));
            let pending = library.manifests_pending();
            // Release before the scoped join even if the regression timed out:
            // the held job is an inline map, with no network work behind it.
            drop(hold);
            (result, entered, pending)
        });

        if let Some(entered) = entered {
            entered.expect("cancellation arrived during the sample wait");
        }
        let error = result
            .expect("cancelled render returned while the manifest worker was held")
            .expect_err("a stopped render is not a completed file");
        assert_eq!(error.to_string(), rustel_audio::RENDER_CANCELLED);
        assert!(pending > 0, "the render waited for the held manifest");
        assert!(!path.exists(), "sample cancellation reached audio writing");
    }

    #[test]
    fn stopped_empty_render_does_not_wait_for_sample_manifests() {
        assert_empty_render_cancels(false);
    }

    #[test]
    fn empty_render_cancels_during_the_sample_manifest_wait() {
        assert_empty_render_cancels(true);
    }

    #[test]
    fn ordinary_sample_wait_still_waits_for_the_manifest_to_finish() {
        let library = SampleLibrary::empty();
        let hold = library.hold_manifest_worker_for_test();
        let (watching, watch) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let (entered, before_release, after_release) = std::thread::scope(|scope| {
            scope.spawn(|| {
                watching.send(watch_next_wait()).expect("wait observer");
                library.wait_until_idle(Duration::from_secs(5));
                let _ = finished.send(());
            });
            let entered = watch
                .recv_timeout(Duration::from_secs(2))
                .and_then(|entered| entered.recv_timeout(Duration::from_secs(2)));
            let before_release = completion.try_recv();
            drop(hold);
            let after_release = completion.recv_timeout(Duration::from_secs(2));
            (entered, before_release, after_release)
        });

        entered.expect("the ordinary wait reached the pending manifest");
        assert!(matches!(before_release, Err(mpsc::TryRecvError::Empty)));
        after_release.expect("the ordinary wait finished after the worker was released");
        assert_eq!(library.manifests_pending(), 0);
    }
}
