//! Headless Session: evaluate → query → schedule generations.
//!
//! Score evaluation and replacement live in `session/evaluation.rs`; finite
//! playback and offline rendering live in `session/playback.rs`. The wait of
//! a live edit for its plugin copies lives in `session/plugin_wait.rs`. This
//! module owns the shared state and live scheduling.

mod assets;
mod diagnostics;
mod installation;
mod output;
mod query;
mod scheduling;

mod evaluation;
mod playback;
mod recovery;

pub use recovery::{catch_score_panic, panic_is_contained};

#[cfg(any(test, feature = "test-support"))]
pub use recovery::SessionPanicPoint;

#[cfg(feature = "device-audio")]
pub(crate) mod confirmation;
#[cfg(feature = "device-audio")]
mod handover;
#[cfg(all(feature = "vst", feature = "device-audio"))]
mod plugin_wait;
#[cfg(all(feature = "vst", feature = "device-audio"))]
pub use plugin_wait::PLUGIN_WAIT;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

use rustel_audio::TakeoverCut;
use rustel_core::host_value::Pointer;
use rustel_core::settings::RuntimeSettings;
use rustel_core::{Hap, Pattern, QueryArcOutcome, State, TimeSpan};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, ScoreEffects, Slot};
use rustel_scheduler::{Clock, Scheduler, Transport, VirtualClock};
use rustel_transpiler::TranspileOptions;

use crate::hap_json::{
    BenchMetrics, OnsetEventJson, PlayReport, QueryReport, RenderReport, ValueJson, fraction_label,
    haps_to_json,
};
#[cfg(feature = "osc")]
use crate::osc_bridge::ScoreOscAccess;
#[cfg(any(feature = "device-audio", test))]
use crate::producer::{ProducerPhase, ProducerTurnRecord};
use crate::render::{RenderFormat, write_onset_dump, write_silent_wav};
use crate::samples::ScoreSampleAccess;
use crate::score_import::ScoreImportAccess;

const DEFAULT_CPS: f64 = 0.5;
const DEFAULT_HORIZON: f64 = 0.5;
pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u16 = 2;
const SCORE_CPU_BUDGET: Duration = Duration::from_secs(2);
const PREBAKE_CPU_BUDGET: Duration = Duration::from_secs(2);
/// Maximum elapsed time before QuickJS is interrupted in one top-level query
/// or scheduler tick. This is deliberately separate from score construction:
/// query-time callbacks run later, while the producer fills its scheduling
/// horizon. It is not process CPU-time accounting or a total-query bound.
const QUERY_JS_CPU_BUDGET: Duration = Duration::from_secs(2);

/// How many cycles a replacement whose first window refused every onset is
/// looked through for anything it plays before it is refused: enough for a
/// bank or sound that alternates by cycle, as `<a b c d>` does, to come
/// round to one that is loaded.
#[cfg(feature = "device-audio")]
const REPLACEMENT_LOOK_AHEAD_CYCLES: i128 = 4;

/// What a replacement's look-ahead found in the cycles ahead of its refused
/// first window. When several apply the priority is: an invalid control
/// refuses the whole replacement even beside a rendered sibling, exactly as
/// one in the first window does; then a rendered onset publishes; then a
/// sample still on its way waits.
#[cfg(feature = "device-audio")]
enum ReplacementLookAhead {
    /// An onset rendered: the replacement can play and is published.
    Renders,
    /// Nothing rendered yet, but a sample that is still loading would. The
    /// replacement is refused as retryable and the last audible score
    /// keeps playing. Publishing now would latch silence if the fetch
    /// never completes.
    Loading(String),
    /// An invalid control waits ahead, which refuses the whole
    /// replacement as the same voice in the first window would.
    InvalidControl(String),
    /// Nothing renders and nothing is coming.
    Nothing,
}

/// Keep live callback-bearing queries small enough to complete atomically
/// inside the replacement-prefill deadline. Further horizon cover is filled
/// by subsequent producer turns.
const MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES: f64 = 1.0;
/// Extra future music retained for callback-bearing scores whose ordinary
/// horizon already queries whole cycles. This absorbs one cycle-cost outlier
/// without changing the query windows seen by patterns.
#[cfg(feature = "device-audio")]
const LIVE_IMPURE_BURST_RESERVE_CYCLES: f64 = 1.0;
/// Bound the reserve in wall time so a low-tempo score or large device margin
/// cannot turn producer resilience into seconds of retained event traffic.
#[cfg(feature = "device-audio")]
const LIVE_IMPURE_BURST_RESERVE_MAX: Duration = Duration::from_millis(500);
/// Leave part of each cycle for event conversion, ring transfer and ordinary
/// producer scheduling instead of letting pattern work consume all of it.
const LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE: f64 = 0.75;
/// Immediate replacements keep the old generation audible for at least this
/// long while the accepted candidate fills its first scheduling runway. A
/// measured-heavy candidate grows from this floor with its bounded one-cycle
/// query budget; see [`Session::live_replacement_takeover_headroom`].
#[cfg(any(feature = "device-audio", test))]
const LIVE_REPLACEMENT_MIN_HEADROOM: Duration = Duration::from_millis(80);
/// Smallest elapsed-time slice offered to a live impure scheduler query.
/// Smaller tokens are dominated by ordinary scheduling jitter, so a steady
/// producer that cannot afford this much fails closed before entering JS.
pub(crate) const MIN_LIVE_QUERY_JS_BUDGET: Duration = Duration::from_millis(25);

/// Set in the onset id of every preview trace - a hap read ahead for the
/// painters, never queued to the audio - so it can never collide with the
/// scheduler's dense counter, which starts at zero.
pub const PREVIEW_ONSET_ID_FLAG: u64 = 1 << 63;

/// The JavaScript a preview query may run. It runs on the engine thread
/// between producer steps, so a callback-bearing score gets the live
/// floor, never the inspection ceiling; over budget, the preview is
/// simply skipped this time.
const PREVIEW_QUERY_JS_BUDGET: Duration = MIN_LIVE_QUERY_JS_BUDGET;
/// Query budget when the live scheduling horizon has run out.
/// With no queued horizon left to protect, allow more than the steady-state
/// floor so a dense score can finish a window and resume playback.
pub(crate) const LIVE_QUERY_RECOVERY_BUDGET: Duration = Duration::from_millis(250);
#[cfg(feature = "device-audio")]
const LIVE_RELOAD_SHIELD_MAX_EXTRA: Duration = Duration::from_millis(2250);
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
/// Do not execute a stable watched setup under a token deadline so small that
/// ordinary scheduling jitter turns valid code into a committed rejection.
/// Below this floor the exact file identity remains Pending and retryable.
const MIN_LIVE_PREBAKE_CPU_BUDGET: Duration = Duration::from_millis(25);
const MAX_PENDING_SESSION_DIAGNOSTICS: usize = 64;
/// The time between 2 looks at the plugin copies a start or an edit waits
/// for. Each look asks the plugin host for each copy.
#[cfg(feature = "vst")]
const PLUGIN_LOOK_EVERY: Duration = Duration::from_millis(20);
#[cfg(feature = "midi")]
const MAX_MIDI_INTENTS_PER_PASS: usize = 256;
#[cfg(feature = "midi")]
const MAX_MIDI_MESSAGES_PER_PASS: usize = 4_096;
#[cfg(feature = "midi")]
const MAX_MIDI_RETAINED_BYTES_PER_PASS: usize = 256 * 1_024;
#[cfg(feature = "midi")]
const MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS: usize = 1_024;
#[cfg(feature = "osc")]
const MAX_OSC_INTENTS_PER_PASS: usize = 256;
#[cfg(feature = "osc")]
const MAX_OSC_BYTES_PER_PASS: usize = 256 * 1024;
#[cfg(feature = "osc")]
const MAX_OSC_ROUTE_ATTEMPTS_PER_PASS: usize = 1_024;
#[cfg(feature = "serial")]
const MAX_SERIAL_INTENTS_PER_PASS: usize = 256;
#[cfg(feature = "serial")]
const MAX_SERIAL_BYTES_PER_PASS: usize = 256 * 1024;
#[cfg(feature = "serial")]
const MAX_SERIAL_ROUTE_ATTEMPTS_PER_PASS: usize = 1_024;

#[cfg(feature = "osc")]
#[derive(Clone, Debug, Eq, PartialEq)]
enum ReportedOscRefusal {
    Destination(std::net::SocketAddr),
    InvalidDestination { host: String, port: u16 },
    OversizedHost,
}

#[cfg(feature = "osc")]
fn osc_refusal_key(host: &str, port: u16) -> ReportedOscRefusal {
    if host.len() > rustel_osc::MAX_OSC_HOST_BYTES {
        ReportedOscRefusal::OversizedHost
    } else if let Ok(destination) = rustel_osc::parse_osc_destination(host, port) {
        let ip = match destination.ip() {
            std::net::IpAddr::V6(ip) => ip
                .to_ipv4_mapped()
                .map(std::net::IpAddr::V4)
                .unwrap_or(std::net::IpAddr::V6(ip)),
            std::net::IpAddr::V4(ip) => std::net::IpAddr::V4(ip),
        };
        ReportedOscRefusal::Destination(std::net::SocketAddr::new(ip, destination.port()))
    } else {
        ReportedOscRefusal::InvalidDestination {
            host: host.to_owned(),
            port,
        }
    }
}

/// Presentation-neutral notice produced while a Session keeps running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDiagnostic {
    pub kind: String,
    pub message: String,
    pub recoverable: bool,
}

/// The kind of a refusal when the library has not finished loading the
/// sound.
///
/// Distinct from `voice-refused`, where the score asks for something this
/// engine cannot play. A surface matches on this kind, not on the message
/// text, so it can show a decode in progress as waiting and not as an error.
pub const SAMPLE_LOADING_DIAGNOSTIC: &str = "sample-loading";
/// The kind of a voice resolver notice: part of a voice was skipped or
/// substituted and the rest still plays. The message is one sentence for
/// people; direct logging prints the notice's JSON record instead.
pub const VOICE_NOTICE_DIAGNOSTIC: &str = "voice-notice";
/// The kind of a plugin line: a note plays with no plugin or with one value
/// less, or the plugin thread has a problem. The set plays on. A text goes
/// out one time for each evaluation.
pub const PLUGIN_DIAGNOSTIC: &str = "plugin";
/// The kind a still-loading refusal carries when its notes wait rather than
/// being skipped: a rewind's first window held whole until its samples are
/// in, or a sample just ahead that an unpublished update waits for. An edit
/// that waits for its plugin copies gives one line of this kind too: see
/// `Session::reload_live`.
pub const SAMPLE_AWAITED_DIAGNOSTIC: &str = "sample-awaited";
/// What a watched score evaluation gets when the horizon is already spent.
///
/// The floor below protects a horizon that still exists; once it is gone,
/// refusing an evaluation only guarantees the next one is refused too, and the
/// artist is left typing into a set that has stopped listening.
#[cfg(any(feature = "device-audio", test))]
const LIVE_SCORE_RECOVERY_BUDGET: Duration = Duration::from_millis(400);
/// The same floor applies to watched score JavaScript: below it, retaining the
/// stable file identity for a later attempt is safer than running ordinary
/// construction code under a deadline dominated by scheduling jitter.
#[cfg(any(feature = "device-audio", test))]
const MIN_LIVE_SCORE_CPU_BUDGET: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LivePrebakeAttempt {
    Applied,
    Deferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) enum LiveScoreAttempt {
    Applied(u64),
    Deferred,
    /// The edit waits for its plugin copies: see [`Session::reload_live`].
    #[cfg(all(feature = "vst", feature = "device-audio"))]
    Waiting,
}

/// What a live reload did: see [`Session::reload_live`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(all(feature = "vst", feature = "device-audio"))]
pub enum LiveReload {
    /// The score is in play, with this generation.
    Installed(u64),
    /// A plugin copy of the edit is not ready. The score in play plays on,
    /// and the session holds the text of the edit.
    Waiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "device-audio")]
pub(crate) enum RollbackAttempt {
    Applied,
    Deferred,
    Unavailable,
}

#[cfg(feature = "device-audio")]
pub(crate) struct LiveAudioBatch {
    /// In onset order. The target frames can be out of order: a voice can
    /// be aimed before its onset, as a stretched one is.
    pub(crate) events: Vec<rustel_audio::AudioEvent>,
    /// Immutable decoded payload selected for each converted prefill onset.
    /// Empty for steady turns, which do not create confirmation windows.
    pub(crate) sample_identities: Vec<Option<u64>>,
    /// Disjoint dispositions of this finite scheduling pass. These describe
    /// conversion/accepted external intent, not callback or external delivery.
    pub(crate) dispositions: confirmation::WindowDispositions,
    /// End of the actually queried/drained time span, not a requested horizon
    /// that an atomic query may only have filled in part.
    pub(crate) queried_through_frame: u64,
    /// A caught query exception is not evidence of intentional silence.
    pub(crate) query_threw: bool,
    /// A fresh query, converted audio, or accepted external output can finish
    /// prefill. An empty transfer from an unchanged full horizon cannot.
    pub(crate) prefill_progress: bool,
    /// Starts immediately after Scheduler accepted the tick, before draining,
    /// JSON/audio conversion, generation handoff, or ring transfer.
    pub(crate) tail_started: Instant,
}

/// One onset's sound in a queried window: see [`Session::window_sounds`].
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSound {
    /// The sound as written.
    pub s: String,
    /// `{bank}_{s}` when a bank is set: the spelling the voice looks up.
    pub banked: Option<String>,
    /// The variant the onset's `n` picks.
    pub n: f64,
    /// The note, as a MIDI number: 36 when the onset has none.
    pub midi: f64,
}

#[cfg(feature = "device-audio")]
pub(crate) enum LiveAudioScheduleError {
    /// The candidate did not commit scheduler state and may be retried.
    Retryable(RuntimeError),
    /// As [`Self::Retryable`], when the sample library is still fetching
    /// what the score asks for: the same identity stays eligible and
    /// nothing is latched. A separate variant lets the engine's health
    /// telemetry tell a library that is still decoding from a machine
    /// that cannot keep up. Only the second is pressure to report, so
    /// previews played one after another do not read as an overload.
    AwaitingSamples(RuntimeError),
    /// Scheduler state may already have advanced or drained by this point.
    /// A pending device-generation cutover must not publish an unchanged retry
    /// as successful silence after this failure.
    CandidateCommitted(RuntimeError),
    /// As [`Self::CandidateCommitted`], when the score is the reason: no
    /// compiled output can render what it asks for. Examples are an orbit
    /// past the last bus, a unison past the supersaw's ceiling, and a sound
    /// with no bank behind it.
    ///
    /// The candidate is committed, the last audible score is put back, and
    /// the player is told. A separate variant lets the engine's health
    /// telemetry tell a mistake in the score from a machine that cannot
    /// keep up, without a match on the message text.
    CandidateRefusedScore(RuntimeError),
}

#[cfg(feature = "device-audio")]
impl LiveAudioScheduleError {
    /// Whether the score's own content is what turned the candidate away,
    /// rather than a limit, a full ring or a host that fell behind.
    pub(crate) fn is_refused_score(&self) -> bool {
        matches!(self, Self::CandidateRefusedScore(_))
    }

    /// Whether this refusal is the library still fetching the score's
    /// samples: waiting, not falling behind.
    pub(crate) fn is_awaiting_samples(&self) -> bool {
        matches!(self, Self::AwaitingSamples(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) enum LiveQueryBudgetMode {
    Steady,
    InitialPrefill,
    ReplacementPrefill,
}

enum EvaluatedScore {
    JavaScript {
        pattern: Pattern,
        effects: Box<ScoreEffects>,
        settings: RuntimeSettings,
        csound: Option<crate::csound::Candidate>,
    },
    MiniFallback(Pattern),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NoPatternPolicy {
    Reject,
    InstallSilence,
}

enum ScheduleAttemptError {
    Atomic(RuntimeError),
    Partial(RuntimeError),
}

struct ScheduledWindow {
    events: Vec<OnsetEventJson>,
    tail_started: Instant,
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    status: rustel_scheduler::TickStatus,
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    query_threw: bool,
}

impl ScheduleAttemptError {
    fn into_runtime_error(self) -> RuntimeError {
        match self {
            Self::Atomic(error) | Self::Partial(error) => error,
        }
    }
}

/// How source was resolved into an active pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluateSource {
    /// Transpiled user JS via QuickJS + native pattern host bindings.
    JavaScript,
    /// Direct `rustel_mini` compile (Rust API fallback).
    MiniRust,
    /// Explicit Pattern installed without JS.
    Pattern,
}

/// Captured source and runtime settings retained for watchdog rollback.
/// Capturing this value does not establish consumer playback.
#[cfg(feature = "device-audio")]
#[derive(Clone)]
struct AudibleSource {
    generation: u64,
    source: Arc<str>,
    mini: bool,
    settings: RuntimeSettings,
    csound: crate::CsoundSnapshot,
    cps: f64,
    cycle_zero_time: f64,
}

/// Settings a Session is built with; [`Session::with_config`] validates them.
///
/// Start from [`SessionConfig::default`] and change fields directly or with
/// the `with_` methods. The struct is non-exhaustive, so a new setting does
/// not break a host's construction.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SessionConfig {
    pub cps: f64,
    /// Default voice budget, unless an accepted score calls setMaxPolyphony.
    pub max_polyphony: usize,
    pub horizon: f64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Prepared engine-owned DSP kernels for offline and finite device rendering.
    /// RustFFT planning and compiler-generated SIMD remain independent.
    pub dsp_dispatch: rustel_audio::DspDispatch,
    /// Filesystem and network resources that evaluated score text may select
    /// through `samples(...)`. Empty by default.
    pub score_sample_access: ScoreSampleAccess,
    /// The origins that evaluated text may import scripts from with
    /// `import('https://...')`. Empty by default.
    pub score_import_access: ScoreImportAccess,
    /// Destinations score-chosen `oschost` values may reach. Loopback only
    /// until the host grants additional IPs.
    #[cfg(feature = "osc")]
    pub score_osc_access: ScoreOscAccess,
    /// The host's pointer, which the score's `mousex` and `mousey` read.
    /// Without one they read 0.
    pub pointer: Option<Pointer>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cps: DEFAULT_CPS,
            max_polyphony: rustel_audio::MAX_POLYPHONY,
            horizon: DEFAULT_HORIZON,
            sample_rate: DEFAULT_SAMPLE_RATE,
            channels: DEFAULT_CHANNELS,
            dsp_dispatch: rustel_audio::DspDispatch::automatic(),
            score_sample_access: ScoreSampleAccess::denied(),
            score_import_access: ScoreImportAccess::denied(),
            #[cfg(feature = "osc")]
            score_osc_access: ScoreOscAccess::loopback_only(),
            pointer: None,
        }
    }
}

impl SessionConfig {
    /// Set the tempo in cycles per second.
    pub fn with_cps(mut self, cps: f64) -> Self {
        self.cps = cps;
        self
    }

    /// Set the default voice budget.
    pub fn with_max_polyphony(mut self, voices: usize) -> Self {
        self.max_polyphony = voices;
        self
    }

    /// Set the scheduling horizon in seconds.
    pub fn with_horizon(mut self, seconds: f64) -> Self {
        self.horizon = seconds;
        self
    }

    /// Set the rendering sample rate in Hz.
    pub fn with_sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate = hz;
        self
    }

    /// Set the output channel count.
    pub fn with_channels(mut self, channels: u16) -> Self {
        self.channels = channels;
        self
    }

    /// Select the engine-owned DSP kernels.
    pub fn with_dsp_dispatch(mut self, dispatch: rustel_audio::DspDispatch) -> Self {
        self.dsp_dispatch = dispatch;
        self
    }

    /// Grant score text access to sample sources.
    pub fn with_score_sample_access(mut self, access: ScoreSampleAccess) -> Self {
        self.score_sample_access = access;
        self
    }

    /// Trust origins for script imports.
    pub fn with_score_import_access(mut self, access: ScoreImportAccess) -> Self {
        self.score_import_access = access;
        self
    }

    /// Grant score-chosen OSC destinations.
    #[cfg(feature = "osc")]
    pub fn with_score_osc_access(mut self, access: ScoreOscAccess) -> Self {
        self.score_osc_access = access;
        self
    }

    /// Give the score a pointer the host writes.
    pub fn with_pointer(mut self, pointer: Pointer) -> Self {
        self.pointer = Some(pointer);
        self
    }
}

/// A Session failure; [`RuntimeError::kind`] names its category.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeError {
    Js(String),
    Mini(String),
    Io(std::io::Error),
    NoPattern,
    /// A signal asked us to stop before the work began.
    Interrupted(i32),
    /// The work was stopped part way through, at the caller's request.
    Cancelled,
    /// Score work panicked. The Session was rebuilt, or its recovery failed
    /// and it refuses further score work.
    Panic(String),
    /// A limit refused the work. The source is not at fault.
    ///
    /// A separate variant, not a `Message` recognised by its text. The CLI
    /// maps it to a distinct exit code, so a caller can retry with a shorter
    /// window instead of reporting a broken pattern.
    ResourceLimit(String),
    /// The native audio host or device rejected playback.
    Audio(String),
    /// This build omits the feature the work needs, such as `mp3-export`.
    Unsupported(String),
    /// The text imports a script from an origin that the host does not
    /// trust. Nothing was fetched.
    UntrustedImport {
        origin: String,
    },
    /// The text uses a native orchestra while the host does not permit
    /// one. Nothing ran.
    CsoundPermission,
    Message(String),
}

impl From<rustel_core::QueryLimit> for RuntimeError {
    fn from(limit: rustel_core::QueryLimit) -> Self {
        match limit {
            // A cancelled query is not a resource refusal and must not exit 3:
            // explicit cancellation has the same outcome as a signalled stop
            // mid-flight.
            rustel_core::QueryLimit::Cancelled => Self::Cancelled,
            other => Self::ResourceLimit(other.to_string()),
        }
    }
}

/// The host's failures are TYPED, so a refusal keeps its kind and exit code
/// rather than being classified by matching message text.
impl From<rustel_jsruntime::QueryError> for RuntimeError {
    fn from(error: rustel_jsruntime::QueryError) -> Self {
        match error {
            rustel_jsruntime::QueryError::Limit(limit) => limit.into(),
            rustel_jsruntime::QueryError::Message(message)
            | rustel_jsruntime::QueryError::Policy(message) => Self::Js(message),
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Js(e) => write!(f, "javascript: {e}"),
            Self::Mini(e) => write!(f, "mini: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::NoPattern => write!(f, "no active pattern; evaluate or set_pattern first"),
            Self::ResourceLimit(e) => write!(f, "{e}"),
            Self::Audio(e) => write!(f, "audio: {e}"),
            Self::Unsupported(e) => write!(f, "{e}"),
            Self::UntrustedImport { origin } => write!(
                f,
                "{origin} is not trusted for import(). Trust it with --allow-import-origin {origin}"
            ),
            Self::CsoundPermission => write!(
                f,
                "Csound is not permitted. Permit native orchestras with --allow-csound, only for trusted scores"
            ),
            Self::Interrupted(signal) => {
                write!(f, "interrupted by signal {signal} before the work started")
            }
            Self::Cancelled => write!(f, "cancelled"),
            Self::Panic(message) => write!(f, "{message}"),
            Self::Message(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl RuntimeError {
    /// A stable machine-readable kind.
    ///
    /// The CLI prints this alongside the message so a caller can branch on the
    /// FAILURE rather than on prose. Every one of these strings is part of the
    /// contract: change one and a consumer's error handling silently stops
    /// matching. The CLI suite in `crates/cli/tests/cli/main.rs` asserts the
    /// kind in the JSON error that the binary prints.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ResourceLimit(_) => "resource-limit",
            Self::Audio(_) => "audio",
            Self::Unsupported(_) => "unsupported",
            Self::UntrustedImport { .. } => "untrusted-import",
            Self::CsoundPermission => "csound-permission",
            Self::Interrupted(_) => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Panic(_) => "panic",
            Self::Js(_) => "evaluation",
            Self::Mini(_) => "mini",
            Self::Io(_) => "io",
            Self::NoPattern => "no-pattern",
            Self::Message(_) => "invalid-argument",
        }
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Preserve `play`'s partial-success Stop contract across both scheduler Stop
/// shapes. A signal already visible when an impure tick enters the bounded JS
/// preflight is returned as `RuntimeError::Cancelled`; one observed inside the
/// core query is `TickStatus::Refused(Cancelled)`. Both mean "stop after the
/// onsets already collected", not an evaluation failure.
fn play_tick_or_stopped(
    result: Result<rustel_scheduler::TickStatus, RuntimeError>,
) -> Result<Option<rustel_scheduler::TickStatus>, RuntimeError> {
    match result {
        Ok(rustel_scheduler::TickStatus::Stopped) | Err(RuntimeError::Cancelled) => Ok(None),
        Ok(status) => Ok(Some(status)),
        Err(error) => Err(error),
    }
}

/// The longest window `play`/`render` will schedule.
///
/// The library enforces this bound, not only the CLI argument parser: `play`
/// is public, and an embedder or a test that calls it directly does not pass
/// through the binary's validation.
const MAX_DURATION_SECS: f64 = 24.0 * 60.0 * 60.0;

/// The shortest horizon `SessionConfig` accepts: the scheduler's smallest
/// refill floor.
const MIN_HORIZON_SECS: f64 = rustel_scheduler::MIN_REFILL_FLOOR_SECS;

/// The most onsets a single `play`/`render` will accumulate.
///
/// One million is far past any musically meaningful window: a dense pattern
/// of 32 events per cycle reaches it after about 17 hours at the default
/// 0.5 cycles per second. The cap keeps the in-memory timeline and its JSON
/// bounded.
const MAX_ONSETS: usize = 1_000_000;

/// Whether a query failure is news: `true` the first time this
/// `(generation, message)` is seen, and again when either part changes.
///
/// `report_diagnostic`'s back-of-queue check suppresses only consecutive
/// repeats, and a live set interleaves other diagnostics between ticks. A
/// fatal throw and a contained callback failure (a throwing `filterValues`
/// predicate, which keeps the music playing) each keep their own record of
/// the last report.
fn query_failure_is_news(
    generation: u64,
    message: &str,
    reported: &mut Option<(u64, String)>,
) -> bool {
    if reported.as_ref() == Some(&(generation, message.to_string())) {
        return false;
    }
    *reported = Some((generation, message.to_string()));
    true
}

/// One-process headless runtime: one QuickJS heap, one scheduler transport.
pub struct Session {
    js: JsRuntime,
    panic_guard_active: bool,
    panic_poisoned: bool,
    panic_is_query: std::cell::Cell<bool>,
    recovery_epoch: u64,
    recovery_generation: u64,
    recovered_source: Option<Arc<str>>,
    recovery_prebakes: Arc<Vec<recovery::RecoveryPrebake>>,
    #[cfg(any(test, feature = "test-support"))]
    injected_panic: std::cell::Cell<Option<SessionPanicPoint>>,
    /// Serial intents from the last scheduling pass, awaiting the live loop.
    #[cfg(feature = "serial")]
    pending_serial: Vec<(f64, crate::serial_bridge::SerialOnset)>,
    /// MIDI intents collected during the last audio scheduling pass, waiting
    /// for the live loop to convert them to wall-clock sends.
    ///
    /// Collected here rather than re-queried because `schedule_through`
    /// ADVANCES the scheduler: asking a second time for the same window would
    /// not return the same onsets, it would return the next ones.
    #[cfg(feature = "midi")]
    pending_midi: Vec<crate::midi_bridge::MidiOnset>,
    /// The visuals recording of the score that just evaluated, before anything
    /// has committed it. A candidate the replacement probe rejects is dropped
    /// here rather than reaching a window.
    #[cfg(feature = "hydra")]
    staged_hydra: Option<Box<rustel_jsruntime::HydraCandidate>>,
    /// The visuals recording of the score that is now installed, waiting for
    /// the surface that owns the window to drain it.
    #[cfg(feature = "hydra")]
    pending_hydra: Option<Box<rustel_jsruntime::HydraCandidate>>,
    /// Per-score diagnostic gates for bounded MIDI collection. They prevent a
    /// dense pattern from turning one refused route into producer-thread log
    /// spam while still reporting the first actionable failure.
    #[cfg(feature = "midi")]
    midi_oversized_route_reported: bool,
    #[cfg(feature = "midi")]
    midi_output_limit_reported: bool,
    /// OSC intents collected during the last audio scheduling pass, waiting
    /// for the live loop to send them.
    ///
    /// Collected there rather than re-queried because `schedule_through`
    /// ADVANCES the scheduler: asking a second time for the same window would
    /// return the next onsets, not these.
    #[cfg(feature = "osc")]
    pending_osc: Vec<(f64, crate::osc_bridge::OscOnset)>,
    /// Refused OSC destinations already reported for the active score. A
    /// dense pattern must not allocate and print the same policy failure once
    /// per onset on the live producer thread.
    #[cfg(feature = "osc")]
    reported_osc_refusals: Vec<ReportedOscRefusal>,
    #[cfg(feature = "osc")]
    osc_output_limit_reported: bool,
    transport: Arc<Transport>,
    scheduler: Scheduler,
    config: SessionConfig,
    last_source: Option<Arc<str>>,
    /// The score of this session plays through the plugins of a live
    /// output: see [`Self::attach_plugins`].
    #[cfg(feature = "vst")]
    plugins_live: AtomicBool,
    /// The sample rate of that output, in Hz. Each plugin call of this
    /// session asks for a copy at this rate.
    #[cfg(feature = "vst")]
    plugin_rate: std::sync::atomic::AtomicU32,
    /// The plugin chains of the scores of this session. The names source
    /// of the live output reads the same chains.
    #[cfg(feature = "vst")]
    chains: Arc<std::sync::Mutex<crate::vst::Chains>>,
    /// The lanes of the scores of this session, each with the number its
    /// plugin notes carry.
    #[cfg(feature = "vst")]
    lanes: crate::vst::LaneTable,
    /// The last look of [`Self::plugins_pending`]: its time, the generation
    /// of the score, and the count.
    #[cfg(feature = "vst")]
    plugin_look: std::cell::Cell<Option<(Instant, u64, usize)>>,
    /// The edit that waits for its plugin copies, while the score in play
    /// plays on: see [`Self::reload_live`].
    #[cfg(all(feature = "vst", feature = "device-audio"))]
    plugin_hold: Option<plugin_wait::PluginHold>,
    /// An edit waits for its plugin copies: see [`Self::set_plugin_wait`].
    #[cfg(all(feature = "vst", feature = "device-audio"))]
    plugin_wait: bool,
    /// Files the most recent `preload(...)` asked for, until a caller waits.
    preload_requested: usize,
    /// Whether a setup this session ran picks sample variants: it writes an
    /// `n` or maps values through code. Its helpers can set `n` for any
    /// later score, so a score's text does not show every `n` it plays. See
    /// [`Self::prebake_selects_variants`].
    prebake_selects_variants: bool,
    /// When set, a bounce ends where the music does rather than where the
    /// requested duration does.
    stop_when_silent: Option<rustel_audio::SilenceStop>,
    /// Optional master limiter for WAV/MP3 file exports.
    export_limiter: Option<rustel_audio::RenderLimiter>,
    /// The input-channel warnings already said for this score and this
    /// input: a silent voice on `in:1` is news once, not once a window.
    input_warnings_reported: std::collections::BTreeSet<String>,
    /// The voice refusals already said for this score, by kind. One install
    /// converts the same onsets over and over - a live set asks its score
    /// every window, and a held replacement re-converts its first window on
    /// every retry - so without this the log fills with the same warning and
    /// a count that climbs while nothing changes. Every install clears it -
    /// a new score's refusal is news again. Awaited and skipped are kept
    /// apart, so notes skipped after a wait are still said.
    refusals_reported: std::collections::BTreeSet<(&'static str, String)>,
    /// The plugin lines already said for this score. A note gives its line
    /// again in each window. Every install clears the set.
    plugin_lines_reported: std::collections::BTreeSet<String>,
    /// Seconds rendered past the scheduled length, for the tail: the
    /// scheduler stops at the length, the reverbs and delays ring out into
    /// the tail, and a silence stop ends it when they have.
    render_tail_secs: f64,
    /// Rate limit for the "fell behind, resumed at the present" report: a
    /// machine that is persistently too slow would otherwise log every tick.
    /// Only the live producer can fall behind, so only that build reads it.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    last_gap_log: Option<Instant>,
    /// Last source selected by an exact consumer completion, not publication.
    #[cfg(feature = "device-audio")]
    audible_source: Option<AudibleSource>,
    #[cfg(feature = "device-audio")]
    audio_confirmations: Option<confirmation::RollbackConfirmations>,
    /// The latency-compensated audio events a live device can hold, so a
    /// takeover does not sound one of their onsets twice.
    #[cfg(feature = "device-audio")]
    compensated_onsets: handover::CompensatedOnsets,
    /// How far a live host has handed the OSC bundles out, so a takeover
    /// does not send an onset that is out again.
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    osc_handed_out: handover::HandedOut,
    /// The same for the serial writes.
    #[cfg(all(feature = "device-audio", feature = "serial"))]
    serial_handed_out: handover::HandedOut,
    /// The key presses that the live windows have placed, so the OSC and
    /// serial messages of a new press go out before the frontier too.
    #[cfg(all(feature = "device-audio", any(feature = "osc", feature = "serial")))]
    placed_presses: handover::PlacedPresses,
    /// Positive channel count from the host's currently open audio input.
    /// Unknown or disconnected inputs retain their documented silent behavior.
    audio_input_channels: Option<usize>,
    /// Device schedule lead for continuous live reloads (0 until a host
    /// reports its device's playback latency).
    schedule_lead: f64,
    /// Live-reload re-query margin (the device's consumption frontier).
    /// Falls back to `schedule_lead` when the host never measured one.
    continuity_margin: Option<f64>,
    /// The sample rate of the last live schedule, in Hz. The producer
    /// converts a takeover time to the takeover frame at this rate, and
    /// [`Self::takeover_frame_edge`] uses the same rate. `None` until a
    /// live producer has scheduled: no device applies a takeover frame.
    live_sample_rate: Option<u32>,
    /// The time (seconds, session clock) where the newest live reload's
    /// re-query cursor started; the producer converts it to the device
    /// takeover frame when it publishes the replacement generation.
    requery_takeover_time: Option<f64>,
    /// Whether the newest live reload asked the consumer to silence what
    /// sounds under it at its takeover frame. A from-zero reload (a rewind)
    /// does: the restarted loop must be heard alone, like a retriggered
    /// sample, rather than over the tail of what it replaced. An ordinary
    /// reload never does: its voices ring out by contract, and cutting them
    /// would click on every edit.
    requery_takeover_cut: TakeoverCut,
    /// Where the current rewind's cycle zero sits, on the session clock:
    /// `Some` only while a replacement whose takeover carries a cut is
    /// unpublished. The first window was queried from here, so a window
    /// held for a loading sample reopens from here, and the producer slides
    /// it (with the takeover and the cut) once the clock has passed it.
    ///
    /// It equals the takeover time for both cut shapes: the edit instant
    /// for an immediate rewind, the line for a quantised one. A slide moves
    /// both together. It is a separate field because the producer consumes
    /// the (takeover, cut) pair in the middle of a turn, before it knows
    /// whether the window holds, and the anchor must outlive that.
    requery_anchor_time: Option<f64>,
    /// The generation that a re-query of the playing score made. Its first
    /// window plays the score the device already plays, so a refused voice
    /// is skipped and logged as in a steady window. No earlier score exists
    /// to return to.
    requeried_generation: Option<u64>,
    /// The next live reload takes over at this time rather than one
    /// continuity margin ahead: a launch quantised to a cycle line.
    takeover_override: Option<f64>,
    /// The next live reload plays its score from the score's own cycle
    /// zero, instead of joining the cycle already running.
    next_from_zero: bool,
    last_path: EvaluateSource,
    /// The default sample banks. `None` until enabled;
    /// resolution then falls back to the bundled `bd` alone.
    samples: Option<std::sync::Arc<crate::samples::SampleLibrary>>,
    pub(crate) csound: crate::csound::State,
    /// The text of each imported script, by the specifier that names it. A
    /// script is fetched once and runs again in a rebuilt realm.
    import_sources: std::collections::BTreeMap<String, Arc<str>>,
    /// The error of each imported script that threw in this realm.
    import_errors: std::collections::BTreeMap<String, Arc<str>>,
    /// Ordinary CLI surfaces report asynchronous sample failures as JSON on
    /// stderr. Full-screen clients disable that direct writer and drain the
    /// same failures into their own diagnostic channel instead, otherwise a
    /// loader thread can scribble over the alternate screen.
    direct_diagnostic_logging: bool,
    /// Full-screen owners drain this instead of allowing writes behind their
    /// alternate screen. It is bounded independently of score complexity.
    pending_diagnostics: VecDeque<SessionDiagnostic>,
    bindings_ready: bool,
    voicings_ready: bool,
    /// Whether this Session has claimed its native module settings instead of
    /// continuing to inherit the ambient Rust compatibility state.
    core_settings_owned: bool,
    /// Whether an unclaimed Session already has settings paired with its
    /// active native graph. Claiming the JavaScript realm freezes this
    /// snapshot instead of inheriting unrelated ambient changes again.
    core_settings_initialized: bool,
    /// Scheduling iterations the last `play` performed.
    ///
    /// Private telemetry, not part of any report. Counting iterations
    /// distinguishes a stopped loop from one that continues spinning too
    /// cheaply for reliable timing assertions.
    last_play_iterations: u64,
    /// The last query throw already reported, as `(generation, message)`.
    ///
    /// A broken callback used to produce one warning per tick, forever; now a
    /// message is news once per generation, and again when it changes.
    reported_query_throw: Option<(u64, String)>,
    /// The last contained callback failure already reported, as
    /// `(generation, message)`. A throwing `filterValues` predicate keeps the
    /// music playing (fail open); this is what keeps the warning from
    /// repeating per press.
    reported_callback_failure: Option<(u64, String)>,
    /// Phase observations waiting for the live producer to publish one
    /// complete bounded turn. Evaluation may precede the scheduling call, so
    /// the Session retains only this single scalar record between boundaries.
    #[cfg(any(feature = "device-audio", test))]
    pending_producer_turn: ProducerTurnRecord,
}

/// Classify what an offline bounce returned.
///
/// A stopped transport is the artist pressing Ctrl-C, not a failure: it has to
/// arrive as `Cancelled` so the CLI exits quietly with the partial file rather
/// than printing an I/O error for something that worked as asked.
fn render_error(error: std::io::Error) -> RuntimeError {
    if error.to_string().contains(rustel_audio::RENDER_CANCELLED) {
        return RuntimeError::Cancelled;
    }
    match error.kind() {
        std::io::ErrorKind::InvalidInput => RuntimeError::Message(format!("scalar audio: {error}")),
        _ => RuntimeError::Io(error),
    }
}

/// Name a top-level audio control whose numeric expression produced NaN or
/// infinity before JSON's required null conversion can erase the distinction.
fn non_finite_audio_control(value: &rustel_core::Value) -> Option<String> {
    let materialized;
    let value = if matches!(value, rustel_core::Value::JsValue(_)) {
        materialized = rustel_core::materialize_js_value(value);
        &materialized
    } else {
        value
    };
    value.as_object()?.iter().find_map(|(name, value)| {
        // Mini-notation rests carry `n: NaN` as an internal no-note
        // sentinel. It is not a malformed user control and the scheduler
        // deliberately drops that voice.
        if name == "n" {
            return None;
        }
        value
            .as_f64()
            .filter(|number| !number.is_finite())
            .map(|_| format!("{name} must be a finite number"))
    })
}

impl Session {
    pub fn new() -> Result<Self, RuntimeError> {
        Self::with_config(SessionConfig::default())
    }

    /// # Validation
    ///
    /// Every numeric field is checked here, because each one feeds a loop
    /// bound, a divisor or an allocation. `play`'s loop runs until
    /// `start_secs + duration_secs + horizon`, so an infinite horizon would
    /// keep an ordinary two-second render from terminating. The bound is a
    /// sum, so validating `duration` alone is not enough.
    pub fn with_config(config: SessionConfig) -> Result<Self, RuntimeError> {
        // The first `gamepad()` any score calls starts the pad poller; a
        // session is where the runtime and the score meet, so this is
        // where the runtime says how.
        #[cfg(feature = "gamepad")]
        rustel_core::gamepad::set_starter(crate::gamepad::ensure_polling);
        let finite = |what: &str, value: f64| -> Result<f64, RuntimeError> {
            if !value.is_finite() || value <= 0.0 {
                return Err(RuntimeError::Message(format!(
                    "{what} must be a finite number greater than zero, got {value}"
                )));
            }
            Ok(value)
        };
        if !(1..=rustel_audio::MAX_CONFIGURABLE_POLYPHONY).contains(&config.max_polyphony) {
            return Err(RuntimeError::Message(format!(
                "max_polyphony must be from 1 to {}, got {}",
                rustel_audio::MAX_CONFIGURABLE_POLYPHONY,
                config.max_polyphony
            )));
        }
        finite("cps", config.cps)?;
        let horizon = finite("horizon", config.horizon)?;
        if horizon < MIN_HORIZON_SECS {
            return Err(RuntimeError::Message(format!(
                "horizon must be at least {MIN_HORIZON_SECS} seconds, got {horizon}"
            )));
        }
        if horizon > MAX_DURATION_SECS {
            return Err(RuntimeError::ResourceLimit(format!(
                "horizon must be at most {MAX_DURATION_SECS} seconds, got {horizon}"
            )));
        }
        if config.sample_rate == 0 {
            return Err(RuntimeError::Message(
                "sample_rate must be greater than zero".into(),
            ));
        }
        if config.channels == 0 {
            return Err(RuntimeError::Message(
                "channels must be greater than zero".into(),
            ));
        }
        let js = config
            .pointer
            .clone()
            .map_or_else(JsRuntime::new, JsRuntime::with_pointer)
            .map_err(|e| RuntimeError::Js(e.to_string()))?;
        let transport = Arc::new(Transport::default());
        let scheduler = Scheduler::new(transport.clone(), config.cps, config.horizon);
        let csound = crate::csound::State::new(config.score_import_access.csound_allowed());
        Ok(Self {
            js,
            panic_guard_active: false,
            panic_poisoned: false,
            panic_is_query: std::cell::Cell::new(false),
            recovery_epoch: 0,
            recovery_generation: 0,
            recovered_source: None,
            recovery_prebakes: Arc::default(),
            #[cfg(any(test, feature = "test-support"))]
            injected_panic: std::cell::Cell::new(None),
            #[cfg(feature = "serial")]
            pending_serial: Vec::new(),
            #[cfg(feature = "midi")]
            pending_midi: Vec::new(),
            #[cfg(feature = "hydra")]
            staged_hydra: None,
            #[cfg(feature = "hydra")]
            pending_hydra: None,
            #[cfg(feature = "midi")]
            midi_oversized_route_reported: false,
            #[cfg(feature = "midi")]
            midi_output_limit_reported: false,
            #[cfg(feature = "osc")]
            pending_osc: Vec::new(),
            #[cfg(feature = "osc")]
            reported_osc_refusals: Vec::new(),
            #[cfg(feature = "osc")]
            osc_output_limit_reported: false,
            transport,
            scheduler,
            config,
            last_source: None,
            #[cfg(feature = "vst")]
            plugins_live: AtomicBool::new(false),
            #[cfg(feature = "vst")]
            plugin_rate: std::sync::atomic::AtomicU32::new(0),
            #[cfg(feature = "vst")]
            chains: Arc::new(std::sync::Mutex::new(crate::vst::Chains::new())),
            #[cfg(feature = "vst")]
            lanes: crate::vst::LaneTable::default(),
            #[cfg(feature = "vst")]
            plugin_look: std::cell::Cell::new(None),
            #[cfg(all(feature = "vst", feature = "device-audio"))]
            plugin_hold: None,
            #[cfg(all(feature = "vst", feature = "device-audio"))]
            plugin_wait: true,
            preload_requested: 0,
            prebake_selects_variants: false,
            stop_when_silent: None,
            export_limiter: None,
            input_warnings_reported: std::collections::BTreeSet::new(),
            refusals_reported: std::collections::BTreeSet::new(),
            plugin_lines_reported: std::collections::BTreeSet::new(),
            render_tail_secs: 0.0,
            last_gap_log: None,
            #[cfg(feature = "device-audio")]
            audible_source: None,
            #[cfg(feature = "device-audio")]
            audio_confirmations: None,
            #[cfg(feature = "device-audio")]
            compensated_onsets: handover::CompensatedOnsets::default(),
            #[cfg(all(feature = "device-audio", feature = "osc"))]
            osc_handed_out: handover::HandedOut::default(),
            #[cfg(all(feature = "device-audio", feature = "serial"))]
            serial_handed_out: handover::HandedOut::default(),
            #[cfg(all(feature = "device-audio", any(feature = "osc", feature = "serial")))]
            placed_presses: handover::PlacedPresses::default(),
            audio_input_channels: None,
            schedule_lead: 0.0,
            continuity_margin: None,
            live_sample_rate: None,
            requery_takeover_time: None,
            requery_takeover_cut: TakeoverCut::None,
            requery_anchor_time: None,
            requeried_generation: None,
            takeover_override: None,
            next_from_zero: false,
            last_path: EvaluateSource::Pattern,
            last_play_iterations: 0,
            samples: None,
            csound,
            import_sources: std::collections::BTreeMap::new(),
            import_errors: std::collections::BTreeMap::new(),
            direct_diagnostic_logging: rustel_voice::default_direct_diagnostic_logging(),
            pending_diagnostics: VecDeque::new(),
            bindings_ready: false,
            voicings_ready: false,
            core_settings_owned: false,
            core_settings_initialized: false,
            reported_query_throw: None,
            reported_callback_failure: None,
            #[cfg(any(feature = "device-audio", test))]
            pending_producer_turn: ProducerTurnRecord::default(),
        })
    }

    /// What the score that just evaluated asked a visuals window to draw,
    /// waiting for something to commit. Cleared into `pending_hydra` there.
    #[cfg(feature = "hydra")]
    pub(crate) fn promote_staged_hydra(&mut self) {
        if let Some(candidate) = self.staged_hydra.take() {
            self.pending_hydra = Some(candidate);
        }
    }

    /// The visuals program a committed score installed, if there is a new one.
    ///
    /// Drained by whichever product surface owns the window, beside
    /// [`Session::take_pending_midi`] and [`Session::take_pending_osc`]. An
    /// update whose program is empty is the instruction to close.
    #[cfg(feature = "hydra")]
    pub fn take_pending_hydra(&mut self) -> Option<Box<rustel_jsruntime::HydraCandidate>> {
        self.pending_hydra.take()
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Effective voice budget. Like other module settings, an explicit score
    /// value persists until another accepted score changes it.
    pub fn max_polyphony(&self) -> usize {
        self.max_polyphony_override()
            .unwrap_or(self.config.max_polyphony)
    }

    /// Explicit accepted score setting, independent of the host default.
    pub fn max_polyphony_override(&self) -> Option<usize> {
        self.js
            .with_runtime_settings(rustel_core::settings::max_polyphony)
    }

    /// Change the host default without overwriting an explicit score override.
    pub fn set_default_max_polyphony(&mut self, voices: usize) {
        self.config.max_polyphony = voices.clamp(1, rustel_audio::MAX_CONFIGURABLE_POLYPHONY);
    }

    /// Replace the origins that text may import scripts from. The next
    /// evaluation reads the new list.
    pub fn set_score_import_access(&mut self, access: ScoreImportAccess) {
        self.csound.set_allowed(access.csound_allowed());
        self.config.score_import_access = access;
    }

    pub fn csound_snapshot(&self) -> crate::CsoundSnapshot {
        self.csound.snapshot()
    }

    pub fn restore_csound_snapshot(&mut self, snapshot: &crate::CsoundSnapshot) {
        self.csound.restore(snapshot)
    }

    pub fn last_evaluate_source(&self) -> EvaluateSource {
        self.last_path.clone()
    }

    /// Exact source text that produced the active generation, when it came
    /// from a score evaluation rather than a directly installed Rust pattern.
    pub fn active_source(&self) -> Option<&str> {
        self.last_source.as_deref()
    }

    /// Gives a live output the plugins of the process. From this call, the
    /// score of this session is the score of that output: an edit keeps the
    /// plugin copies of each chain it leaves as it was, and ends the copies
    /// of a chain that left. The plugins run at the sample rate of the
    /// output. The call is no score change: each commit tells the chains of
    /// this session its score. A session with no such output prepares no
    /// plugin copy, and its score changes do not reach the plugin host.
    /// Each session has chains of its own, and each render to a file has
    /// too.
    #[cfg(all(feature = "vst", feature = "device-audio"))]
    pub fn attach_plugins(&self, device: &rustel_audio::LiveScalarDevice) {
        self.attach_instruments(device);
    }

    /// Connects this session's instruments to a live output.
    #[cfg(feature = "device-audio")]
    pub fn attach_instruments(&self, device: &rustel_audio::LiveScalarDevice) {
        #[cfg(feature = "vst")]
        {
            self.plugin_rate
                .store(device.sample_rate(), std::sync::atomic::Ordering::Relaxed);
            self.plugins_live
                .store(true, std::sync::atomic::Ordering::Relaxed);
            crate::vst::attach(device, &self.chains);
        }
        device.set_insert_provider(self.csound.provider(false));
        let csound = self.csound.clone();
        #[cfg(feature = "vst")]
        let chains = self.chains.clone();
        device.set_insert_names(Arc::new(move || {
            let mut named = csound.named_chains();
            #[cfg(feature = "vst")]
            named.extend(crate::vst::named_chains(&chains));
            named.sort_unstable();
            named.dedup();
            named
        }));
    }

    /// Gives the plugins the sample rate of the live output after a reopen
    /// of the output. A reopen is no new attach: see [`Self::attach_plugins`].
    #[cfg(feature = "vst")]
    pub fn set_plugin_rate(&self, sample_rate: u32) {
        self.plugin_rate
            .store(sample_rate, std::sync::atomic::Ordering::Relaxed);
    }

    /// True when the score of this session plays through the plugins of a
    /// live output: see [`Self::attach_plugins`].
    #[cfg(feature = "vst")]
    pub fn plugins_live(&self) -> bool {
        self.plugins_live.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Converts a note of this session for a live output: see
    /// [`crate::render::live_audio_event`]. The note takes the number of
    /// its plugin chain from the chains of this session.
    fn live_audio_event(
        &self,
        onset: &OnsetEventJson,
        sample_rate: u32,
        cps: f64,
        samples: &dyn rustel_voice::SampleLookup,
    ) -> Result<rustel_audio::AudioEvent, rustel_voice::VoiceError> {
        let lookup = crate::csound::Lookup {
            samples,
            state: &self.csound,
        };
        let event = crate::render::live_audio_event(onset, sample_rate, cps, &lookup, |value| {
            #[cfg(feature = "vst")]
            if let ValueJson::Raw(value) = value {
                return crate::vst::live_chain(&self.chains, value);
            }
            #[cfg(not(feature = "vst"))]
            let _ = value;
            None
        })?;
        self.csound.record_chain(&event.controls);
        Ok(event)
    }

    /// Plugin calls named in a candidate source. Evaluated haps supply the
    /// chain identity used to prepare each plugin copy.
    #[cfg(feature = "vst")]
    pub fn plugin_calls_for_source(&self, source: &str) -> Vec<crate::lint::NamedPlugin> {
        crate::lint::live_plugins(source)
    }

    pub fn generation(&self) -> u64 {
        self.transport.generation()
    }
}

/// The kinds of diagnostic this thread's sessions wrote straight to stderr,
/// for tests of the direct log (the queue has its own run dedupe, so
/// `take_diagnostics` cannot see a repeated record there).
#[cfg(test)]
mod direct_diagnostics_for_test {
    use std::cell::RefCell;

    thread_local! {
        static KINDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(kind: String) {
        KINDS.with(|kinds| kinds.borrow_mut().push(kind));
    }

    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(super) fn take() -> Vec<String> {
        KINDS.with(|kinds| std::mem::take(&mut *kinds.borrow_mut()))
    }
}

#[cfg(test)]
mod test_support {
    //! Fixtures shared by the session's test modules.

    use super::*;

    /// A session evaluating `source`, with its diagnostics kept for the test
    /// rather than printed.
    pub(super) fn quiet_session(source: &str) -> Session {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.evaluate(source).expect("evaluate");
        session
    }

    /// The "query-threw" diagnostics reported since the last take.
    pub(super) fn warnings(session: &mut Session) -> Vec<String> {
        session
            .take_diagnostics()
            .into_iter()
            .filter(|diagnostic| diagnostic.kind == "query-threw")
            .map(|diagnostic| diagnostic.message)
            .collect()
    }

    /// A session whose scores may fetch samples from `origin`.
    pub(super) fn session_with_sample_origin(origin: &str) -> Session {
        let mut config = SessionConfig::default();
        config
            .score_sample_access
            .permit_origin(origin)
            .expect("test sample origin");
        Session::with_config(config).expect("session")
    }
}

#[cfg(test)]
mod tests {
    /// The first plugin note of the first cycle.
    #[cfg(feature = "vst")]
    pub(super) fn plugin_note(session: &Session) -> serde_json::Value {
        let report = session
            .query_report(Fraction::ZERO, Fraction::ONE)
            .expect("query");
        let values = report.haps.iter();
        let mut values = values.map(|hap| serde_json::to_value(&hap.value).expect("value"));
        let note = values.find(|value| value.get("vst").or(value.get("vsti")).is_some());
        note.expect("plugin note")
    }
    #[cfg(feature = "vst")]
    pub(super) fn plugin_chain(session: &Session) -> u64 {
        crate::vst::live_chain(&session.chains, &plugin_note(session)).expect("plugin chain")
    }
    /// The fixture plugin in a new folder, as the only plugin folder of the
    /// process: no test reads the plugins or the presets of the machine. A
    /// new folder is a new plugin for the plugin host, so its first copy is
    /// not ready at once. One test at a time has the folder.
    #[cfg(all(feature = "vst", feature = "device-audio"))]
    pub(super) fn fixture_plugins() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        pub(super) static FOLDER: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let one = FOLDER.lock().unwrap_or_else(|poison| poison.into_inner());
        let folder = tempfile::tempdir().expect("plugin folder");
        rustel_vst3_fixture::install(folder.path());
        crate::vst::pin_standard_folders(vec![folder.path().to_path_buf()]);
        crate::vst::host().set_preset_folder(folder.path().join("presets"));
        (one, folder)
    }
    /// What sounds across a save at clock 11.9 with a 0.25 s continuity
    /// margin, as `(time, begin)`: the outgoing copies the device keeps
    /// (aimed before the takeover at 12.15) and all that the replacement
    /// emits. The outgoing score runs at one cycle a second, so cycle N is
    /// time N. With a `live_rate`, the save reads its takeover on a frame
    /// at that sample rate, as a live device does.
    pub(super) fn sounding_across_a_save(
        old: &str,
        new: &str,
        live_rate: Option<u32>,
    ) -> Vec<(f64, f64)> {
        let begin_of = |label: &str| -> f64 {
            let mut parts = label.splitn(2, '/');
            let n: f64 = parts.next().expect("numerator").parse().expect("numerator");
            let d: f64 = parts
                .next()
                .map_or(1.0, |d| d.parse().expect("denominator"));
            n / d
        };
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate(old).expect("initial score");
        session.restart_transport_at(0.0);
        session.set_schedule_lead(0.25);
        session.set_continuity_margin(0.25);
        session.live_sample_rate = live_rate;
        let cover = session.config.horizon + 0.25;
        let mut sounding = Vec::new();
        for _ in 0..10 {
            if session.scheduled_to_cycle() > 12.6 {
                break;
            }
            for event in session
                .schedule_through(11.9, 11.9 + cover)
                .expect("producer prefetch past the takeover")
            {
                if (11.9..12.15).contains(&event.target_time) {
                    sounding.push((event.target_time, begin_of(&event.whole_begin)));
                }
            }
        }
        assert!(
            session.scheduled_to_cycle() > 12.6,
            "test premise: prefetched past the takeover, got {}",
            session.scheduled_to_cycle()
        );
        session.reload_at(new, false, 11.9).expect("the save");
        for step in 0..5 {
            let now = 11.9 + 0.1 * f64::from(step);
            for event in session
                .schedule_through(now, now + cover)
                .expect("replacement window")
            {
                sounding.push((event.target_time, begin_of(&event.whole_begin)));
            }
        }
        sounding.sort_by(|a, b| a.partial_cmp(b).expect("finite times"));
        sounding
    }
    pub(super) fn queried_parts(session: &Session) -> Vec<(Fraction, Fraction)> {
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query score")
            .into_iter()
            .map(|hap| (hap.part.begin, hap.part.end))
            .collect()
    }
    pub(super) fn shown_note(session: &Session) -> String {
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query last-good");
        haps.iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>()
            .join(" ")
    }
    use super::*;
    use rustel_core::Value;
    pub(super) fn active_values(session: &Session) -> Vec<String> {
        session
            .js
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("active graph remains queryable")
            .into_iter()
            .map(|hap| hap.value.show())
            .collect()
    }
    /// Long enough for Windows to finish refusing a closed loopback port.
    /// `wait_until_idle` returns immediately when work settles, so this is a
    /// deadline rather than an unconditional delay.
    pub(super) const LOOPBACK_REFUSAL_DEADLINE: Duration = Duration::from_secs(15);
    pub(super) const QUERY_SPIN_SOURCE: &str = r#"
      globalThis.__querySpin = true;
      new Pattern(state => {
        globalThis.__queryCalls = (globalThis.__queryCalls || 0) + 1;
        if (globalThis.__querySpin) {
          while (true) {}
        }
        return pure('recovered').query(state);
      })
    "#;
    pub(super) fn set_query_spin(session: &Session, spin: bool) {
        session
            .js
            .eval(if spin {
                "globalThis.__querySpin = true"
            } else {
                "globalThis.__querySpin = false"
            })
            .expect("set query spin flag");
    }
    #[cfg(feature = "midi")]
    pub(super) fn midi_collection_onset(onset_id: u64, value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 3],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }
    /// A bind whose callback builds a nested callback graph on every
    /// invocation, and collects while doing so. Each nested `polyBind` adds a
    /// cell to the live `BridgeFrame`, a GC root, so the frame's lifetime
    /// sets how long those cells survive.
    pub(super) const NESTED_CALLBACK_SOURCE: &str = r#"pure('bd').polyBind(x => {
        __gc();
        globalThis.peakCells = Math.max(globalThis.peakCells || 0, __cellsLive());
        return pure(x).polyBind(y => pure(y).fast(2));
    })"#;
    pub(super) fn nested_callback_session() -> Session {
        let mut session = Session::new().expect("session");
        session.ensure_bindings().expect("bindings");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate(NESTED_CALLBACK_SOURCE)
            .expect("evaluate nested callback source");
        assert!(
            session.active_needs_host(),
            "the source must be impure, or this proves nothing about the host \
             scope"
        );
        session
    }
    /// The PEAK live-cell count observed from inside the callback while
    /// playback was running, plus how many cells the run created and how many
    /// onsets it scheduled.
    ///
    /// The peak is sampled by the callback while playback is active. After
    /// `play` returns the frame has already been dropped, hiding any growth
    /// caused by an overly broad frame lifetime.
    pub(super) fn peak_cells_during_play(duration_secs: f64) -> (u64, u64, usize) {
        let mut session = nested_callback_session();
        let created_before = rustel_jsruntime::cells_created();
        let report = session.play(duration_secs).expect("play");
        let peak = session
            .js
            .get_number("peakCells")
            .expect("the callback must have run and recorded a peak") as u64;
        let created = rustel_jsruntime::cells_created() - created_before;
        (created, peak, report.onsets.len())
    }
    #[test]
    fn every_session_config_field_is_validated() {
        // `play`'s loop bound is a sum that includes `horizon`, so an
        // infinite horizon would keep a two-second render from terminating.
        let bad = [
            (
                "horizon",
                SessionConfig {
                    horizon: f64::INFINITY,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: f64::NAN,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: 0.0,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: -1.0,
                    ..Default::default()
                },
            ),
            // Shorter than the scheduler's smallest refill floor.
            (
                "horizon",
                SessionConfig {
                    horizon: 0.0005,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: 1e12,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: 0.0,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: f64::INFINITY,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: f64::NAN,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: -1.0,
                    ..Default::default()
                },
            ),
            (
                "sample_rate",
                SessionConfig {
                    sample_rate: 0,
                    ..Default::default()
                },
            ),
            (
                "channels",
                SessionConfig {
                    channels: 0,
                    ..Default::default()
                },
            ),
        ];
        for (field, config) in bad {
            assert!(
                Session::with_config(config).is_err(),
                "an invalid {field} was accepted; every config field feeds a \
                 loop bound, a divisor or an allocation"
            );
        }
        // ...and the default is of course still valid.
        assert!(Session::with_config(SessionConfig::default()).is_ok());
    }
    #[cfg(feature = "device-audio")]
    pub(super) fn loading_prefill_fixture() -> (
        Session,
        crate::LiveFileProducer,
        Arc<crate::samples::SampleLibrary>,
    ) {
        let library = Arc::new(crate::samples::SampleLibrary::with_loading_sample_for_test(
            "heldsample",
        ));
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            ..Default::default()
        })
        .expect("session");
        session.samples = Some(Arc::clone(&library));
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        session.evaluate_mini("~").expect("initial silent source");
        session.restart_transport_at(0.0);
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let initial = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill must not publish a replacement"),
                |_| panic!("initial rest emitted audio"),
            )
            .expect("initial silent prefill");
        assert_eq!(
            (initial.scheduled, initial.pushed, initial.pending),
            (0, 0, 0)
        );
        assert!(
            session.audible_source.is_none(),
            "no output receipt is bound"
        );
        let _ = producer.producer_load_snapshot();
        (session, producer, library)
    }
    /// The loading fixture with a silent audible score published, ready for
    /// a rewind to replace it: `(session, producer, library)`.
    #[cfg(feature = "device-audio")]
    pub(super) fn rewind_hold_fixture() -> (
        Session,
        crate::LiveFileProducer,
        Arc<crate::samples::SampleLibrary>,
    ) {
        let (mut session, mut producer, library) = loading_prefill_fixture();
        let before = session.generation();
        let old = rustel_core::controls::ControlSpec::new(["s"])
            .pattern(&rustel_mini::mini("~").expect("silent old score"));
        session.set_pattern(old).expect("old silent score");
        session.restart_transport_at(0.0);
        let mid = session.generation();
        producer.arm_replacement(before, mid);
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| {},
                |_| panic!("silence emitted audio"),
            )
            .expect("silent old score publishes");
        session
            .mark_audible_generation(mid)
            .expect("injected replay-policy target");
        (session, producer, library)
    }
    /// A parked rewind held on a loading sample, ready for a test to take
    /// turns against: the replacement is armed and its first turn has
    /// queried, refused for loading, and parked.
    #[cfg(feature = "device-audio")]
    pub(super) fn parked_rewind_fixture() -> (
        Session,
        crate::LiveFileProducer,
        Arc<crate::samples::SampleLibrary>,
    ) {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample*4\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a held rewind published"),
                |_| panic!("a held rewind emitted audio"),
            )
            .expect("the rewind holds quietly");
        assert!(producer.cut_takeover_deferred_for_loading());
        assert_eq!(
            producer.producer_load_snapshot().atomic_refusals - refusals_before,
            1,
            "the first turn queried and held"
        );
        (session, producer, library)
    }
    #[cfg(feature = "device-audio")]
    pub(super) fn check_loading_prefill_queued_progress(external: bool) {
        let (mut session, mut producer, library) = loading_prefill_fixture();
        let before = session.generation();
        let pattern = rustel_core::fastcat(
            (0..4)
                .map(|index| {
                    let mut controls = vec![("s".into(), Value::Str("heldsample".into()))];
                    if external && index == 3 {
                        controls.push(("note".into(), Value::F64(60.0)));
                        controls.push(("midiport".into(), Value::Str("test-port".into())));
                    }
                    rustel_core::pure(Value::object(controls))
                })
                .collect(),
        );
        session.set_pattern(pattern).expect("native queued pattern");
        session.restart_transport_at(0.0);
        let after = session.generation();
        producer.arm_replacement(before, after);
        session.scheduler.set_horizon(1.0);
        assert_eq!(
            session.scheduler.tick(&VirtualClock::new(0.0)),
            rustel_scheduler::TickStatus::Filled
        );
        session.scheduler.set_horizon(0.5);
        assert_eq!(session.scheduler.queued(), 4);
        let first = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || 0.0,
            48_000,
            |_, _, _| panic!("loading first transfer must not publish"),
            |_| panic!("loading first transfer emitted audio"),
        );
        let first = first.expect("loading hold is a quiet turn");
        assert_eq!((first.scheduled, first.pushed, first.pending), (0, 0, 0));
        assert!(
            !producer.cut_takeover_deferred_for_loading(),
            "an edit's loading hold has no line cut to withdraw"
        );
        assert_eq!(session.scheduler.queued(), 1);
        assert!(!session.has_pending_external_output());
        let first_record = producer.producer_load_snapshot();
        assert_eq!(first_record.query_span_millicycles, 0);
        assert_eq!(first_record.scheduler_events, 0);
        assert_eq!(first_record.refused_voices, 3);
        if !external {
            library.finish_loading_sample_for_test();
            assert_eq!(library.take_ready().len(), 1);
        }
        let mut published = Vec::new();
        let mut audio = Vec::new();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.3,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("queued output completes prefill without another query");
        assert!(!producer.cut_takeover_deferred_for_loading());
        let record = producer.producer_load_snapshot();
        assert_eq!(record.query_span_millicycles, 0);
        assert_eq!(record.scheduler_events, 0);
        let expected = usize::from(!external);
        assert_eq!(record.converted_audio_events, expected as u64);
        assert_eq!(record.refused_voices, u64::from(external));
        assert_eq!(
            (step.scheduled, step.pushed, step.pending),
            (expected, expected, 0)
        );
        assert_eq!(audio.len(), expected);
        assert!(audio.iter().all(|event| event.target_frame == 36_000));
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, after);
        assert_eq!(session.scheduler.queued(), 0);
        #[cfg(feature = "midi")]
        if external {
            let intents = session.take_pending_midi();
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].target_time, 0.75);
            assert_eq!(intents[0].generation, after);
            assert!(!rustel_midi::plan(&intents[0].controls, intents[0].duration_secs).is_empty());
        }
    }
    #[cfg(feature = "device-audio")]
    pub(super) fn check_unconfirmed_prefill_rollback(
        source_a: &str,
        source_b: &str,
        mini: bool,
        delayed_b: bool,
    ) {
        use rustel_audio::device::ManualLiveOutput;

        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: 48_000,
            ..SessionConfig::default()
        })
        .expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        if mini {
            session.evaluate_mini(source_a).expect("source A");
        } else {
            session.evaluate(source_a).expect("source A");
        }
        let mut output = ManualLiveOutput::new(48_000, session.generation()).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output confirmations");
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let pump = |session: &mut Session,
                    producer: &mut crate::LiveFileProducer,
                    output: &ManualLiveOutput| {
            let device = output.device();
            producer.step_unwatched_with_clock_and_cutover(
                session,
                || device.clock_seconds(),
                48_000,
                |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                |event| device.push(event),
            )
        };

        // A reaches the same DSP and host-copy body used by live devices.
        // No marker or manually advanced frame counter stands in for it.
        let mut pcm = [0.0_f32; 256];
        let mut nonzero = false;
        for _ in 0..512 {
            pump(&mut session, &mut producer, &output).expect("A continuation");
            output.render(&mut pcm);
            assert!(pcm.iter().all(|sample| sample.is_finite()));
            nonzero |= pcm.iter().any(|sample| *sample != 0.0);
        }
        let copied_a = output.device().report();
        assert!(copied_a.submitted_frames > 0 && copied_a.callbacks > 0);
        assert_eq!(nonzero, !mini, "audible and valid-silence fixture controls");
        assert_eq!(copied_a.accepted_events > 0, !mini);

        let transport = session.transport();
        let now = output.device().clock_seconds();
        producer
            .shield_reload_with_clock(
                &mut session,
                || now,
                48_000,
                |event| output.device().push(event),
            )
            .expect("A shield");
        let generation_a = session.generation();
        let generation_b = session
            .reload_with_clock_cancellable(source_b, mini, transport.stopped_flag(), || now)
            .expect("B evaluates");
        producer.arm_replacement(generation_a, generation_b);
        let prefill = pump(&mut session, &mut producer, &output).expect("B prefill");
        assert_eq!(output.device().generation(), generation_b);
        assert_eq!(prefill.pushed > 0, !mini);
        assert_eq!(
            output.device().report().submitted_frames,
            copied_a.submitted_frames
        );

        // The host is deliberately not invoked for B. C is valid native mini
        // syntax, but its bare string haps cannot become scalar voice objects.
        let generation_c = session
            .reload_with_clock_cancellable("c4*64", true, transport.stopped_flag(), || now)
            .expect("C installs before conversion");
        assert!(generation_c > generation_b);
        producer.arm_replacement(generation_b, generation_c);
        let output = std::cell::RefCell::new(output);
        let mut copied_b = false;
        let refusal = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || {
                    // This clock is sampled AFTER turn-entry receipt draining,
                    // with Session already at C. B completes while that failing
                    // turn is in progress; rollback itself must drain the result.
                    if delayed_b && !copied_b {
                        for _ in 0..512 {
                            output.borrow_mut().render(&mut pcm);
                            assert!(pcm.iter().all(|sample| sample.is_finite()));
                        }
                        copied_b = true;
                    }
                    output.borrow().device().clock_seconds()
                },
                48_000,
                |generation, takeover, cut| {
                    output
                        .borrow()
                        .device()
                        .set_generation(generation, takeover, cut)
                },
                |event| output.borrow().device().push(event),
            )
            .expect_err("C refuses its nonempty first conversion window")
            .to_string();
        assert!(
            refusal.contains("scalar audio refused every onset"),
            "{refusal}"
        );
        assert!(
            refusal.contains("expected hap.value to be an object"),
            "{refusal}"
        );
        assert_eq!(
            output.borrow().device().report().submitted_frames > copied_a.submitted_frames,
            delayed_b
        );
        assert_eq!(
            session.active_source(),
            Some(if delayed_b { source_b } else { source_a }),
            "only a copied window may select its retained rollback source"
        );
    }
    #[test]
    fn max_polyphony_is_transactional_and_isolated_per_session() {
        assert_eq!(
            rustel_core::settings::MAX_CONFIGURABLE_POLYPHONY,
            rustel_audio::MAX_CONFIGURABLE_POLYPHONY
        );
        let mut first = Session::with_config(SessionConfig {
            max_polyphony: 192,
            ..Default::default()
        })
        .expect("session");
        let second = Session::new().expect("independent session");
        assert_eq!(first.max_polyphony(), 192);
        assert_eq!(first.max_polyphony_override(), None);
        assert_eq!(second.max_polyphony(), 128);
        first
            .evaluate("setMaxPolyphony(256); s('sine')")
            .expect("valid override");
        assert_eq!(first.max_polyphony(), 256);
        first.set_default_max_polyphony(64);
        assert_eq!(
            first.max_polyphony(),
            256,
            "host defaults do not replace score settings"
        );
        first.evaluate("s('sine')").expect("next score");
        assert_eq!(
            first.max_polyphony(),
            256,
            "omitting a module setter preserves it"
        );
        assert!(
            first
                .evaluate("setMaxPolyphony(4); throw new Error('reject')")
                .is_err()
        );
        assert_eq!(
            first.max_polyphony(),
            256,
            "failed evaluation is transactional"
        );
        assert_eq!(second.max_polyphony(), 128);
        first
            .evaluate("setMaxPolyphony(4); s('sine')")
            .expect("accepted replacement");
        assert_eq!(first.max_polyphony(), 4);
        assert_eq!(first.max_polyphony_override(), Some(4));
    }
    #[test]
    fn max_polyphony_rejects_invalid_score_values_without_changing_last_good() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setMaxPolyphony(192); s('sine')")
            .expect("score");
        let generation = session.generation();
        for bad in [
            "", "0", "-1", "1.5", "257", "512", "NaN", "Infinity", "'64'", "null", "true",
        ] {
            let source = format!("setMaxPolyphony({bad}); s('sine')");
            let error = session.evaluate(&source).expect_err(&source);
            assert!(error.to_string().contains("setMaxPolyphony"), "{error}");
            assert_eq!(session.max_polyphony(), 192, "{source}");
            assert_eq!(session.generation(), generation, "{source}");
        }
        for voices in [0, 257, usize::MAX] {
            assert!(
                Session::with_config(SessionConfig {
                    max_polyphony: voices,
                    ..Default::default()
                })
                .is_err()
            );
        }
    }
    pub(super) fn default_voicing_value() -> Value {
        let mut controls = rustel_core::OrderedMap::new();
        controls.insert("chord".into(), Value::Str("C7".into()));
        Value::List(
            rustel_core::voicings::render_voicing(&controls).expect("configured C7 voicing"),
        )
    }
    pub(super) fn default_voicing_size_pattern() -> Pattern {
        rustel_core::state_signal(|_| default_voicing_value())
    }
    pub(super) fn session_core_settings(
        session: &Session,
    ) -> (
        rustel_core::rng::RngMode,
        rustel_core::compose::Alignment,
        usize,
    ) {
        session.js.with_runtime_settings(|| {
            let mut controls = rustel_core::OrderedMap::new();
            controls.insert("chord".into(), Value::Str("C7".into()));
            (
                rustel_core::rng::rng_mode(),
                rustel_core::compose::default_alignment(),
                rustel_core::voicings::render_voicing(&controls)
                    .expect("configured C7 voicing")
                    .len(),
            )
        })
    }
}
