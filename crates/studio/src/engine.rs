//! In-process live engine for native editor surfaces.
//!
//! [`Session`] owns a QuickJS realm and is intentionally not `Send`. A UI
//! should therefore construct one [`StudioEngine`] inside its engine worker
//! and exchange only commands, snapshots, and [`StudioUpdate`] values with the
//! terminal thread. The CPAL callback remains independent: this worker merely
//! fills its bounded event ring through [`LiveFileProducer`].

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};

use rustel_audio::{
    AudioEvent, DecodedSample, DevicePlaybackError, LiveDeviceReport, LiveReverbBatch,
    LiveScalarDevice, MasterLevels, RecordCapture, SAMPLE_BANK_CAPACITY, SampleId, TakeoverCut,
};
use rustel_scheduler::Transport;

mod controls;
mod devices;
mod evaluation;
mod external;
mod hydra;
mod launch;
mod link;
mod loading;
mod output;
mod preview;
mod recording;
mod recovery;
mod samples;
mod shield_hold;
mod ui_state;
pub use loading::LoadingCue;
pub(crate) use loading::import_alert;
use loading::{Followed, LoadState};
use shield_hold::ShieldHold;
use ui_state::StudioUiState;

use super::settings::LoadMode;
use super::wav::{TakeStatus, TakeWriter};
use rustel_runtime::midi_clock::{self, ClockEstimate, ClockIn, ClockOut};
use rustel_runtime::sounds::Variants;
use rustel_runtime::ui_analysis::UiAudioAnalysisSet;
#[cfg(test)]
use rustel_runtime::ui_events::visual_layout;
use rustel_runtime::ui_events::{
    UiAcceptedOnset, UiAudioMetadata, UiEventSendStatus, UiLayoutEnvelope, UiTraceBatchRequest,
    source_revision,
};
use rustel_runtime::{
    EnginePressureMonitor, EnginePressureSnapshot, LiveFileProducer, LiveProducerStep,
    RuntimeError, Session, SessionConfig,
};

/// Scheduling cadence used by the native studio worker.
pub const DEFAULT_STUDIO_POLL_INTERVAL: Duration = Duration::from_millis(2);
/// Most haps one query may produce in Studio, intermediate results included;
/// a query past it is refused. Because the cap is below
/// [`rustel_core::DEFAULT_HAP_BUDGET`], the session also checks a fresh score
/// without JavaScript callbacks over its first cycle before installing it.
const STUDIO_QUERY_HAP_BUDGET: u64 = 65_536;
/// UI analysis is capped just below 30 Hz.
pub const DEFAULT_STUDIO_UI_AUDIO_INTERVAL: Duration = Duration::from_micros(33_334);
/// A short first-start lead keeps the downbeat out of an already-rendered
/// device buffer.
pub const DEFAULT_STUDIO_START_PREROLL: Duration = Duration::from_millis(150);
/// Bound graceful stop acknowledgement and sink drain.
pub const DEFAULT_STUDIO_STOP_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the mix has to stay below the silence floor before a graceful
/// stop concludes that nothing is left to hear.
const GRACEFUL_STOP_SILENCE_HOLD: Duration = Duration::from_millis(250);
/// How long a preview waits for its sample to load before giving up.
const AUDITION_LOAD_TIMEOUT: Duration = Duration::from_secs(4);
/// Default ceiling on decoded PCM the sounding score does not name.
pub const DEFAULT_PREVIEW_BUDGET_BYTES: usize = 64 * 1024 * 1024;
/// Default pause after the last preview before unused samples are dropped.
pub const DEFAULT_UNUSED_SAMPLE_IDLE: Duration = Duration::from_secs(30);
/// How long after something large is let go - an output, retired samples -
/// its memory is handed back to the system, once the engine is idle. Long
/// enough that a stop and the sample releases following it end in one call.
const FREE_MEMORY_DELAY: Duration = Duration::from_secs(1);
/// The least time between two hand-backs, so previewing sound after sound,
/// each opening and closing an output, does not trim after every one.
const FREE_MEMORY_SPACING: Duration = Duration::from_secs(5);
/// While idle, the longest freed memory waits to go back: this catches what
/// the engine does not see let go - a Hydra renderer retiring, the loader
/// threads' decode buffers, the interface's own.
const FREE_MEMORY_INTERVAL: Duration = Duration::from_secs(30);
/// How much decoded sound the tabs visited most recently may keep, beyond
/// the tabs that are always kept. Counted in bytes, not tabs: one tab is a
/// kick drum, the next is five soundfonts. Raised to the most one sound
/// may hold when "biggest sound" is set higher, so one such sound still
/// fits.
pub const RECENT_TABS_BYTES: usize = 256 * 1024 * 1024;

/// How often what the live tabs hold is worked out again while sounds keep
/// landing under the same texts. A new text or launch is worked out at
/// once; until a due pass catches up with what has landed,
/// nothing is dropped, so a sound a tab has just finished loading cannot
/// be taken for a preview.
const PROTECTION_REFRESH: Duration = Duration::from_millis(250);

/// What the performer can reach without a wait, as the studio sees it.
///
/// A set is more than the text last evaluated: a pad launches a tab never
/// opened this evening, a split pane shows a second score, and a lane kept
/// commented out is enabled mid-set. Their sounds are the performance's,
/// not the browser's, so the preview ceiling and the idle sweep must not
/// drop them, even when the text last evaluated is something else: a
/// setup, a snippet heard on its own, or the silence that a stopped
/// studio plays a preview over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveMaterial {
    /// Kept whatever the preview ceiling says: the tab playing, those open
    /// in a pane, those behind a learnt pad, the setup tabs.
    pub pinned: Vec<Arc<str>>,
    /// Every other tab, the most recently visited first, each kept whole
    /// while their sounds together fit in [`RECENT_TABS_BYTES`].
    pub recent: Vec<Arc<str>>,
    /// How many tabs have left the studio: closed, deleted, or put down
    /// with their set. A tab that leaves takes its sounds out of both
    /// lists, and the idle sweep that already ran this quiet spell kept
    /// them, so a move here owes one more. A count rather than an event:
    /// a send the full queue refused is sent again with the latest.
    pub tabs_closed: u64,
    /// Whether a setup picks sample variants: a setup open in a tab, or
    /// either setup as the studio stores it. See
    /// [`rustel_runtime::sounds::variant_selection`].
    /// Its helpers can set `n` for any score, so every tab then keeps every
    /// variant of what it names. A setup the engine has already applied
    /// counts whatever this says: see
    /// [`rustel_runtime::Session::prebake_selects_variants`].
    pub setups_select_variants: bool,
    /// The plugins the reference column shows: the open plugin of its vst
    /// tab, and the plugin of a word list for a plugin call. The idle sweep
    /// keeps them loaded, as the plugins the tabs name. The column loads a
    /// plugin on show, so an unload here starts the load again at once.
    pub plugins: Vec<String>,
}

/// The ids no memory policy may drop, and what they were worked out
/// against.
struct Protection {
    key: ProtectionKey,
    ids: std::collections::HashSet<SampleId>,
    /// The part of `ids` kept only because a recently visited tab names
    /// it, within the recent tabs' allowance: what the studio reports as
    /// theirs.
    recent: std::collections::HashSet<SampleId>,
    worked_out_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProtectionKey {
    /// Moves when the performer does something: a text evaluated or
    /// edited, a launch armed.
    texts: u64,
    /// Moves as sounds land and go: the retained set's fingerprint and the
    /// library's settled epoch.
    landed: (u64, u64),
    /// Moves as the score sounds an id not yet in
    /// [`StudioEngine::played_by_score`], or that set is let go: caught up
    /// with like a sound landing, and nothing is dropped until it is.
    played: usize,
    in_use: usize,
    /// The recent tabs' allowance, which follows the process-wide
    /// **biggest sound** ceiling: a change is the performer's too, worked
    /// out at once.
    allowance: usize,
}
/// How far ahead of the device clock a preview is placed.
const AUDITION_LEAD_SECS: f64 = 0.06;
/// How far ahead of its own moment a run's note is handed to the device.
/// Wider than the lead itself, so a note is always placed at the moment
/// it belongs to rather than at the earliest the device would take it.
const AUDITION_RUN_LEAD_SECS: f64 = AUDITION_LEAD_SECS * 2.0;
/// Gate length of a preview; a one-shot sample plays to its end anyway.
const AUDITION_GATE_SECS: f64 = 2.0;
/// A chord is held a little longer than a one-shot: the ear needs the
/// notes to overlap long enough to hear them as one sound.
const AUDITION_CHORD_GATE_SECS: f64 = 2.6;
/// Most notes one preview sounds at once. A chord nobody writes is still
/// a chord the browser must not turn into a wall of voices.
pub const MAX_AUDITION_NOTES: usize = 8;
/// A run is heard one note at a time, so it can afford more of them than a
/// chord can: two octaves of a scale and its return.
pub const MAX_AUDITION_RUN_NOTES: usize = 24;
/// Computer-key slots, independent of browser and generated previews.
const PIANO_KEYS: usize = 16;
const PIANO_RELEASE_SECS: f64 = 0.02;

/// Peak below which a decaying tail counts as digital silence, roughly
/// -120 dBFS. A quiet passage in a long sample must not look like its end.
const GRACEFUL_STOP_SILENCE_PEAK: f32 = 0.000_001;

const DEVICE_PROGRESS_DEADLINE: Duration = Duration::from_secs(3);
const DEVICE_RECYCLE_COOLDOWN: Duration = Duration::from_secs(8);
const DEVICE_LEAD_WAIT: Duration = Duration::from_millis(250);
const MAX_PENDING_DIAGNOSTICS: usize = 16;
const STARTUP_SAMPLE_WARM_BUDGET: Duration = Duration::from_millis(50);
const LIVE_RELOAD_SAMPLE_WARM_BUDGET: Duration = Duration::from_millis(15);
/// A setup runs between scores rather than under one, and what it names is
/// wanted by whatever plays next rather than by this turn, so it gets the
/// smaller live budget: enough to start the loads, never enough to hold the
/// engine off its next tick.
const SETUP_SAMPLE_WARM_BUDGET: Duration = LIVE_RELOAD_SAMPLE_WARM_BUDGET;
/// How far a stick or trigger must move past the value last written for it
/// before another line is logged. A stick swept across its throw changes by
/// more than a rounding error on nearly every turn the engine takes;
/// logged at that rate `studio.log` grows for motion nobody rereads, the
/// same flood `Trace` exists to keep off `Info` but does not by itself stop
/// from reaching disk. A fifth of the throw is a position a reader can tell
/// apart from the last one on the page; finer moves are folded in silently
/// until they add up to this much.
#[cfg(feature = "gamepad")]
const GAMEPAD_AXIS_LOG_THRESHOLD: f32 = 0.2;

/// Configuration owned by one studio engine worker.
#[derive(Clone, Debug)]
pub struct StudioConfig {
    pub session: SessionConfig,
    pub poll_interval: Duration,
    pub ui_audio_interval: Duration,
    pub start_preroll: Duration,
    pub stop_timeout: Duration,
    /// The output to open first: a device name, or `silent` for none.
    /// `None` follows the host default.
    pub output: Option<String>,
    /// The output buffer asked for, in frames, when the player chose a
    /// size: the output-latency knob - the callback size on macOS and
    /// Linux, the buffer queued ahead of the device period on Windows.
    /// `None` keeps the automatic policy (interactive default on hardware,
    /// large on forwarded sinks, `RUSTEL_LIVE_BUFFER_FRAMES` as the typed-in
    /// override).
    pub output_buffer_frames: Option<u32>,
    /// Kick off the default sample library's background manifest load at
    /// startup. The CLI keeps the default (`true`); a harness that needs no
    /// library at all turns it off, so opening a studio never starts the
    /// load. (The studio e2e harness keeps it on: registering local banks
    /// needs the loaded library, and nothing a hermetic score names depends
    /// on the pinned manifests' background fetch.)
    pub default_samples: bool,
    /// The input to open for `s("in")`: a device name, or a distinctive
    /// substring of one. `None` opens no input at all.
    pub input: Option<String>,
    /// Ceiling on decoded PCM that the current score does not name.
    /// Previewed (and previously scored) samples beyond this are dropped,
    /// oldest first. Zero means no ceiling.
    pub preview_budget_bytes: usize,
    /// After this long without a sample preview, decoded PCM the sounding
    /// score does not name is dropped. Zero means never.
    pub unused_sample_idle: Duration,
    /// Watch the gamepads from launch - the poller, the notices, a pad's
    /// row on the devices panel. The product keeps the default (`true`); a
    /// test harness turns it off so a runner's plugged-in pads never reach
    /// a hermetic studio's log (the notice queue is process-global and
    /// once-per-process, so which test's studio receives the watcher's
    /// hello would otherwise be a race).
    pub watch_pads: bool,
    /// Let a sample open the host's default audio input when no input is
    /// chosen - what ^H records from on a machine where nobody picked one.
    /// The product keeps the default (`true`); a test harness turns it off,
    /// and the studio then answers a sample the way a machine with no
    /// input does, so a hermetic studio never opens the runner's
    /// microphone (nor, on macOS, asks for permission to).
    pub open_default_input: bool,
    /// The origins of the Trust page, which scores import scripts from.
    /// They add to the origins that `session` already trusts.
    pub import_origins: Vec<String>,
}

impl Default for StudioConfig {
    fn default() -> Self {
        Self {
            session: SessionConfig::default(),
            output_buffer_frames: None,
            poll_interval: DEFAULT_STUDIO_POLL_INTERVAL,
            ui_audio_interval: DEFAULT_STUDIO_UI_AUDIO_INTERVAL,
            start_preroll: DEFAULT_STUDIO_START_PREROLL,
            stop_timeout: DEFAULT_STUDIO_STOP_TIMEOUT,
            output: None,
            default_samples: true,
            input: None,
            preview_budget_bytes: DEFAULT_PREVIEW_BUDGET_BYTES,
            unused_sample_idle: DEFAULT_UNUSED_SAMPLE_IDLE,
            watch_pads: true,
            open_default_input: true,
            import_origins: Vec::new(),
        }
    }
}

/// The origins that `session` trusts for script imports, and `origins` of
/// the Trust page added to them.
pub(super) fn import_access(
    session: &SessionConfig,
    origins: &[String],
) -> rustel_runtime::ScoreImportAccess {
    let mut access = session.score_import_access.clone();
    for origin in origins {
        // The page keeps only origins that the runtime named.
        let _ = access.permit_origin(origin);
    }
    access
}

impl StudioConfig {
    fn validate(&self) -> Result<(), RuntimeError> {
        for (name, value) in [
            ("studio poll interval", self.poll_interval),
            ("studio UI audio interval", self.ui_audio_interval),
            ("studio stop timeout", self.stop_timeout),
        ] {
            if value.is_zero() {
                return Err(RuntimeError::Message(format!(
                    "{name} must be greater than zero"
                )));
            }
        }
        Ok(())
    }
}

/// Routes deliberately owned by this first native bridge.
///
/// MIDI, OSC, and serial intents are drained and reported as unsupported; they
/// are not silently allowed to accumulate. Keeping this explicit is preferable
/// to a second, subtly different copy of the CLI's platform-output loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudioCapabilities {
    pub scalar_audio: bool,
    pub midi: bool,
    pub osc: bool,
    pub serial: bool,
}

impl Default for StudioCapabilities {
    fn default() -> Self {
        Self {
            scalar_audio: true,
            midi: false,
            osc: false,
            serial: false,
        }
    }
}

/// Stable facts about the currently open audio stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudioDeviceInfo {
    pub stream_id: u64,
    pub audio: rustel_runtime::AudioStreamFacts,
    /// Immutable kernel choices captured from this playback's opened device.
    pub registry: Arc<rustel_runtime::CapabilityRegistry>,
    /// The Studio refuses playback when this guard cannot be armed.
    pub allocator_tripwire_armed: bool,
}

/// Successful source construction. A live replacement remains pending until
/// the device takes its generation; its [`StudioUpdate::Layout`] is emitted
/// as soon as the evaluated source is observed, ahead of that cutover.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudioInstall {
    pub generation: u64,
    pub source_revision: String,
    pub pending_cutover: bool,
    /// Nothing was installed for this request: a repeat launch press was
    /// answered with the launch already in flight (or just landed), and
    /// `generation` is that launch's. The surface must not announce a
    /// second install or restart what it shows.
    pub answered_repeat: bool,
    /// Nothing was installed yet: the session holds the edit for its plugin
    /// copies, and `generation` is the one in play. The edit lands when the
    /// copies are ready, and [`StudioEngine::take_launch_outcome`] answers
    /// it then.
    pub held: bool,
}

/// Result of a graceful playback stop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudioStop {
    pub acknowledged: bool,
    pub report: LiveDeviceReport,
}

impl StudioStop {
    /// Idempotent acknowledgement for Stop while no output stream exists.
    pub(crate) fn idle() -> Self {
        Self {
            acknowledged: true,
            report: LiveDeviceReport::default(),
        }
    }
}

/// Presentation-neutral importance carried to the terminal surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudioDiagnosticLevel {
    /// Frequent internal detail, emitted several times a second. It is
    /// useful only when something has gone wrong.
    ///
    /// These lines reach the log at `Debug`. A `Note` line reaches it at
    /// `Info`, so a knob held for ten seconds would write eighty `Note`
    /// lines.
    Trace,
    /// For the log only: a fact worth keeping (what stream opened, and at
    /// what cost) that must never displace what the status line is
    /// saying about the music.
    Note,
    Info,
    Warning,
    Error,
}

/// A bounded, presentation-neutral status for the terminal surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudioDiagnostic {
    pub kind: String,
    pub message: String,
    pub recoverable: bool,
    pub level: StudioDiagnosticLevel,
    /// The condition a warning names, for the log to resolve later.
    pub alert: Option<DiagnosticAlert>,
}

/// A log alert crossing from the engine: see
/// [`super::log::StudioLog::resolve_alert`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticAlert {
    /// The line counts on the header's warning chip until its key resolves.
    Raise(String),
    /// Every line raised under this key has cleared; nothing is logged.
    Resolve(String),
}

impl StudioDiagnostic {
    fn runtime(error: &RuntimeError, recoverable: bool) -> Self {
        Self {
            kind: error.kind().to_owned(),
            message: error.to_string(),
            recoverable,
            level: StudioDiagnosticLevel::Error,
            alert: None,
        }
    }

    /// Resolve every log line raised under `key`.
    pub(super) fn resolve(key: impl Into<String>) -> Self {
        Self {
            alert: Some(DiagnosticAlert::Resolve(key.into())),
            ..Self::trace("alert", "")
        }
    }

    /// The same diagnostic, raised under `key`.
    pub(super) fn raising(self, key: impl Into<String>) -> Self {
        Self {
            alert: Some(DiagnosticAlert::Raise(key.into())),
            ..self
        }
    }

    pub(super) fn message(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            recoverable: true,
            level: StudioDiagnosticLevel::Warning,
            alert: None,
        }
    }

    pub(super) fn info(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            recoverable: true,
            level: StudioDiagnosticLevel::Info,
            alert: None,
        }
    }

    /// A line for the log alone. `info` and `message` also take the
    /// status line, which is right for news the player acts on and wrong
    /// for a fact reported beside an install: the stream facts queued as
    /// a `message` landed on the footer AFTER "playing from the top" and
    /// replaced it.
    pub(super) fn note(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            recoverable: true,
            level: StudioDiagnosticLevel::Note,
            alert: None,
        }
    }

    /// A line for the log at `Debug`: the machinery talking to itself.
    pub(super) fn trace(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            recoverable: true,
            level: StudioDiagnosticLevel::Trace,
            alert: None,
        }
    }
}

/// Typed, in-process UI traffic. No JSON serialization or parsing is required.
///
/// Layout delivery is retried until the receiver accepts it. Trace loss is
/// carried into a later batch's `dropped` field, while audio analysis is
/// replaceable and may simply be discarded by a full receiver.
#[derive(Clone, Debug, PartialEq)]
pub enum StudioUpdate {
    Layout(UiLayoutEnvelope),
    Traces(UiTraceBatchRequest),
    Audio {
        metadata: UiAudioMetadata,
        analysis: Box<UiAudioAnalysisSet>,
    },
    Diagnostic(StudioDiagnostic),
}

/// Ownership-preserving result of a nonblocking update handoff.
pub type StudioUpdateSendResult = Result<(), (UiEventSendStatus, StudioUpdate)>;

/// Adapt a standard bounded channel to [`StudioEngine::tick`].
pub fn try_send_update(
    sender: &SyncSender<StudioUpdate>,
    update: StudioUpdate,
) -> StudioUpdateSendResult {
    match sender.try_send(update) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(update)) => Err((UiEventSendStatus::DroppedFull, update)),
        Err(TrySendError::Disconnected(update)) => Err((UiEventSendStatus::Disconnected, update)),
    }
}

/// Cheap status-bar snapshot. `session_generation` may describe a candidate
/// still being prefetched; device publication and consumer confirmation are
/// separate observations, neither of which proves physical speaker delivery.
/// `Default` is the studio before anything has played: stopped, generation
/// zero, no device. Tests build one and set only the fields they are about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudioSnapshot {
    /// Accepted orchestra sources and URL cache for an isolated export.
    pub csound: rustel_runtime::CsoundSnapshot,
    pub playing: bool,
    /// A Stop has been requested and the last voices are ringing out; the
    /// device is still open and `playing` remains true until they have.
    pub stopping: bool,
    pub session_generation: u64,
    /// Producer-published device generation, retained under its original API
    /// name. It can advance before any replacement audio has been copied.
    pub audible_generation: Option<u64>,
    /// Last replay-eligible copied window on the current output epoch. Valid
    /// silence counts; an unconfirmed replacement leaves the previous value.
    /// Absent while stopped or draining a Stop.
    pub confirmed_audio_generation: Option<u64>,
    pub source_revision: Option<String>,
    pub cps: f64,
    pub device_time: f64,
    pub cycle: f64,
    pub device: Option<StudioDeviceInfo>,
    /// Friendly name of the input `s("in")` is playing, once one is open. Its
    /// channel count and sample format already ride along in `device`, in
    /// `AudioStreamFacts::input`.
    pub input_device: Option<String>,
    /// How many channels the open input has: `in` to `in:n-1`. Zero with
    /// no input open.
    pub input_channels: usize,
    /// How far behind the writer the input's readers actually sit, in
    /// frames. The lag sizes itself to the driver - never nearer than one
    /// delivery, and usually a delivery or two back - so this is the only
    /// number there is to show. Zero with no input open.
    pub input_lag_frames: u64,
    /// The loudest sample the input delivered since the last snapshot, for
    /// a meter and an activity light.
    pub input_peak: f32,
    pub recording: Option<RecordingInfo>,
    pub launch: Option<LaunchInfo>,
    /// Orbits that have sounded lately, lowest first.
    pub orbits: Vec<OrbitLevel>,
    /// Stereo pairs the output has; 1 for a stereo device.
    pub output_pairs: u16,
    pub clock: ClockStatus,
    /// Platform-neutral callback, scheduling, voice, and queue pressure.
    pub pressure: Option<EnginePressureSnapshot>,
    /// A preview whose sample is still loading - the browser shows it as
    /// such next to the sound instead of playing silence with no word.
    pub audition_loading: Option<String>,
    /// What the playing or the waiting score needs that is still loading.
    pub loading: Option<LoadingCue>,
    /// Decoded sound the engine holds, split the way its memory policy
    /// sees it.
    pub sample_memory: SampleMemory,
    /// Bytes live on the score's script heap, as its allocator counts them.
    pub script_heap_bytes: usize,
    /// What the audio side holds, or `None` with no stream open.
    pub audio_memory: Option<AudioMemory>,
}

/// Decoded sound the engine holds, split the way its memory policy sees it,
/// and the limits it holds each part to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SampleMemory {
    /// Kept: named by a live tab, the score or an armed launch.
    pub live_bytes: usize,
    /// The part of `live_bytes` kept only because a recently visited tab
    /// names it, held to `recent_limit_bytes`. The rest is kept whatever
    /// its size.
    pub recent_bytes: usize,
    /// Everything else - previews, mostly - held to the preview ceiling.
    pub preview_bytes: usize,
    /// The preview ceiling the engine applies; zero is none. Sent back so
    /// what is shown is what holds, not what the settings last asked for.
    pub preview_budget_bytes: usize,
    /// How long after the last preview unused sounds go; zero is never.
    pub unused_idle: Duration,
    /// How much the recently visited tabs may keep, as the engine applies
    /// it: see [`RECENT_TABS_BYTES`].
    pub recent_limit_bytes: usize,
}

/// What the audio side holds in memory, read on the engine thread from
/// counters it moves and figures measured when each part was made, never
/// from the callback. Parts left zeroed until a voice reaches them - the
/// voice and pending slots, the delay lines - are not counted, and neither
/// are the decoded sounds, which [`SampleMemory`] has.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioMemory {
    /// The output's event rings, as far as pushes have reached: a score
    /// fills the whole ring once it has wrapped.
    pub event_ring: usize,
    /// The input rings, as far as an input has written each.
    pub input: usize,
    /// The record tap, as far as a take has written it.
    pub record: usize,
    /// The orbit and `.FX()` reverbs the output holds.
    pub reverbs: usize,
    /// What the output's DSP wrote when it was prepared.
    pub backend: usize,
    /// The analysis taps and the producer's backlog, a ring whose ends
    /// cycle through all it reserved. The table of traces waiting for the
    /// interface is not here: it lives as long as the engine, open output
    /// or not, so it is part of what the memory breakdown does not itemise.
    pub other: usize,
}

impl AudioMemory {
    /// Every part together: what the audio side holds beside its sounds.
    pub fn total(&self) -> usize {
        self.event_ring + self.input + self.record + self.reverbs + self.backend + self.other
    }
}

/// Outcome of one producer turn.
// The one-shot Stop payload is boxed. Boxing `Running::step` instead would
// allocate on every healthy producer turn solely to equalize enum variants.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StudioTick {
    Idle,
    /// Stop was requested; no new onsets are scheduled and the device stays
    /// open until the sounding tail has decayed.
    Stopping,
    Running {
        /// `None` means score/query work failed recoverably; the device and
        /// previous audible generation continue running.
        step: Option<LiveProducerStep>,
        audible_generation: u64,
        accepted_audio: usize,
    },
    Stopped(Box<StudioStop>),
}

/// Cloneable atomic stop path for a UI thread.
///
/// Calling this does not wait behind a synchronous QuickJS evaluation. The
/// same atomic is passed into the evaluator, and the engine worker performs the
/// device drain on its next boundary.
#[derive(Clone)]
pub struct StudioStopHandle {
    transport: Arc<Transport>,
}

impl std::fmt::Debug for StudioStopHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StudioStopHandle")
            .field("stopped", &self.transport.is_stopped())
            .finish()
    }
}

impl StudioStopHandle {
    pub fn request_stop(&self) {
        self.transport.stop();
    }

    pub fn is_stopped(&self) -> bool {
        self.transport.is_stopped()
    }
}

/// Master fader and metering shared between the terminal thread and the
/// engine worker.
///
/// The fader is written by the interface and read by the engine; the levels
/// travel the other way. Both directions are plain atomics, so drawing a
/// meter never waits behind a producer turn and moving a fader never waits
/// behind a redraw.
#[derive(Debug)]
pub struct StudioMasterBus {
    max_polyphony: std::sync::atomic::AtomicUsize,
    effective_max_polyphony: std::sync::atomic::AtomicUsize,
    /// Zero means no explicit score override.
    max_polyphony_override: std::sync::atomic::AtomicUsize,
    gain_bits: AtomicU32,
    peak_bits: AtomicU32,
    lufs_bits: AtomicU32,
    clipped_blocks: AtomicU64,
    /// The worst limiter reduction since the interface last looked, linear.
    reduction_bits: AtomicU32,
    /// The limiter's ceiling in dBFS; a non-finite value is "off".
    limiter_threshold_bits: AtomicU32,
    /// Index into `rustel_audio::LimiterCharacter::ALL`.
    limiter_character: std::sync::atomic::AtomicU8,
    /// Whether the limiter's ceiling is brought back up to full scale.
    limiter_makeup: std::sync::atomic::AtomicBool,
    /// Whether starts and edits wait for their sounds: [`LoadMode::Wait`].
    load_waits: std::sync::atomic::AtomicBool,
}

impl Default for StudioMasterBus {
    fn default() -> Self {
        Self {
            max_polyphony: std::sync::atomic::AtomicUsize::new(rustel_audio::MAX_POLYPHONY),
            max_polyphony_override: std::sync::atomic::AtomicUsize::new(0),
            effective_max_polyphony: std::sync::atomic::AtomicUsize::new(
                rustel_audio::MAX_POLYPHONY,
            ),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            peak_bits: AtomicU32::new(0),
            lufs_bits: AtomicU32::new(rustel_audio::SILENCE_LUFS.to_bits()),
            clipped_blocks: AtomicU64::new(0),
            reduction_bits: AtomicU32::new(1.0f32.to_bits()),
            // The studio opens with the limiter off, and the settings turn
            // it on. The limiter adds lookahead to everything that plays:
            // five milliseconds on the default character. The output is
            // clipped at full scale with or without it, so the limiter
            // changes how the signal reaches the ceiling, not whether a
            // ceiling exists.
            limiter_threshold_bits: AtomicU32::new(f32::NAN.to_bits()),
            limiter_character: std::sync::atomic::AtomicU8::new(0),
            limiter_makeup: std::sync::atomic::AtomicBool::new(true),
            load_waits: std::sync::atomic::AtomicBool::new(LoadMode::default() == LoadMode::Wait),
        }
    }
}

impl StudioMasterBus {
    /// Host default; an explicit accepted score setting takes precedence.
    pub fn set_max_polyphony(&self, voices: usize) {
        self.max_polyphony.store(
            voices.clamp(1, rustel_audio::MAX_CONFIGURABLE_POLYPHONY),
            Ordering::Relaxed,
        );
    }

    pub fn max_polyphony(&self) -> usize {
        self.max_polyphony.load(Ordering::Relaxed)
    }

    /// Last effective value resolved by the engine, including a score override.
    pub fn effective_max_polyphony(&self) -> usize {
        self.effective_max_polyphony.load(Ordering::Relaxed)
    }

    /// Explicit score override, retained while stopped as well as playing.
    pub fn max_polyphony_override(&self) -> Option<usize> {
        let voices = self.max_polyphony_override.load(Ordering::Relaxed);
        (voices != 0).then_some(voices)
    }

    /// Interface side: move the fader.
    pub fn set_gain(&self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 4.0)
        } else {
            1.0
        };
        self.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    /// Interface side: set the ceiling, or `None` to take the limiter out.
    pub fn set_limiter(&self, settings: Option<rustel_audio::LimiterSettings>) {
        match settings {
            // A non-finite ceiling is the off sentinel, so one stored as a
            // value would read back as off anyway. Refused here so it says
            // one thing rather than two.
            Some(settings) if settings.threshold_db.is_finite() => {
                self.limiter_character
                    .store(settings.character as u8, Ordering::Relaxed);
                self.limiter_threshold_bits
                    .store(settings.threshold_db.to_bits(), Ordering::Release);
            }
            Some(_) | None => self
                .limiter_threshold_bits
                .store(f32::NAN.to_bits(), Ordering::Release),
        }
    }

    /// What the engine should put on the device; `None` while it is off.
    /// Whether the ceiling is made back up to full scale.
    ///
    /// On by default: without it, switching the limiter on makes a set
    /// quieter by exactly the headroom it was given, which reads as the
    /// limiter having broken the volume rather than having changed the
    /// sound.
    pub fn set_limiter_makeup(&self, on: bool) {
        self.limiter_makeup
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn limiter_makeup(&self) -> bool {
        self.limiter_makeup
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Interface side: how starts and edits meet sounds still loading,
    /// from the engine's next turn on.
    pub fn set_load_mode(&self, mode: LoadMode) {
        self.load_waits
            .store(mode == LoadMode::Wait, Ordering::Relaxed);
    }

    pub fn load_mode(&self) -> LoadMode {
        if self.load_waits.load(Ordering::Relaxed) {
            LoadMode::Wait
        } else {
            LoadMode::Async
        }
    }

    pub fn limiter(&self) -> Option<rustel_audio::LimiterSettings> {
        let threshold_db = f32::from_bits(self.limiter_threshold_bits.load(Ordering::Acquire));
        if !threshold_db.is_finite() {
            return None;
        }
        let character = rustel_audio::LimiterCharacter::ALL
            .get(usize::from(self.limiter_character.load(Ordering::Relaxed)))
            .copied()
            .unwrap_or_default();
        Some(rustel_audio::LimiterSettings {
            threshold_db,
            character,
        })
    }

    /// How many frames the master limiter holds the audio back right now.
    ///
    /// The MIDI schedule is timed from the device clock, which the limiter
    /// sits behind, so it reads the runway here and delays its notes to
    /// match. Off holds nothing back, so the answer is zero; a character
    /// change is picked up by the next read, so it lands only on notes
    /// scheduled after it and never moves ones already placed.
    pub fn limiter_latency_frames(&self, sample_rate: u32) -> usize {
        match self.limiter() {
            Some(settings) => {
                rustel_audio::Limiter::latency_frames_at(settings.character, sample_rate)
            }
            None => 0,
        }
    }

    /// Interface side: read and reset the level since the previous frame.
    pub fn take_levels(&self) -> MasterLevels {
        MasterLevels {
            peak: f32::from_bits(self.peak_bits.swap(0, Ordering::AcqRel)),
            lufs: f32::from_bits(self.lufs_bits.load(Ordering::Relaxed)),
            clipped_blocks: self.clipped_blocks.load(Ordering::Relaxed),
            reduction: f32::from_bits(self.reduction_bits.swap(1.0f32.to_bits(), Ordering::AcqRel)),
        }
    }

    /// Engine side: fold in one device reading. The peak is a maximum rather
    /// than a store, so a producer turn faster than the frame rate cannot
    /// hide a transient from the meter.
    fn publish(&self, levels: MasterLevels) {
        self.peak_bits
            .fetch_max(levels.peak.max(0.0).to_bits(), Ordering::Relaxed);
        self.lufs_bits
            .store(levels.lufs.to_bits(), Ordering::Relaxed);
        self.clipped_blocks
            .store(levels.clipped_blocks, Ordering::Relaxed);
        // The worst of the interval, for the same reason the peak is a max.
        self.reduction_bits
            .fetch_min(levels.reduction.max(0.0).to_bits(), Ordering::Relaxed);
    }
}

/// When an evaluate takes effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Launch {
    /// At once - one continuity margin ahead, as an edit should.
    Now,
    /// On the next line of this many cycles: a quarter for a beat, one for
    /// the cycle, more for a phrase.
    Quantised { unit_cycles: f64 },
}

/// A launch waiting for its cycle line.
#[derive(Clone, Debug)]
struct PendingLaunch {
    source: String,
    mini: bool,
    /// The line, as a cycle: what the launch is waiting for.
    boundary_cycle: f64,
    /// When that cycle fell under the mapping in force at arm time. An
    /// outside clock can retime the transport during the countdown, so
    /// the fire and the countdown read [`StudioEngine::pending_line_time`]
    /// instead.
    boundary_time: f64,
    /// The score starts from its own cycle zero when it lands, rather
    /// than joining the cycle already running.
    rewind: bool,
    /// The quantise unit the line was chosen on, in cycles. A fired
    /// rewind's grace is sized from it.
    unit_cycles: f64,
    /// A snippet previewed under the set: see
    /// [`StudioEngine::played_before_preview`].
    preview: bool,
    /// What its text needs; in wait mode it lands on the first line after
    /// all of it has loaded.
    followed: Vec<Followed>,
    /// A line has passed while its sounds were loading.
    waited: bool,
}

/// A launch that has fired: its evaluation is done, the new generation is
/// built and waiting for the line.
#[derive(Clone, Debug)]
struct Landing {
    boundary_cycle: f64,
    boundary_time: f64,
    /// What the fired evaluation installed. Its `source_revision`
    /// recognises a repeat request so the same press can be answered
    /// without arming a second launch, which would be heard as a double.
    install: StudioInstall,
    /// Whether the fired launch rewinds; a repeat must match it, since a
    /// rewind repeat means "restart again" and is a different gesture.
    rewind: bool,
    /// The quantise unit the line was chosen on, in cycles.
    unit_cycles: f64,
}

/// One orbit that has sounded lately: its meter and where it goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitLevel {
    pub orbit: u8,
    /// Highest absolute sample lately, post-fader, held and decaying.
    pub peak: f32,
    /// The output pair it is written to; 0 is the main pair.
    pub pair: u8,
}

/// How long a fired rewind stays answerable after its line: half its
/// quantise unit at the tempo it landed on.
///
/// A repeat press belongs to the line nearest it. The first press fires one
/// head-room before the line, so the second press of a fast double press
/// can arrive just after the line. In the first half of the unit after the
/// line, the press repeats the launch that landed and is answered with that
/// rewind. Past the half, the press is nearer the next line and restarts
/// there. The rule is the same for a beat, a cycle and a phrase of any
/// length.
///
/// A repeat press of the same rewinding score, by the time it arrives:
///
/// ```text
///      fire           line         line + grace
///       |<- head-room ->|<--- grace --->|
///  -----+---------------+---------------+------------> device time
///       | answered with | answered with | arms a launch
///       | the landing   | this rewind   | on the next line
/// ```
///
/// A unit or tempo that is not a positive finite number has no line to be
/// near: no grace.
fn rewind_grace_secs(unit_cycles: f64, cps: f64) -> f64 {
    if !(unit_cycles.is_finite() && unit_cycles > 0.0 && cps.is_finite() && cps > 0.0) {
        return 0.0;
    }
    0.5 * unit_cycles / cps
}

/// A fired rewind launch the ear has already heard, kept so a repeat press
/// just after its line is answered with it instead of re-installing it.
#[derive(Clone, Debug)]
struct RecentRewind {
    /// The fired install. Its `source_revision` recognises the repeat.
    install: StudioInstall,
    /// The line the launch landed on, reported while the grace runs.
    boundary_cycle: f64,
    /// When the line passed. The memory answers until this plus the grace.
    landed_at: f64,
    /// How long past `landed_at` a repeat press is still the same gesture:
    /// see [`rewind_grace_secs`].
    grace_secs: f64,
}

/// How long an orbit stays on the strip after it last sounded.
const ORBIT_LINGER: Duration = Duration::from_secs(4);
/// Per-tick decay of a held orbit peak.
const ORBIT_PEAK_DECAY: f32 = 0.86;

/// How the engine stands with outside clocks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClockStatus {
    pub out_port: Option<String>,
    pub in_port: Option<String>,
    /// The tempo heard on the clock in, once it has been heard.
    pub external_bpm: Option<f64>,
    /// Following the clock in and within a few milliseconds of it.
    pub locked: bool,
    /// The Link session, when the studio stands in one.
    pub link: Option<rustel_runtime::link::LinkStatus>,
}

/// A launch that is armed, as the interface sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaunchInfo {
    pub boundary_cycle: f64,
    pub cycles_left: f64,
    pub seconds_left: f64,
}

/// Head-room a reload needs before its takeover: the schedule lead, the
/// continuity margin, and a little for the evaluation itself.
const LAUNCH_HEADROOM_SECS: f64 = 0.08;
/// How often a moving slider re-queries the horizon. Shorter than the
/// horizon itself, so the ear never waits a whole cover for a change;
/// long enough that a held key makes a handful of seams, not sixty.
const SLIDER_REQUERY_INTERVAL: Duration = Duration::from_millis(120);

/// The next cycle line at or after `earliest`: the first multiple of
/// `unit` past `cycle_now` that leaves at least `min_ahead` cycles.
pub fn next_boundary(cycle_now: f64, unit: f64, min_ahead: f64) -> f64 {
    let unit = if unit.is_finite() && unit > 0.0 {
        unit
    } else {
        1.0
    };
    let mut boundary = (cycle_now / unit).floor() * unit + unit;
    while boundary - cycle_now < min_ahead {
        boundary += unit;
    }
    boundary
}

/// Synchronous engine core. Construct and retain it on one worker thread.
/// Where a retiring id's uninstall stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Baton {
    /// The bank never held it: an orphaned zone, or a decode whose install
    /// was refused and requeued before the library forgot it.
    NotOwed,
    /// The install ring was full; retried before each turn's installs.
    Owed,
    /// In the ring, with the device's callback count read after the push.
    /// Two more callbacks prove one whole block ran after it: the baton
    /// drained at its entry, the slot rendered empty, every voice on it
    /// retired.
    Pushed { callbacks: u64 },
}

/// An id the library has forgotten and the engine no longer retains, on
/// its way back to the library's free list once nothing can read it.
///
/// Handing it back any earlier plays the wrong sample: a decode reissued
/// the id installs over a slot an already-queued event still names, or
/// under a voice still reading it, and that event or voice sounds the new
/// PCM under the old name.
#[derive(Clone, Debug)]
struct RetiringSample {
    id: SampleId,
    baton: Baton,
    /// One past the last frame any score event queued before the library
    /// forgot this id could target. Previews are covered by the engine's
    /// live `audition_horizon_frame`.
    quiet_after_frame: u64,
}

/// Callbacks that have to complete after a baton's push before its slot is
/// provably empty and rendered so.
const BATON_SETTLE_CALLBACKS: u64 = 2;

pub struct StudioEngine {
    config: StudioConfig,
    session: Session,
    recovery_epoch: u64,
    launch_pads: Vec<(u8, u8)>,
    live: Option<StudioPlayback>,
    /// Output parked between transports: no score producer, transport or
    /// cycle ownership. A stop parks the stream here rather than closing
    /// it, and the next start adopts it where it stands.
    piano_output: Option<LiveScalarDevice>,
    piano_close_when_idle: bool,
    /// Piano mode is open in the app: from [`Self::prepare_piano`] to
    /// [`Self::close_piano_mode`]. A Stop and a focus loss stop the keys,
    /// and Piano mode stays open.
    #[cfg(feature = "vst")]
    piano_mode: bool,
    piano_sound: super::PianoSound,
    piano_volume: u16,
    piano_has_score_tail: bool,
    piano_tail_silent_since: Option<Instant>,
    stopped_device_time: Option<f64>,
    piano: [Option<PianoVoice>; PIANO_KEYS],
    master: Arc<StudioMasterBus>,
    /// Output the musician chose in the device panel. `None` follows the
    /// host default, which is what a fresh studio does.
    preferred_output: Option<String>,
    /// Input chosen for `s("in")`; opened on whatever device is live and
    /// kept across device switches. `None` means no input.
    preferred_input: Option<String>,
    /// The mixer's faders, linear, kept here so a device that follows
    /// the current one opens at the same levels.
    orbit_gains: [f32; rustel_audio::MAX_ORBITS],
    input_gain: f32,
    /// A refused or failed input. Retry after a delay and report each episode.
    input_refused: Option<String>,
    /// The input while nothing plays: a stream of its own onto a ring of
    /// its own, so the meter reads, the browser knows the channels and a
    /// "one-two" into the microphone can be seen before the music. Gone
    /// the moment an output opens - the live device carries the input
    /// then - and back on the next idle turn after a stop.
    input_monitor: Option<rustel_audio::AudioInput>,
    /// The selector the open input - live or monitor - was opened for,
    /// so a change of choice is seen as one and the old input let go.
    input_opened_for: Option<String>,
    /// The selector the last "audio in: …" line was about. The input moves
    /// from its monitor to the live device when the music starts and back
    /// when it stops; that is the same input, and not news twice.
    input_announced: Option<String>,
    /// Failed attempts since the last healthy input observation.
    input_attempts: u64,
    /// When the failed input can be opened again.
    input_retry_at: Instant,
    ui: StudioUiState,
    pending_diagnostics: PendingDiagnostics,
    pending_accepted_audio: Vec<UiAcceptedOnset>,
    /// The socket `.osc()` bundles leave by, opened the first time a score
    /// asks for one; `None` until then, and for good once opening failed.
    #[cfg(feature = "osc")]
    osc_sender: Option<rustel_osc::OscSender>,
    #[cfg(feature = "osc")]
    osc_open_failed: bool,
    /// The ports `.midi()` writes to, opened on the first onset that names
    /// one, and the clock that maps device seconds to the wall for them.
    midi_outputs: rustel_runtime::midi_bridge::MidiOutputs,
    /// The MIDI input ports a score listens to through `midin('...')` and
    /// `midikeys('...')`.
    midi_inputs: Option<rustel_runtime::midi_input::MidiInputs>,
    /// Which ports this machine may open, either way. A score naming one
    /// that is not enabled is refused rather than quietly playing into it.
    midi_enabled: super::devices::MidiEnablement,
    /// The refusals already said, so a score striking a disabled port four
    /// times a bar says it once rather than once an onset.
    midi_disabled_reported: std::collections::BTreeSet<String>,
    /// Presses taken by every score-opened keyboard, last time the
    /// producer looked. Watched for a change: see `hear_midi_keys_now`.
    midi_keys_seen: u64,
    midi_clock: rustel_runtime::midi_bridge::MidiClock,
    /// Where each key's queued note-offs were stamped: drift the clock slew
    /// absorbs over a whole note would otherwise sort the next note-on before
    /// a queued off, and the synth would cut the note it just started.
    midi_note_order: rustel_runtime::midi_bridge::NoteOrdering,
    /// The counters as last read, and which troubles were told already:
    /// each is said once a set.
    midi_report: rustel_midi::MidiReport,
    midi_trouble_reported: [bool; 4],
    /// The ports `.serial()` writes to, opened on the first onset. Kept for
    /// the engine's life and started afresh each set, so an open still in
    /// flight at a restart is adopted rather than asked for twice.
    #[cfg(feature = "serial")]
    serial_outputs: rustel_runtime::serial_bridge::SerialOutputs,
    /// Which serial refusals were told already: each is said once a set,
    /// the way the MIDI troubles are.
    #[cfg(feature = "serial")]
    serial_reported: std::collections::BTreeSet<String>,
    /// What a reload shield staged for the external outputs and has not
    /// sent yet.
    shield_hold: ShieldHold,
    /// Producer-owned PCM handles survive device stops so Stop -> Evaluate
    /// can seed a fresh backend without asking the loader to decode again.
    retained_samples: HashMap<SampleId, DecodedSample>,
    /// Uninstalls the sample bank refused because its install ring was full.
    /// Dropping a whole idle-out font at once asks for more batons than the
    /// 64-slot ring holds, and a refused uninstall would leave the PCM in
    /// the bank forever - the engine has already forgotten the id. Retried
    /// before each turn's installs, once the callback has drained room.
    retiring: Vec<RetiringSample>,
    /// The latest frame any preview pushed so far lands on. A run reaches
    /// seconds ahead, and the callback holds an event that far out in front
    /// of everything pushed after it, so nothing retires until the frontier
    /// is past here.
    audition_horizon_frame: u64,
    /// When each retained id was last installed or previewed.
    sample_last_used: HashMap<SampleId, Instant>,
    /// Last time a browser preview asked for a sound.
    last_preview_at: Instant,
    /// Whether the idle sweep has already run for this quiet spell. The
    /// sweep asks the score what it names - a source scan and two of the
    /// library's tables - and once the idle has elapsed it would otherwise
    /// be owed on every turn of the engine loop rather than once.
    idle_swept: bool,
    /// The score last warmed, so idle eviction can keep what it still
    /// names while it plays.
    current_score: String,
    preview_budget_bytes: usize,
    unused_sample_idle: Duration,
    /// The names the text last evaluated can reach, and the variants of
    /// each it can play, read once per text.
    current_score_names: Vec<(String, Variants)>,
    /// The sounds the score's own pattern resolved in the window its last
    /// warm queried, with the variants picked there. Kept while it plays,
    /// like [`Self::current_score_names`], and they reach what no text
    /// scan can: a name built in JavaScript.
    score_window_names: Vec<(String, Variants)>,
    /// The names the always-kept tabs can reach, as one set, each with the
    /// variants any of them can play.
    live_pinned: Vec<(String, Variants)>,
    /// The names each other tab can reach, most recently visited first.
    live_recent: Vec<Vec<(String, Variants)>>,
    /// [`LiveMaterial::setups_select_variants`] as last sent.
    live_setups_select_variants: bool,
    /// Samples sounded by the current score, including its predecessor
    /// until the replacement is confirmed audible. Stop leaves
    /// [`Self::played_by_text`]. The scan of a text is a bet on what it can
    /// play; this is what it did play - a variant picked by code the scan
    /// could not follow, a name built at runtime. Filled where the score's
    /// events are handed to the output, on this thread, never the
    /// callback's; bounded by the bank's slots. An audition and the keyboard
    /// never add to it, and a snippet previewed under the set is taken back
    /// out with the snippet: see [`Self::played_before_preview`].
    played_by_score: std::collections::HashSet<SampleId>,
    /// The part of [`Self::played_by_score`] that [`Self::played_text`]
    /// sounded, from [`Self::text_from_generation`] on: what stays after
    /// Stop, until a different text starts or no tab holds this one.
    played_by_text: std::collections::HashSet<SampleId>,
    /// A replacement keeps the previous score's dynamic samples available
    /// for rollback until the callback confirms the new score.
    played_reset_pending: bool,
    /// Accepted notes keep their PCM through their latest possible bank
    /// read, even after an edit removes their score's longer-lived pins.
    sample_use_until: HashMap<SampleId, u64>,
    /// The first generation whose events count as [`Self::played_text`]'s:
    /// an earlier one is the text before it, playing on to its cutover.
    text_from_generation: u64,
    /// The text the score was last installed as, while a tab holds it;
    /// `None` once a tab has left and none does.
    played_text: Option<String>,
    /// Whether the transport has yet to install the performer's score: it
    /// may have opened on the silence or the snippet a preview or a take
    /// plays over, and the first install that is no preview starts it.
    awaiting_score: bool,
    /// [`Self::played_by_score`] and [`Self::played_by_text`] as they stood
    /// before a snippet was previewed under the set, while the snippet is
    /// the score. What the preview sounds counts while it plays, so nothing
    /// is dropped from under it; the score put back takes up these sets
    /// again, as though the preview had never played. Kept, the preview's
    /// sounds would stay protected - and count as the set's - though
    /// nothing the performer runs can play them.
    played_before_preview: Option<(
        std::collections::HashSet<SampleId>,
        std::collections::HashSet<SampleId>,
    )>,
    /// The first generation whose events count as the score's: those of an
    /// earlier one, pushed after the score was put back over a preview,
    /// are the preview playing on to its cutover.
    played_from_generation: u64,
    /// Whether the score handed over next - evaluated at once, or armed
    /// for a line and carried there by [`PendingLaunch::preview`] - is a
    /// snippet previewed under the set. Set by the worker with each
    /// evaluation, and taken by the install.
    next_install_preview: bool,
    /// [`LiveMaterial::tabs_closed`] as last sent.
    live_tabs_closed: u64,
    /// The plugins the texts of the tabs name, as last sent.
    #[cfg(feature = "vst")]
    live_plugins_named: Vec<String>,
    /// The strings of the texts of the tabs, as last sent: see
    /// [`rustel_runtime::vst::strings_in_source`].
    #[cfg(feature = "vst")]
    live_plugin_strings: Vec<String>,
    /// [`LiveMaterial::plugins`] as last sent.
    #[cfg(feature = "vst")]
    live_plugins_shown: Vec<String>,
    /// Whether the idle sweep of this quiet spell has unloaded the plugins
    /// nothing keeps: see [`Self::unload_idle_plugins`].
    #[cfg(feature = "vst")]
    plugins_swept: bool,
    /// How much decoded sound the recently visited tabs may keep, when a
    /// test says: see [`Self::recent_tabs_allowance`] otherwise.
    recent_tabs_bytes: Option<usize>,
    /// Bumped whenever what is protected may have changed with the texts:
    /// the score evaluated, or the live material.
    protection_generation: u64,
    /// The ids last worked out as protected: see [`Protection`].
    protection: Option<Protection>,
    /// When memory let go should be handed back to the system, once idle.
    free_memory_due: Option<Instant>,
    /// When memory was last handed back.
    last_free_memory: Instant,
    /// The take being written, if one is.
    recording: Option<Recording>,
    /// A closing tap or writer whose final result has not been collected.
    /// Mutually exclusive with `recording`; a new take waits for collection.
    closing_recording: Option<ClosingRecording>,
    /// The sample being recorded from the audio input, if one is. Never at
    /// the same time as a take: the two are one recorder to the player.
    sample_recording: Option<SampleRecording>,
    /// What the snippet shelf asked to be previewed, waiting for the next turn.
    #[cfg(feature = "hydra")]
    hydra_preview: Option<Option<String>>,
    /// The theme's own sketch to install, when one arrived since the last
    /// drive. The bool declares a camera-backed s0; it is not permission.
    #[cfg(feature = "hydra")]
    hydra_theme: Option<(Option<String>, bool)>,
    /// How large a frame the terminal will read, waiting for the next turn.
    #[cfg(feature = "hydra")]
    hydra_frame_size: Option<rustel_runtime::hydra::HydraFrameRequest>,
    /// A launch waiting for its cycle line, and what came of the last one.
    pending_launch: Option<PendingLaunch>,
    /// Sounds followed while they load: see [`LoadState`].
    load: LoadState,
    launch_outcome: Option<Result<StudioInstall, RuntimeError>>,
    /// The last time a slider move re-queried the horizon, and whether a
    /// move since is still waiting for its turn. A held key or a drag
    /// moves a slider dozens of times a second; every move's value lands
    /// at once (the score reads the slider when it queries), but the
    /// re-query that brings it forward into the already-queried horizon
    /// costs a generation flip with a takeover seam, and a seam per
    /// keystroke is audible as gaps. One seam per `SLIDER_REQUERY_INTERVAL`
    /// carries the latest value; the rest ride the next query.
    slider_requery_at: Option<Instant>,
    slider_requery_pending: bool,
    /// Latest audio target per binding when the callback control ring is full.
    pending_live_controls: Vec<rustel_audio::live_control::LiveControlUpdate>,
    /// A launch that has fired but whose line is still ahead: the boundary
    /// it is landing on, and the install its evaluation produced. The
    /// countdown keeps running to the line, which is what the ear is
    /// waiting for; the install rides it so a repeat request can be
    /// answered without arming a second one.
    landing: Option<Landing>,
    /// The rewind launch that fired most recently and what became of it,
    /// kept briefly past its line so an eager double-press is answered
    /// rather than re-installed. Cleared by anything that replaces the
    /// score by other means: an edit, a stop, a line-cut with a new score.
    recent_rewind: Option<RecentRewind>,
    /// Whether this engine has withdrawn the pre-armed line cut because the
    /// producer held the replacement's flip for sample loading. While held,
    /// the cut at the line would fade the outgoing score into a room the
    /// incoming one cannot yet sound - silence, then a click when the flip
    /// finally lands. Withdrawn instead: the old score keeps playing until
    /// the replacement can render. The cut is not re-armed when loading
    /// finishes: the landed flip carries its own cut at the line.
    line_cut_withdrawn: bool,
    /// Test hook: make the next output-latency recycle lose its output
    /// and fail to reopen it, the path that stops the engine.
    #[cfg(test)]
    fail_output_recycles_for_test: bool,
    /// The orbit meters: when each last sounded and its held peak.
    orbit_last_heard: [Option<Instant>; rustel_audio::scalar::MAX_ORBITS],
    orbit_peak: [f32; rustel_audio::scalar::MAX_ORBITS],
    /// Where each orbit is sent, kept across devices.
    orbit_routing: [u8; rustel_audio::scalar::MAX_ORBITS],
    clock_out: Option<ClockOut>,
    clock_in: Option<ClockIn>,
    clock_followed_at: Instant,
    clock_locked: bool,
    clock_external_cps: Option<f64>,
    link: Option<rustel_runtime::link::Link>,
    link_followed_at: Instant,
    link_playing_sent: Option<bool>,
    /// Each pad's buttons as last read, for telling a press or a release
    /// from a value that was already there. Seeded at rest: a pad found
    /// already held down the first time it is read still counts as freshly
    /// pressed, the same as a controller plugged in mid-chord.
    #[cfg(feature = "gamepad")]
    gamepad_buttons_seen: [[f32; rustel_core::gamepad::BUTTONS]; rustel_core::gamepad::MAX_PADS],
    /// Each axis's value the last time a line was written for it, kept for
    /// [`GAMEPAD_AXIS_LOG_THRESHOLD`]; `None` until one has been.
    #[cfg(feature = "gamepad")]
    gamepad_axes_logged:
        [[Option<f32>; rustel_core::gamepad::AXES]; rustel_core::gamepad::MAX_PADS],
}

/// Frames kept clear of the input ring's writer when a sample is drained:
/// one large driver delivery can land between the check and the read, and a
/// frame the writer has already come back round to is not the frame it was.
const SAMPLE_RING_GUARD_FRAMES: u64 = 8_192;

/// The most input frames one turn hands the writer, so a turn after a stall
/// is bounded like every other.
const SAMPLE_DRAIN_FRAMES: u64 = 16_384;

/// A sample being recorded from the audio input.
///
/// Read on the engine's own turns from the ring the input callback already
/// writes - the one `s("in")` plays - so the audio callbacks are not touched
/// and what is recorded is what the input fader lets through. A take records
/// the final mix instead; one recorder runs at a time.
struct SampleRecording {
    writer: TakeWriter,
    ring: Arc<rustel_audio::input::InputRing>,
    /// The ring frame the next read starts from.
    cursor: u64,
    /// An input opened for this recording alone, on the host's default
    /// device, when none is chosen: closed with the recording.
    own_input: Option<rustel_audio::AudioInput>,
    /// Frames the ring had already written over before they were read.
    dropped: u64,
}

impl SampleRecording {
    /// Hand everything the input wrote since the last turn to the writer,
    /// in bounded chunks, as stereo: a mono input sounds on both sides.
    ///
    /// All of it, not one chunk: a turn that took one chunk left the rest to
    /// be written over, and a recording pumped late kept only its ending.
    fn drain(&mut self) {
        while self.drain_chunk() {}
    }

    /// One chunk of [`Self::drain`]; `false` once there is nothing left.
    fn drain_chunk(&mut self) -> bool {
        let written = self.ring.written();
        let oldest = written.saturating_sub(
            (rustel_audio::input::INPUT_RING_FRAMES as u64)
                .saturating_sub(SAMPLE_RING_GUARD_FRAMES),
        );
        if self.cursor < oldest {
            self.dropped = self.dropped.saturating_add(oldest - self.cursor);
            self.cursor = oldest;
        }
        let frames = written.saturating_sub(self.cursor).min(SAMPLE_DRAIN_FRAMES);
        if frames == 0 {
            return false;
        }
        let stereo = self.ring.channels() >= 2;
        let mut chunk = Vec::with_capacity(frames as usize * 2);
        for frame in self.cursor..self.cursor + frames {
            let left = self.ring.sample(frame, 0);
            chunk.push(left);
            chunk.push(if stereo {
                self.ring.sample(frame, 1)
            } else {
                left
            });
        }
        self.cursor += frames;
        if !self.writer.push(chunk) {
            self.dropped = self.dropped.saturating_add(frames);
        }
        true
    }
}

/// A take in progress: the device's record tap, drained every tick into a
/// writer thread. While the device is closed the take is kept on the wall
/// clock with silence, so a stop in the middle of a set is a gap in the
/// file and not a splice.
struct Recording {
    writer: TakeWriter,
    input: Option<RecordingInput>,
    sample_rate: u32,
    started: Instant,
    /// Frames handed to the writer, silence included.
    frames: u64,
    /// Frames that became silence because the disk or the ring fell behind.
    dropped: u64,
    tap_dropped_seen: u64,
    scratch: Vec<f32>,
}

struct RecordingInput {
    capture: RecordCapture,
    sample_rate: u32,
}

enum ClosingRecording {
    Tap {
        recording: Recording,
        target: u64,
        has_output: bool,
        flush: bool,
    },
    Writer(TakeWriter),
}

impl Recording {
    fn target_frames(&self) -> u64 {
        (self.started.elapsed().as_secs_f64() * f64::from(self.sample_rate)) as u64
    }

    fn drain_input(&mut self, device: Option<&LiveScalarDevice>) -> Result<u64, String> {
        let Some(input) = self.input.as_mut() else {
            return Ok(0);
        };
        if !device.is_some_and(|device| device.owns_record_capture(&input.capture)) {
            input
                .capture
                .request_close()
                .map_err(|error| error.to_string())?;
        }
        // Observe retirement before draining: a writer that retires after
        // this drain may have published one more block for the next turn.
        let retired = input.capture.is_closed();
        let loss = if input.sample_rate == self.sample_rate {
            input
                .capture
                .drain(&mut self.scratch)
                .map_err(|error| error.to_string())?;
            let dropped = input
                .capture
                .dropped_frames()
                .map_err(|error| error.to_string())?;
            let loss = dropped.saturating_sub(self.tap_dropped_seen);
            self.tap_dropped_seen = dropped;
            loss
        } else {
            0
        };
        if retired {
            self.input = None;
            self.tap_dropped_seen = 0;
        }
        Ok(loss)
    }

    fn submit(&mut self, tap_loss: u64, target: u64, has_output: bool) -> Result<(), &'static str> {
        let pcm_frames =
            u64::try_from(self.scratch.len() / 2).map_err(|_| "recording frame count overflow")?;
        let silence = recording_padding(
            self.frames,
            pcm_frames,
            tap_loss,
            target,
            self.sample_rate,
            has_output,
        )?;
        let frames = pcm_frames
            .checked_add(silence)
            .ok_or("recording frame count overflow")?;
        if frames == 0 {
            return Ok(());
        }
        let next_frames = self
            .frames
            .checked_add(frames)
            .ok_or("recording frame count overflow")?;
        let dropped = self
            .dropped
            .checked_add(tap_loss)
            .ok_or("recording dropped frame count overflow")?;
        let chunk = std::mem::replace(&mut self.scratch, Vec::with_capacity(1 << 16));
        let accepted = self.writer.push_padded(chunk, silence)?;
        let next_dropped = if accepted {
            dropped
        } else {
            dropped
                .checked_add(frames)
                .ok_or("recording dropped frame count overflow")?
        };
        // Preserve attempted-frame accounting and whole-item loss on Full.
        // A rejected item is not automatically replaced by silence later.
        self.frames = next_frames;
        self.dropped = next_dropped;
        Ok(())
    }
}

fn recording_padding(
    frames: u64,
    pcm_frames: u64,
    tap_loss: u64,
    target: u64,
    sample_rate: u32,
    has_output: bool,
) -> Result<u64, &'static str> {
    let have = frames
        .checked_add(pcm_frames)
        .and_then(|have| have.checked_add(tap_loss))
        .ok_or("recording frame count overflow")?;
    let rate = u64::from(sample_rate);
    let missing = target.saturating_sub(have);
    let threshold = if has_output { rate / 2 } else { 0 };
    let wall_padding = if missing > threshold {
        missing.min(rate)
    } else {
        0
    };
    tap_loss
        .checked_add(wall_padding)
        .ok_or("recording frame count overflow")
}

/// One take, as the interface sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingInfo {
    pub path: std::path::PathBuf,
    pub seconds: f64,
    pub bytes: u64,
    pub dropped_seconds: f64,
    pub error: Option<String>,
}

impl std::fmt::Debug for StudioEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StudioEngine")
            .field("config", &self.config)
            .field("playing", &self.live.is_some())
            .field("generation", &self.session.generation())
            .finish_non_exhaustive()
    }
}

impl StudioEngine {
    pub fn new(config: StudioConfig) -> Result<Self, RuntimeError> {
        let preferred_output = config.output.clone();
        let preferred_input = config.input.clone();
        config.validate()?;
        let ui_audio_interval = config.ui_audio_interval;
        let preview_budget_bytes = config.preview_budget_bytes;
        let unused_sample_idle = config.unused_sample_idle;
        let mut session = Session::with_config(config.session.clone())?;
        session.set_score_import_access(import_access(&config.session, &config.import_origins));
        session.set_query_hap_budget(STUDIO_QUERY_HAP_BUDGET)?;
        let master = Arc::new(StudioMasterBus::default());
        master.set_max_polyphony(config.session.max_polyphony);
        master
            .effective_max_polyphony
            .store(config.session.max_polyphony, Ordering::Relaxed);
        // Watch the pads from launch, not from the first score that names
        // one, so the studio reports a pad that is plugged in before
        // anything plays. With `watch_pads: false` no pad stack is opened
        // and no pad notice reaches the log.
        #[cfg(feature = "gamepad")]
        if config.watch_pads {
            rustel_runtime::gamepad::ensure_polling();
        }
        session.set_schedule_trace_enabled(true);
        session.set_direct_diagnostic_logging(false);

        let mut pending_diagnostics = PendingDiagnostics::default();
        if config.default_samples
            && let Err(error) = session.enable_default_samples()
        {
            pending_diagnostics.push_back(StudioDiagnostic::message(
                "sample-library",
                format!("default sample library is unavailable: {error}"),
            ));
        }
        // A pack imported in Settings is fetched under no less than a score
        // is: the policy the host chose for scores, with each pack's own
        // origin permitted on top of it. The library must exist first.
        if let Some(library) = session.sample_library() {
            library.set_import_policy(config.session.score_sample_access.clone());
        }

        Ok(Self {
            config,
            session,
            recovery_epoch: 0,
            launch_pads: Vec::new(),
            live: None,
            piano_output: None,
            piano_close_when_idle: true,
            #[cfg(feature = "vst")]
            piano_mode: false,
            piano_sound: super::PianoSound::default(),
            piano_volume: super::piano::DEFAULT_PIANO_VOLUME,
            piano_has_score_tail: false,
            piano_tail_silent_since: None,
            stopped_device_time: None,
            piano: [None; PIANO_KEYS],
            master,
            preferred_output: preferred_output.clone(),
            preferred_input,
            orbit_gains: [1.0; rustel_audio::MAX_ORBITS],
            input_gain: 1.0,
            input_refused: None,
            input_monitor: None,
            input_opened_for: None,
            input_announced: None,
            input_attempts: 0,
            input_retry_at: Instant::now(),
            ui: StudioUiState::new(ui_audio_interval),
            pending_diagnostics,
            pending_accepted_audio: Vec::new(),
            #[cfg(feature = "osc")]
            osc_sender: None,
            #[cfg(feature = "osc")]
            osc_open_failed: false,
            midi_outputs: rustel_runtime::midi_bridge::MidiOutputs::new(),
            midi_inputs: None,
            midi_enabled: super::devices::MidiEnablement::default(),
            midi_disabled_reported: std::collections::BTreeSet::new(),
            midi_keys_seen: 0,
            midi_clock: rustel_runtime::midi_bridge::MidiClock::new(),
            midi_note_order: rustel_runtime::midi_bridge::NoteOrdering::new(),
            midi_report: rustel_midi::MidiReport::default(),
            midi_trouble_reported: [false; 4],
            #[cfg(feature = "serial")]
            serial_outputs: rustel_runtime::serial_bridge::SerialOutputs::new(),
            #[cfg(feature = "serial")]
            serial_reported: std::collections::BTreeSet::new(),
            shield_hold: ShieldHold::default(),
            retained_samples: HashMap::new(),
            retiring: Vec::new(),
            audition_horizon_frame: 0,
            sample_last_used: HashMap::new(),
            last_preview_at: Instant::now(),
            idle_swept: false,
            current_score: String::new(),
            preview_budget_bytes,
            unused_sample_idle,
            current_score_names: Vec::new(),
            score_window_names: Vec::new(),
            live_pinned: Vec::new(),
            live_recent: Vec::new(),
            live_setups_select_variants: false,
            played_by_score: std::collections::HashSet::new(),
            played_by_text: std::collections::HashSet::new(),
            played_reset_pending: false,
            sample_use_until: HashMap::new(),
            text_from_generation: 0,
            played_text: None,
            awaiting_score: false,
            played_before_preview: None,
            played_from_generation: 0,
            next_install_preview: false,
            live_tabs_closed: 0,
            #[cfg(feature = "vst")]
            live_plugins_named: Vec::new(),
            #[cfg(feature = "vst")]
            live_plugin_strings: Vec::new(),
            #[cfg(feature = "vst")]
            live_plugins_shown: Vec::new(),
            #[cfg(feature = "vst")]
            plugins_swept: false,
            recent_tabs_bytes: None,
            protection_generation: 0,
            protection: None,
            free_memory_due: None,
            last_free_memory: Instant::now(),
            recording: None,
            closing_recording: None,
            sample_recording: None,
            #[cfg(feature = "hydra")]
            hydra_preview: None,
            #[cfg(feature = "hydra")]
            hydra_theme: None,
            #[cfg(feature = "hydra")]
            hydra_frame_size: None,
            pending_launch: None,
            load: LoadState::default(),
            launch_outcome: None,
            landing: None,
            recent_rewind: None,
            line_cut_withdrawn: false,
            #[cfg(test)]
            fail_output_recycles_for_test: false,
            slider_requery_at: None,
            slider_requery_pending: false,
            pending_live_controls: Vec::new(),
            orbit_last_heard: [None; rustel_audio::scalar::MAX_ORBITS],
            orbit_peak: [0.0; rustel_audio::scalar::MAX_ORBITS],
            orbit_routing: [0; rustel_audio::scalar::MAX_ORBITS],
            clock_out: None,
            clock_in: None,
            clock_followed_at: Instant::now(),
            clock_locked: false,
            clock_external_cps: None,
            link: None,
            link_followed_at: Instant::now(),
            link_playing_sent: None,
            #[cfg(feature = "gamepad")]
            gamepad_buttons_seen: [[0.0; rustel_core::gamepad::BUTTONS];
                rustel_core::gamepad::MAX_PADS],
            #[cfg(feature = "gamepad")]
            gamepad_axes_logged: [[None; rustel_core::gamepad::AXES];
                rustel_core::gamepad::MAX_PADS],
        })
    }

    pub fn capabilities(&self) -> StudioCapabilities {
        StudioCapabilities::default()
    }

    pub fn poll_interval(&self) -> Duration {
        self.config.poll_interval
    }

    /// Test seam: the cycle the next continuous replacement will join at -
    /// the re-query takeover instant, read on the scheduler's mapping - and
    /// forget the instant. `None` says none is named.
    #[doc(hidden)]
    pub fn requery_takeover_cycle_for_test(&mut self) -> Option<f64> {
        let takeover = self.session.take_requery_takeover_time()?;
        Some(self.session.cycle_at_time(takeover))
    }

    /// Test seam: plant the from-zero request exactly as a rewinding
    /// evaluate does - the worker sets it just before calling in.
    #[doc(hidden)]
    pub fn plant_from_zero_for_test(&mut self) {
        self.session.start_next_from_zero();
    }

    pub fn stop_timeout(&self) -> Duration {
        self.config.stop_timeout
    }

    /// The engine's sample library, for checking sound names before they
    /// are asked for.
    pub fn sample_library(&self) -> Option<Arc<rustel_runtime::samples::SampleLibrary>> {
        self.session.sample_library().cloned()
    }

    /// The shared fader and meter. Clone it onto the interface thread.
    pub fn master_bus(&self) -> Arc<StudioMasterBus> {
        Arc::clone(&self.master)
    }

    /// Play the next score from its own cycle zero rather than from where
    /// the cycle already stands - a launch that rewinds.
    ///
    /// The transport keeps running and nothing sounding is cut: the moment
    /// the new score takes over becomes cycle zero, so what is ringing out
    /// under it rings out on the times it was already given.
    pub fn start_next_from_zero(&mut self) {
        // The immediate path only: the quantised launch logs its own fire
        // at the line (advance_launch). Logged here so a reader can match
        // the from-zero request with other log lines of the same moment.
        self.log_launch("rewind now - from zero");
        self.session.start_next_from_zero();
    }

    /// Silence everything sounding, over the choke ramp.
    ///
    /// For swapping an audition of a whole snippet: the one being replaced
    /// has to stop rather than ring on under the one asked for. A reload
    /// deliberately leaves voices alone - an edit that cut them would click
    /// on every save - so this is asked for, never implied.
    pub fn cut_sounding(&mut self) {
        // Also cancel keys whose onsets are still queued: cutting active
        // voices alone can otherwise let a pending held note start afterward.
        self.stop_piano();
        if let Some(device) = self.audio_device() {
            device.cut_sounding();
        }
    }

    pub fn stop_handle(&self) -> StudioStopHandle {
        StudioStopHandle {
            transport: self.session.transport(),
        }
    }

    pub fn is_playing(&self) -> bool {
        self.live.is_some()
    }

    /// True between a Stop request and the moment the tail has rung out.
    pub fn is_stopping(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.draining.is_some())
    }

    /// Begin a graceful stop: the transport halts so nothing new is
    /// scheduled, and the next tick retires every onset that has not sounded
    /// yet while letting the voices already playing finish. Idempotent, and
    /// a no-op while nothing is playing.
    pub fn request_stop(&mut self) {
        // A stop is a stop: a launch waiting for its line is not going to
        // happen, and saying so now is what ends its countdown.
        self.supersede_launch();
        self.end_loads();
        if let Some(live) = self.live.as_ref() {
            self.stopped_device_time
                .get_or_insert_with(|| live.device.clock_seconds());
        }
        self.stop_piano();
        self.session.transport().stop();
    }

    pub fn generation(&self) -> u64 {
        self.session.generation()
    }

    pub fn active_source(&self) -> Option<&str> {
        self.session.active_source()
    }

    pub fn device_info(&self) -> Option<StudioDeviceInfo> {
        self.live
            .as_ref()
            .map(|live| StudioDeviceInfo::from_device(&live.device, Arc::clone(&live.registry)))
    }

    pub fn device_report(&self) -> Option<LiveDeviceReport> {
        self.audio_device().map(LiveScalarDevice::report)
    }

    pub fn snapshot(&mut self) -> StudioSnapshot {
        let pressure = self.live.as_mut().map(|live| {
            let buffer_frames = live
                .device
                .reported_buffer_frames()
                .unwrap_or_else(|| live.device.requested_buffer_frames());
            let device = live.device.report();
            let producer = live.producer.producer_load_snapshot();
            live.pressure_monitor
                .sample(device, producer, buffer_frames)
        });
        let device_time = self
            .live
            .as_ref()
            .map(|live| live.device.clock_seconds())
            .unwrap_or(self.stopped_device_time.unwrap_or(0.0));
        let device_time = if self.session.transport().is_stopped() {
            *self.stopped_device_time.get_or_insert(device_time)
        } else {
            device_time
        };
        StudioSnapshot {
            csound: self.session.csound_snapshot(),
            playing: self.live.is_some(),
            stopping: self.is_stopping(),
            session_generation: self.session.generation(),
            audible_generation: self.live.as_ref().map(|live| live.device.generation()),
            confirmed_audio_generation: self
                .live
                .as_ref()
                .filter(|_| !self.is_stopping() && !self.session.transport().is_stopped())
                .and_then(|_| self.session.confirmed_audio_generation()),
            source_revision: self.session.active_source().map(source_revision),
            cps: self.session.cps(),
            device_time,
            cycle: self.session.cycle_at_time(device_time),
            device: self.device_info(),
            input_device: self
                .audio_device()
                .and_then(LiveScalarDevice::input_name)
                .or_else(|| {
                    self.input_monitor
                        .as_ref()
                        .map(rustel_audio::AudioInput::name)
                })
                .map(str::to_owned),
            input_channels: self.input_channels(),
            input_lag_frames: self
                .audio_device()
                .filter(|device| device.input_name().is_some())
                .map_or(0, LiveScalarDevice::input_read_lag_frames),
            input_peak: match (
                self.audio_device()
                    .filter(|device| device.input_name().is_some()),
                self.input_monitor.as_ref(),
            ) {
                (Some(device), _) => device.take_input_peak(),
                (None, Some(monitor)) => monitor.take_peak(),
                (None, None) => 0.0,
            },
            recording: self.recording_info(),
            launch: self.launch_info(),
            clock: ClockStatus {
                out_port: self.clock_out.as_ref().map(|out| out.port().to_owned()),
                in_port: self.clock_in.as_ref().map(|input| input.port().to_owned()),
                external_bpm: self.clock_external_cps.map(midi_clock::bpm),
                locked: self.clock_locked,
                link: self.link.as_ref().map(|link| link.status()),
            },
            orbits: self.orbit_levels(),
            output_pairs: self
                .audio_device()
                .map(LiveScalarDevice::output_pairs)
                .unwrap_or(1),
            pressure,
            audition_loading: self
                .live
                .as_ref()
                .and_then(|live| live.audition.as_ref())
                .map(|pending| pending.request.sound.clone()),
            loading: self.loading_cue(),
            sample_memory: self.sample_memory(),
            script_heap_bytes: self.session.js_heap_live(),
            audio_memory: self.audio_memory(),
        }
    }

    /// Advance one producer turn using elapsed time from this playback start.
    pub fn tick(
        &mut self,
        emit: impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) -> Result<StudioTick, RuntimeError> {
        let observed_at = self
            .live
            .as_ref()
            .map(|live| live.started_at.elapsed())
            .unwrap_or_default();
        self.tick_at(observed_at, emit)
    }

    /// The turn a stopped engine takes: what the devices have to say
    /// reaches the studio whether or not anything plays.
    pub fn idle_turn(&mut self, mut emit: impl FnMut(StudioUpdate) -> StudioUpdateSendResult) {
        self.sync_recovered_session();
        self.sync_polyphony();
        self.pump_recording();
        self.service_idle_piano();
        self.ensure_input();
        // A sample is often recorded with the transport stopped. If only
        // the playing tick drained it, a stopped recording would keep just
        // what the input ring still holds when the recording ends.
        self.pump_sample_recording();
        self.collect_gamepad_notices();
        self.tend_idle_sample_memory();
        self.settle_loads();
        // The stopped path is this one, not `tick_at`: the worker calls
        // `tick` only while there is a device. MIDI input does not depend
        // on playback, so this turn also opens a keyboard that a score asks
        // for and closes one that it stops asking for.
        self.open_score_midi_inputs();
        self.pump_link();
        emit_pending_diagnostics(&mut self.pending_diagnostics, &mut emit);
    }

    /// Deterministic-time form used by worker integrations and unit tests.
    pub fn tick_at(
        &mut self,
        observed_at: Duration,
        mut emit: impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) -> Result<StudioTick, RuntimeError> {
        self.sync_recovered_session();
        self.sync_polyphony();
        self.sync_input_channels();
        self.session.consume_audio_confirmations();
        self.sync_played_score();
        self.collect_session_diagnostics();
        self.pump_recording();
        self.pump_sample_recording();
        self.sync_line_cut_with_loading_holds();
        self.advance_launch();
        self.advance_held_edit();
        self.settle_landing();
        self.settle_loads();
        self.flush_slider_requery();
        self.collect_gamepad_notices();
        self.flush_live_controls();
        // Every turn, playing or not. A keyboard is not part of the audio
        // step. A score that asks for a keyboard gets it when it is
        // evaluated, so a port that cannot open is reported then and not
        // at the next play. A score that stops asking releases the port at
        // once and does not hold it through a stop: on some hosts a port
        // has one owner.
        self.open_score_midi_inputs();
        self.hear_midi_keys_now();
        emit_pending_diagnostics(&mut self.pending_diagnostics, &mut emit);

        if self.live.is_none() {
            self.service_idle_piano();
            self.tend_idle_sample_memory();
            return Ok(StudioTick::Idle);
        }
        if let Some(live) = self.live.as_mut() {
            live.producer.reap_completed_output(
                live.device.completed_intake_frames(),
                live.device.sample_rate(),
            );
            let refusals = live.device.report().asset_queues.fx_reverb_refusals;
            if let Some(diagnostic) =
                fx_reverb_refusal_diagnostic(refusals, &mut live.last_fx_reverb_refusals)
            {
                queue_diagnostic(&mut self.pending_diagnostics, diagnostic);
            }
        }
        self.sync_polyphony();
        {
            // The fader travels down and the levels travel up on every turn,
            // so the meter stays live even while a score is silent.
            let live = self.live.as_ref().expect("checked playback");
            live.device.set_master_gain(self.master.gain());
            // The limiter is set with the gain on every turn, so a ceiling
            // set from the interface reaches the device.
            live.device.set_limiter(self.master.limiter());
            live.device.set_limiter_makeup(self.master.limiter_makeup());
            self.apply_mix_gains(&live.device);
            let levels = live.device.take_levels();
            self.master.publish(levels);
        }
        self.observe_orbits();
        self.ensure_input();
        let initial_start = self
            .live
            .as_ref()
            .is_some_and(|live| live.initial_start_generation.is_some());
        if !initial_start {
            self.advance_clock_out();
            self.follow_clock_in();
            self.pump_link();
        }
        if self.session.transport().is_stopped() {
            // Stop must reach a followed MIDI device on this turn too.
            if initial_start {
                self.advance_clock_out();
                self.follow_clock_in();
            }
            return Ok(self.advance_graceful_stop(observed_at));
        }

        // A stream error means the device went away during playback: a
        // Bluetooth headset changed profile (which happens when its
        // microphone is opened), an interface was unplugged, or the machine
        // woke up. Try to reopen through the recycle path before ending the
        // set. Every other `check_health` refusal means the machine cannot
        // keep up, and reopening the same device does not help.
        let failed = self
            .live
            .as_ref()
            .expect("checked playback")
            .device
            .output_failed();
        if failed {
            match self.reopen_failed_device(observed_at) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    let error = runtime_device_error(DevicePlaybackError::Unavailable(format!(
                        "audio output {} failed while playing live",
                        self.live.as_ref().expect("checked playback").device.name()
                    )));
                    let _ = self.stop_output(Duration::from_millis(100), false);
                    return Err(error);
                }
            }
        }
        if let Err(error) = self
            .live
            .as_ref()
            .expect("checked playback")
            .device
            .check_health()
        {
            let error = runtime_device_error(error);
            let _ = self.stop_output(Duration::from_millis(100), false);
            return Err(error);
        }

        if let Err(error) = self.recycle_stalled_device(observed_at) {
            let _ = self.stop_output(self.config.stop_timeout, false);
            return Err(error);
        }
        let asset_install_started = Instant::now();
        self.retry_pending_uninstalls();
        self.install_ready_samples();
        self.enforce_sample_memory(Instant::now());
        self.release_retired_samples();
        self.session
            .record_live_asset_preparation(asset_install_started.elapsed());
        let stop_pending = initial_start
            && self
                .live
                .as_ref()
                .expect("checked playback")
                .device
                .stop_pending();
        if stop_pending || self.hold_start(&mut emit) {
            // A held start keeps its output open: previews sound through it
            // as they would through a playing score.
            self.advance_previews();
            emit_pending_diagnostics(&mut self.pending_diagnostics, &mut emit);
            return Ok(StudioTick::Running {
                step: None,
                audible_generation: self
                    .live
                    .as_ref()
                    .expect("checked playback")
                    .device
                    .generation(),
                accepted_audio: 0,
            });
        }
        {
            let live = self.live.as_mut().expect("checked playback");
            anchor_initial_start(
                &mut self.session,
                &mut live.initial_start_generation,
                &live.device,
                self.config.start_preroll,
            );
        }
        // External clock following remains authoritative after the initial
        // anchor. Steady turns keep their earlier clock delivery boundary.
        // Auditions and recording keep their absolute device clocks.
        if initial_start {
            self.advance_clock_out();
            self.follow_clock_in();
        }
        if self.session.transport().is_stopped() {
            self.advance_clock_out();
            return Ok(self.advance_graceful_stop(observed_at));
        }
        self.advance_previews();
        self.retire_finished_audition_transport();
        if self.session.transport().is_stopped() {
            self.advance_clock_out();
            return Ok(self.advance_graceful_stop(observed_at));
        }
        {
            let live = self.live.as_ref().expect("checked playback");
            self.session
                .set_continuity_margin(live.device.continuity_margin_seconds());
        }

        let mut accepted_audio = std::mem::take(&mut self.pending_accepted_audio);
        let device_now = self
            .live
            .as_ref()
            .map_or(0.0, |live| live.device.clock_seconds());
        let step_result = {
            let live = self.live.as_mut().expect("checked playback");
            let device = &live.device;
            let mut external = ExternalOutputs {
                midi: &mut self.midi_outputs,
                hold: &mut self.shield_hold,
            };
            let mut played = PlayedSink {
                by_score: &mut self.played_by_score,
                by_text: &mut self.played_by_text,
                samples: &self.retained_samples,
                library: self.session.sample_library().cloned(),
                use_until: &mut self.sample_use_until,
                sample_rate: device.sample_rate(),
                from: self.played_from_generation,
                text_from: self.text_from_generation,
            };
            let mut reverbs = LiveReverbBatch::default();
            let result = live
                .producer
                .step_unwatched_with_clock_and_cutover_and_preparation(
                    &mut self.session,
                    || device.render_frontier_seconds(),
                    device.sample_rate(),
                    |events| {
                        device
                            .prepare_kabel_events(events)
                            .map_err(RuntimeError::ResourceLimit)
                    },
                    |events| LiveReverbBatch::install_inserts(device, events),
                    |generation, takeover_frame, cut| {
                        // The external outputs retire the old generation's
                        // onsets at the same frame the audio does, so nothing
                        // hangs.
                        external.take_over(
                            device.generation(),
                            generation,
                            takeover_frame,
                            retire_frame(device, takeover_frame),
                            cut,
                            device.sample_rate(),
                        );
                        device.set_generation(generation, takeover_frame, cut);
                    },
                    |event| {
                        reverbs.observe_prepared(device, &event);
                        let pushed = device.push(event.clone());
                        if pushed {
                            played.note(&event, device.render_frontier_frames());
                            accepted_audio.push(UiAcceptedOnset {
                                generation: event.generation,
                                onset_id: event.onset_id,
                                frequency_hz: Some(event.freq_hz),
                                gain: Some(event.gain),
                            });
                        }
                        pushed
                    },
                );
            // Retry pending inserts even when this turn scheduled no batch.
            let preparation_started = Instant::now();
            reverbs.flush(device);
            live.producer
                .record_asset_preparation(preparation_started.elapsed());
            result
        };
        self.sync_recovered_session();
        self.sync_polyphony();
        let trace_started = Instant::now();

        let step_failed = step_result.is_err();
        self.send_pending_osc(device_now, step_failed);
        self.send_pending_midi(device_now, step_failed);
        self.send_pending_serial(device_now, step_failed);
        self.release_shield_hold(device_now);

        let step_succeeded = step_result.is_ok();
        let accepted_audio_count = accepted_audio.len();
        let stopping = self.is_stopping();
        {
            let live = self.live.as_ref().expect("checked playback");
            self.ui.after_step(
                &mut self.session,
                &live.device,
                observed_at,
                step_succeeded,
                stopping,
                accepted_audio,
                &mut self.pending_diagnostics,
                &mut emit,
            );
        }
        self.live
            .as_mut()
            .expect("checked playback")
            .producer
            .complete_turn(trace_started.elapsed());

        self.sync_recovered_session();
        self.sync_played_score();
        self.abandon_rolled_back_landing();
        let audible_generation = self
            .live
            .as_ref()
            .expect("checked playback")
            .device
            .generation();
        let tick = match step_result {
            Ok(step) => {
                self.live
                    .as_mut()
                    .expect("checked playback")
                    .last_step_error = None;
                StudioTick::Running {
                    step: Some(step),
                    audible_generation,
                    accepted_audio: accepted_audio_count,
                }
            }
            Err(RuntimeError::Cancelled) => {
                // A stop that arrived during this turn takes the same
                // graceful path as one observed at the top of the next.
                return Ok(self.advance_graceful_stop(observed_at));
            }
            Err(error) => {
                let message = error.to_string();
                let live = self.live.as_mut().expect("checked playback");
                if live.last_step_error.as_deref() != Some(message.as_str()) {
                    live.last_step_error = Some(message);
                    queue_diagnostic(
                        &mut self.pending_diagnostics,
                        StudioDiagnostic::runtime(&error, true),
                    );
                }
                StudioTick::Running {
                    step: None,
                    audible_generation,
                    accepted_audio: accepted_audio_count,
                }
            }
        };

        self.collect_session_diagnostics();

        emit_pending_diagnostics(&mut self.pending_diagnostics, &mut emit);
        Ok(tick)
    }

    /// One turn of a graceful stop.
    ///
    /// The first turn hushes: an empty generation takes over at the current
    /// frame, which retires every onset that has not started and what the
    /// external outputs owe from that frame. The voices already sounding
    /// finish their envelopes and tails. Later turns watch score sources and
    /// its post-fader peak. Once that bus has settled, retire the score while
    /// preserving any independent keyboard audio.
    fn advance_graceful_stop(&mut self, observed_at: Duration) -> StudioTick {
        let Some(live) = self.live.as_mut() else {
            return StudioTick::Idle;
        };
        if live.draining.is_none() {
            let (hushed, frame) = hush(&live.device);
            let sample_rate = live.device.sample_rate();
            live.draining = Some(StopDrain { silent_since: None });
            self.external_outputs()
                .retire_from(hushed, frame, sample_rate);
        }
        self.advance_piano_releases();
        let live = self.live.as_mut().expect("checked playback");
        let (sources_active, peak) = live.device.take_score_activity();
        let drain = live.draining.as_mut().expect("draining");
        // The score bus alone decides when its tail ends. A held keyboard
        // note neither extends that tail nor changes transport ownership.
        if !sources_active && peak <= GRACEFUL_STOP_SILENCE_PEAK {
            drain.silent_since.get_or_insert(observed_at);
        } else {
            drain.silent_since = None;
        }
        let silent_long_enough = drain
            .silent_since
            .is_some_and(|since| observed_at.saturating_sub(since) >= GRACEFUL_STOP_SILENCE_HOLD);
        if !silent_long_enough {
            return StudioTick::Stopping;
        }
        if !self.piano_close_when_idle || self.piano.iter().any(Option::is_some) {
            // The score finished naturally; the keyboard still owns audio.
            // Keep its voices and the device/recording tap uninterrupted.
            self.detach_stopped_output_for_piano();
            self.close_input();
            self.piano_has_score_tail = false;
            self.piano_tail_silent_since = None;
            self.ui.reset();
            StudioTick::Idle
        } else {
            self.stop(self.config.stop_timeout)
                .map(|stop| StudioTick::Stopped(Box::new(stop)))
                .unwrap_or(StudioTick::Idle)
        }
    }

    /// Stop is idempotent at the Session level and ends the current playback.
    /// A healthy device stays open after an acknowledged Stop or a zero timeout.
    /// A later [`Self::evaluate`] adopts that stream rather than opening a new one.
    /// An idle Linux default output is reopened to resolve its current route.
    ///
    /// This is the immediate form: the device declicks over ten milliseconds
    /// and drains. A user-facing Stop goes through [`Self::request_stop`] and
    /// lets the tail ring out first.
    pub fn stop(&mut self, timeout: Duration) -> Option<StudioStop> {
        self.stop_output(timeout, true)
    }

    fn stop_output(&mut self, timeout: Duration, keep_output: bool) -> Option<StudioStop> {
        // What its last text sounded stays for that text's next start.
        self.end_played();
        self.end_loads();
        if let Some(live) = self.live.as_ref() {
            self.stopped_device_time
                .get_or_insert_with(|| live.device.clock_seconds());
            // The score is no longer kept for being the score, so what the
            // last sweep decided was still wanted is decided again: a tab
            // closed while it played goes with the next idle sweep.
            self.ask_idle_sweep();
        }
        self.piano = [None; PIANO_KEYS];
        self.piano_close_when_idle = true;
        self.piano_has_score_tail = false;
        self.piano_tail_silent_since = None;
        self.session.transport().stop();
        self.session.consume_audio_confirmations();
        self.supersede_launch();
        if let Some(out) = self.clock_out.as_mut() {
            out.advance(false, 0.0, 1.0, Instant::now());
        }
        self.pump_recording();
        self.pump_sample_recording();
        // Piano can retain the callback after a score finishes. Stopping
        // that output needs the same cleanup and
        // acknowledgement as stopping an output owned by the score.
        let mut device = self
            .live
            .take()
            .map(|live| live.device)
            .or_else(|| self.piano_output.take())?;
        // The room ended; a fired rewind is no longer what sounds. The next
        // quantised rewind after a fresh start is a real restart.
        self.recent_rewind = None;
        if let Some(library) = self.session.sample_library() {
            requeue_retained_samples(library, &self.retained_samples);
        }
        device.set_analysis_enabled(false);
        device.set_visual_analysis_mask(0);
        // Every note the ports still hold goes off with the audio; the
        // ports themselves reopen on the next onset that names them.
        self.external_outputs().forget();
        let acknowledged = device.stop_and_wait(timeout);
        let report = device.report();
        // Reuse avoids host clicks from closing and reopening a stream.
        // A zero timeout leaves the stop ramp pending on a healthy callback;
        // an expired positive timeout means the callback cannot be reused.
        if keep_output && device.check_health().is_ok() && (acknowledged || timeout.is_zero()) {
            device.close_input();
            self.piano_output = Some(device);
        } else {
            drop(device);
        }
        // The parked output keeps its rings and pools until the next start
        // adopts them or a device change releases the stream. What the
        // session let go is still owed back.
        self.owe_free_memory();
        self.sync_input_channels();
        // A timed-out callback may still read retiring samples during its
        // stop ramp. Keep their horizons until it becomes silent.
        if acknowledged || self.piano_output.is_none() {
            self.release_every_retiring_sample();
        }
        self.session.consume_audio_confirmations();
        self.ui.reset();
        Some(StudioStop {
            acknowledged: acknowledged && report.stop_acknowledged,
            report,
        })
    }
}

fn output_recovery_cancelled(requested: Option<&DevicePlaybackError>) -> RuntimeError {
    match requested {
        Some(error) => RuntimeError::Audio(format!("{error}; output recovery cancelled")),
        None => RuntimeError::Cancelled,
    }
}

fn arm_output_recovery_after_recycle(
    session: &mut Session,
    producer: &mut LiveFileProducer,
    now: f64,
    sample_rate: u32,
) -> Result<(), RuntimeError> {
    // The replacement output is already open. New decodes must follow its
    // rate even if requery fails; retained PCM keeps its own rate.
    if let Some(library) = session.sample_library() {
        library.set_render_rate(sample_rate);
    }
    // A held start counts its plugin copies at this rate too.
    #[cfg(feature = "vst")]
    session.set_plugin_rate(sample_rate);
    session.consume_audio_confirmations();
    let requery = session.requery_after_output_recycle_at(now);
    session.consume_audio_confirmations();
    if let Some((generation_before, generation_after)) = requery? {
        producer.arm_output_recovery_requery(generation_before, generation_after);
    }
    Ok(())
}

fn requeue_retained_samples(
    library: &rustel_runtime::samples::SampleLibrary,
    retained: &HashMap<SampleId, DecodedSample>,
) {
    // Preserve anything that completed downloading while the device was
    // reopening. The atomic queue operation drops retained PCM for any id
    // that already has a newer publication waiting.
    let mut recovery = Vec::with_capacity(retained.len());
    for (id, decoded) in retained {
        recovery.push((*id, decoded.clone()));
    }
    library.requeue_ready_batch_before_newer(recovery);
}

impl Drop for StudioEngine {
    fn drop(&mut self) {
        self.session.transport().stop();
        // The last note-offs, and the workers behind the ports, before the
        // process goes: a wedged driver gets half a second, not forever.
        {
            // Close the input callbacks before the key epoch advances, or a
            // driver callback racing shutdown repopulates the ring we just
            // cleared.
            drop(self.midi_inputs.take());
            self.session.midi_input_bus().clear_keys();
            self.midi_outputs.shutdown(Duration::from_millis(500));
        }
        // Join the callback before draining the recording tap's final block.
        drop(self.live.take());
        drop(self.piano_output.take());
        let _ = self.stop_recording();
        let _ = self.stop_sample_recording();
    }
}

/// The names a text can make the engine look up, whether the lane is live,
/// muted or commented out: each sound, and the sound under every bank the
/// text names. `s("bd").bank("tr909")` reaches `tr909_bd` as well as `bd`.
/// Each name has the variants the text can play of it, and no others: see
/// [`rustel_runtime::sounds::to_keep`]. A tab naming `recordings:0` keeps
/// take 0, not the take 3 the browser previewed.
fn protected_names(source: &str) -> Vec<(String, Variants)> {
    rustel_runtime::sounds::to_keep(source)
}

/// Every variant of a name: every name once a setup picks variants.
static EVERY_VARIANT: Variants = Variants::All;

/// A name and the variants to keep of it: its own, or every one when a
/// setup picks variants.
fn widened(sound: &(String, Variants), every_variant: bool) -> (&str, &Variants) {
    let variants = if every_variant {
        &EVERY_VARIANT
    } else {
        &sound.1
    };
    (sound.0.as_str(), variants)
}

/// Where the score's events note what they sound: the engine's
/// `played_by_score` and `played_by_text`, each from its first generation.
/// The bank holds 2048 ids, so neither set grows past that however long
/// the set runs.
struct PlayedSink<'a> {
    by_score: &'a mut std::collections::HashSet<SampleId>,
    by_text: &'a mut std::collections::HashSet<SampleId>,
    samples: &'a HashMap<SampleId, DecodedSample>,
    library: Option<Arc<rustel_runtime::samples::SampleLibrary>>,
    use_until: &'a mut HashMap<SampleId, u64>,
    sample_rate: u32,
    /// An earlier generation is a preview the score has been put back
    /// over, playing on to its cutover, and is not the score's.
    from: u64,
    /// An earlier generation is not the current text's.
    text_from: u64,
}

impl PlayedSink<'_> {
    /// Note a score event's sample, if it plays one.
    fn note(&mut self, event: &AudioEvent, render_frontier: u64) {
        note_sample_use(
            event,
            self.samples,
            self.library.as_deref(),
            self.use_until,
            self.sample_rate,
            render_frontier,
        );
        if event.synth.is_some() || event.generation < self.from {
            return;
        }
        // The source-selection order the voice uses: wavetable bodies come
        // out of the sample bank too.
        if let Some(id) = event
            .wavetable
            .map(|table| table.table)
            .or_else(|| event.sample.map(|sample| sample.sample))
        {
            self.by_score.insert(id);
            if event.generation >= self.text_from {
                self.by_text.insert(id);
            }
        }
    }
}

fn note_sample_use(
    event: &AudioEvent,
    samples: &HashMap<SampleId, DecodedSample>,
    library: Option<&rustel_runtime::samples::SampleLibrary>,
    use_until: &mut HashMap<SampleId, u64>,
    sample_rate: u32,
    render_frontier: u64,
) {
    if event.synth.is_some() {
        return;
    }
    let Some(id) = event
        .wavetable
        .map(|table| table.table)
        .or_else(|| event.sample.map(|sample| sample.sample))
    else {
        return;
    };
    // A restart can move a late onset to its callback adoption frame. The
    // frontier read after the successful push bounds that frame: generation
    // publication precedes the push, and each render chunk rechecks it.
    let event = AudioEvent {
        target_frame: event.target_frame.max(render_frontier),
        ..event.clone()
    };
    // A decode can become ready during the query, after this turn's bank
    // installations. Its queued PCM already determines the note's horizon.
    let waiting = library.and_then(|library| library.peek_ready_sample(id));
    for sample in samples.get(&id).into_iter().chain(waiting.as_ref()) {
        let Some((id, until)) = event.sample_end_frame(sample, sample_rate) else {
            continue;
        };
        use_until
            .entry(id)
            .and_modify(|end| *end = (*end).max(until))
            .or_insert(until);
    }
}

impl StudioDeviceInfo {
    fn from_device(
        device: &LiveScalarDevice,
        registry: Arc<rustel_runtime::CapabilityRegistry>,
    ) -> Self {
        Self {
            stream_id: device.stream_id(),
            audio: device.audio_facts(),
            registry,
            allocator_tripwire_armed: true,
        }
    }

    pub(super) fn name(&self) -> &str {
        self.audio.output().device_id()
    }
}

struct StudioPlayback {
    device: LiveScalarDevice,
    registry: Arc<rustel_runtime::CapabilityRegistry>,
    producer: LiveFileProducer,
    /// Only a fresh output may refresh cycle zero, once, before its first
    /// producer attempt. Replacements and recovered outputs keep their place.
    initial_start_generation: Option<u64>,
    pressure_monitor: EnginePressureMonitor,
    started_at: Instant,
    progress_at: Duration,
    progress_clock: u64,
    last_recycle_at: Option<Duration>,
    last_step_error: Option<String>,
    last_fx_reverb_refusals: u64,
    /// Present from the first tick after a Stop until the tail has decayed.
    draining: Option<StopDrain>,
    /// A preview whose sample is still loading, retried each tick.
    audition: Option<PendingAudition>,
    /// A run being played out: its remaining notes, handed to the device
    /// one at a time as their moment comes, so stopping it stops the ones
    /// that have not sounded yet. Queued events cannot be unqueued.
    run: Option<RunningAudition>,
    /// This playback was opened only to host a browser audition. Once that
    /// audition ends, the silent score and its cycle clock can go with it.
    audition_owned: bool,
}

fn anchor_initial_start(
    session: &mut Session,
    pending_generation: &mut Option<u64>,
    device: &LiveScalarDevice,
    preroll: Duration,
) {
    let Some(generation) = pending_generation.take() else {
        return;
    };
    // A reload or control re-query already owns a different timeline. Never
    // move it, or retry this anchor after a producer has submitted onsets.
    if session.generation() != generation || session.transport().is_stopped() {
        return;
    }
    let anchor = device.clock_seconds() + device.schedule_lead_seconds() + preroll.as_secs_f64();
    // The explicit start already reset the query cursor to zero and cleared
    // the previous performance's keys. This only finishes its clock mapping:
    // notes played during input/Hydra preparation belong to the new start.
    session.finish_transport_start_at(anchor);
}

#[derive(Clone, Copy)]
struct PianoVoice {
    releasing: bool,
    release_at: Option<(u64, u64)>,
}

/// A scale being heard: the notes still to play and when the next is due.
#[derive(Clone, Debug)]
struct RunningAudition {
    request: AuditionRequest,
    /// Index of the next note in `request.notes`.
    next: usize,
    /// Device clock second the next note lands on, not the moment it is
    /// handed over. A run is heard as a tempo, so what matters is where
    /// each note sounds, and the device places a note by its own clock.
    due: f64,
}

impl RunningAudition {
    /// The moment the next note lands on, once it is near enough to hand
    /// to the device - `None` while its moment is still ahead, and when
    /// the run has no notes left.
    fn next_onset(&mut self, now: f64) -> Option<f64> {
        // A run whose next note is already past - its sound was still
        // loading, or a tick ran long - must not fire everything it missed
        // at once. Slide what is left of it to start from here, so the
        // rest of the scale keeps its step.
        self.due = self.due.max(now + AUDITION_LEAD_SECS);
        (self.next < self.request.notes.len() && self.due <= now + AUDITION_RUN_LEAD_SECS)
            .then_some(self.due)
    }

    /// That note is the device's now; the one after it is a step later.
    fn sounded(&mut self) {
        self.next += 1;
        self.due += self.request.step_secs.max(0.01);
    }
}

/// What one preview plays: a sound on its own (`bd:3`, the samples
/// browser), or a chord of MIDI notes played on that sound (the chord
/// browser). A sample preview is the no-notes case of the same request,
/// so both take one resolve-and-push path.
#[derive(Clone, Debug)]
struct AuditionRequest {
    sound: String,
    /// MIDI numbers, 60 being middle C. Empty plays the sound itself.
    notes: Vec<f32>,
    gain: f32,
    /// Seconds between one note and the next. Zero is a chord - every
    /// note at once; anything else is a run, each note a step later, which
    /// is how a scale is heard.
    step_secs: f64,
    /// Where in the reserved choke block this request's voices sit. A run
    /// hands its notes over one push at a time, so each push must claim a
    /// different slot or the notes cut each other and the scale comes out
    /// as a single note repeating.
    slot: usize,
    /// The device clock second this request's first voice belongs to.
    /// `None` is "as soon as the device will take it", which is what a
    /// chord or a one-shot wants. A run names the moment instead: its
    /// notes are a step apart from each other, not a step apart from
    /// whenever the tick loop got round to handing each one over.
    at: Option<f64>,
}

struct PendingAudition {
    request: AuditionRequest,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuditionPush {
    Played,
    Loading,
}

impl Clone for PendingAudition {
    fn clone(&self) -> Self {
        Self {
            request: self.request.clone(),
            deadline: self.deadline,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct StopDrain {
    silent_since: Option<Duration>,
}

/// Retire every onset that has not started yet, without touching the voices
/// already sounding: an empty generation takes over at the current frame,
/// exactly as a live reload's cutover would.
/// The visual-analysis slot auditions are tagged into: the top one, which a
/// score would only reach by carrying 64 audio-reactive visuals. The samples
/// browser reads this slot for its preview-only scope.
pub const AUDITION_UI_VISUAL_SLOT: u8 = 63;
/// Every preview is tagged into that slot, so the browser's scope shows the
/// preview alone rather than the whole mix.
const AUDITION_VISUALS: u64 = 1 << AUDITION_UI_VISUAL_SLOT;
/// A choke group holds one voice: a second voice in it silences the first.
/// A browser wants that between previews and not inside a chord, where the
/// notes must ring together: in one shared group a triad sounds as a single
/// note. So each note of a preview takes a group of its own, from a
/// reserved block at the top of the float range where nothing a score
/// writes can collide, and the next preview's note cuts the one that stood
/// in its place.
fn audition_cut_group(slot: usize) -> f32 {
    f32::from_bits(f32::MAX.to_bits() - slot.min(MAX_AUDITION_RUN_NOTES) as u32)
}

fn piano_cut_group(key: usize) -> f32 {
    f32::from_bits(f32::MAX.to_bits() - (MAX_AUDITION_RUN_NOTES + 1 + key) as u32)
}

/// Take the audio over by an empty generation at the current frame, which
/// retires every onset that has not started. Returns the generation it
/// replaced and that frame.
fn hush(device: &LiveScalarDevice) -> (u64, u64) {
    let frame = frame_at(device.clock_seconds(), device.sample_rate());
    let hushed = device.generation();
    device.set_generation(hushed.wrapping_add(1), frame, TakeoverCut::None);
    (hushed, frame)
}

/// How long an externally-scheduled onset waits, from the device clock's
/// now. `Duration::from_secs_f64` panics on a value that is not finite or
/// is beyond `u64::MAX` seconds, and a score's target time reaches here
/// from the scheduler unchecked, so a nonsense target waits no time at all
/// and an absurd one waits the most the CLI's own live loop will.
#[cfg(any(feature = "osc", feature = "serial"))]
fn output_wait(target_time: f64, device_now: f64) -> Duration {
    const MAX_WAIT_SECS: f64 = 3600.0;
    let remaining = target_time - device_now;
    if !remaining.is_finite() {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(remaining.clamp(0.0, MAX_WAIT_SECS))
}

/// The outputs outside the device that follow the score's audio: MIDI's
/// ports and what the [`ShieldHold`] has not sent.
struct ExternalOutputs<'a> {
    midi: &'a mut rustel_runtime::midi_bridge::MidiOutputs,
    hold: &'a mut ShieldHold,
}

impl ExternalOutputs<'_> {
    /// Follow a flip from `previous` to `next` at `takeover_frame`,
    /// retiring what `previous` owes from `retired_from`. Without a `cut`,
    /// a MIDI note of `previous` that sounds from the takeover frame on
    /// stands in for the copy `next` sends of it.
    fn take_over(
        &mut self,
        previous: u64,
        next: u64,
        takeover_frame: u64,
        retired_from: u64,
        cut: TakeoverCut,
        sample_rate: u32,
    ) {
        self.midi
            .take_over_generation_from(previous, next, takeover_frame, retired_from, cut);
        self.hold.cut_from(retired_from, sample_rate);
    }

    /// Retire what the outgoing `generation` owes from `frame`, where its
    /// audio is retired: MIDI's batches that have not started, and what the
    /// hold holds.
    fn retire_from(&mut self, generation: u64, frame: u64, sample_rate: u32) {
        self.midi.prune_generation_from(generation, frame);
        self.hold.cut_from(frame, sample_rate);
    }

    /// Forget everything owed, for a stop, a detach or an output recycle,
    /// where the audio it belonged to is gone.
    fn forget(&mut self) {
        self.midi.reset_after_audio_recycle();
        self.hold.clear();
    }
}

/// Whether the render frontier has passed `line_frame`, so a cut armed
/// there has fired.
fn line_passed(device: &LiveScalarDevice, line_frame: u64) -> bool {
    device.render_frontier_frames() > line_frame
}

/// Where a flip at `takeover_frame` retires the outgoing score's external
/// output: its takeover, or the line of an earlier cut that has fired.
fn retire_frame(device: &LiveScalarDevice, takeover_frame: u64) -> u64 {
    device
        .armed_line_frame()
        .filter(|line| line_passed(device, *line))
        .map_or(takeover_frame, |line| line.min(takeover_frame))
}

/// The device frame nearest `seconds`, and never before the first.
fn frame_at(seconds: f64, sample_rate: u32) -> u64 {
    (seconds * f64::from(sample_rate)).round().max(0.0) as u64
}

fn runtime_device_error(error: DevicePlaybackError) -> RuntimeError {
    match error {
        DevicePlaybackError::Unavailable(message) => RuntimeError::Audio(message),
        DevicePlaybackError::ResourceLimit(message) => RuntimeError::ResourceLimit(message),
        DevicePlaybackError::Cancelled => RuntimeError::Cancelled,
    }
}

/// A launch line, by who has to read it: a routine decision is the log's,
/// a broken promise is the player's (see [`StudioEngine::warn_launch`]).
#[derive(Clone, Debug, PartialEq)]
enum LaunchLine {
    Note(String),
    Warning(String),
}

/// What the refill after a withdrawn line cut says. A refill that runs is
/// routine; one that fails leaves the room the cut already silenced quiet
/// until the score's next window, and the player hears that silence.
fn refill_line(requery: Result<Option<(u64, u64)>, RuntimeError>) -> Option<LaunchLine> {
    match requery {
        Ok(Some((before, after))) => Some(LaunchLine::Note(format!(
            "line cut withdrawn after the line - refilling {before}→{after}"
        ))),
        Ok(None) => None,
        Err(error) => Some(LaunchLine::Warning(format!(
            "line cut withdrawn after the line - refill failed: {error}"
        ))),
    }
}

fn fx_reverb_refusal_diagnostic(refusals: u64, previous: &mut u64) -> Option<StudioDiagnostic> {
    if refusals <= *previous {
        return None;
    }
    *previous = refusals;
    Some(StudioDiagnostic {
        kind: "audio".into(),
        message: "An .FX() reverb could not be prepared within the live audio limits. Affected notes may play dry. Reduce simultaneous reverb sizes or voices.".into(),
        recoverable: true,
        level: StudioDiagnosticLevel::Error,
        alert: None,
    })
}

#[derive(Debug, Default)]
struct PendingDiagnostics {
    queue: VecDeque<StudioDiagnostic>,
    alert_keys: BTreeSet<String>,
}

impl PendingDiagnostics {
    fn push_back(&mut self, diagnostic: StudioDiagnostic) {
        queue_diagnostic(self, diagnostic);
    }

    #[cfg(test)]
    fn clear(&mut self) {
        emit_pending_diagnostics(self, &mut |_| Ok(()));
    }
}

impl std::ops::Deref for PendingDiagnostics {
    type Target = VecDeque<StudioDiagnostic>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

fn queue_diagnostic(pending: &mut PendingDiagnostics, diagnostic: StudioDiagnostic) {
    let key = match &diagnostic.alert {
        Some(DiagnosticAlert::Raise(key)) => {
            // Each admitted alert reserves a recovery slot until UI handoff.
            // At capacity, omit new keys; existing alerts can still recover.
            if !pending.alert_keys.contains(key) {
                if pending.alert_keys.len() >= MAX_PENDING_DIAGNOSTICS {
                    return;
                }
                pending.alert_keys.insert(key.clone());
            }
            Some(key)
        }
        Some(DiagnosticAlert::Resolve(key)) => {
            if !pending.alert_keys.contains(key) {
                return;
            }
            Some(key)
        }
        None => None,
    };
    // Keep the latest MIDI state. Sample warnings retain their log entry.
    if let Some(key) = key.filter(|key| key.starts_with("midi:output:")) {
        pending.queue.retain(|queued| {
            !matches!(
                &queued.alert,
                Some(DiagnosticAlert::Raise(other) | DiagnosticAlert::Resolve(other))
                    if other == key
            )
        });
    }
    if let Some(DiagnosticAlert::Resolve(key)) = &diagnostic.alert
        && pending.iter().any(
            |queued| matches!(&queued.alert, Some(DiagnosticAlert::Resolve(other)) if other == key),
        )
    {
        return;
    }
    if pending.back() == Some(&diagnostic) {
        return;
    }
    if pending.len() >= MAX_PENDING_DIAGNOSTICS {
        if let Some(index) = pending
            .iter()
            .position(|queued| !matches!(&queued.alert, Some(DiagnosticAlert::Resolve(_))))
        {
            pending.queue.remove(index);
        } else {
            return;
        }
    }
    pending.queue.push_back(diagnostic);
}

fn midi_output_diagnostic(
    notice: rustel_runtime::midi_bridge::MidiOutputNotice,
) -> StudioDiagnostic {
    use rustel_runtime::midi_bridge::MidiOutputNotice;
    match notice {
        MidiOutputNotice::OpenFailed { port, message } => {
            StudioDiagnostic::message("midi", message).raising(format!("midi:output:{port}"))
        }
        MidiOutputNotice::Opened { port } => {
            StudioDiagnostic::resolve(format!("midi:output:{port}"))
        }
    }
}

fn emit_pending_diagnostics(
    pending: &mut PendingDiagnostics,
    emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
) {
    while let Some(diagnostic) = pending.queue.pop_front() {
        let resolved = match &diagnostic.alert {
            Some(DiagnosticAlert::Resolve(key)) => Some(key.clone()),
            _ => None,
        };
        match emit(StudioUpdate::Diagnostic(diagnostic)) {
            Ok(()) => {
                if let Some(key) = resolved {
                    pending.alert_keys.remove(&key);
                }
            }
            Err((_, StudioUpdate::Diagnostic(diagnostic))) => {
                pending.queue.push_front(diagnostic);
                break;
            }
            Err(_) => unreachable!("diagnostic handoff returned a different update kind"),
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::wav::test_support::{WriterGate, is_finished, writer_at_return_gate};

    mod fx_reverb {
        use super::*;

        #[test]
        fn a_reverb_refusal_is_reported_once_per_counter_increase() {
            let mut previous = 0;
            assert!(fx_reverb_refusal_diagnostic(0, &mut previous).is_none());

            let diagnostic = fx_reverb_refusal_diagnostic(3, &mut previous).expect("new refusals");
            assert_eq!(previous, 3);
            assert_eq!(diagnostic.kind, "audio");
            assert_eq!(diagnostic.level, StudioDiagnosticLevel::Error);
            assert!(diagnostic.recoverable);
            assert!(diagnostic.message.contains("Affected notes may play dry"));
            assert!(fx_reverb_refusal_diagnostic(3, &mut previous).is_none());

            assert!(fx_reverb_refusal_diagnostic(4, &mut previous).is_some());
            assert_eq!(previous, 4);
            assert!(fx_reverb_refusal_diagnostic(4, &mut previous).is_none());
        }
    }
    mod hap_budget {
        use super::*;

        /// `StudioEngine::new` installs [`STUDIO_QUERY_HAP_BUDGET`] itself: with no
        /// override, a fresh score one hap past it in its first cycle is refused
        /// before install.
        #[test]
        fn constructor_installs_the_studio_query_hap_budget() {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some("silent".into()),
                ..StudioConfig::default()
            })
            .expect("studio");

            let over = format!(r#"s("hh*{}")"#, STUDIO_QUERY_HAP_BUDGET + 1);
            let error = engine
                .session
                .reload_at(&over, false, 0.0)
                .expect_err("a fresh score past the studio hap budget must not start");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert!(
                error
                    .to_string()
                    .contains(&format!("more than {STUDIO_QUERY_HAP_BUDGET} haps")),
                "{error}"
            );
            assert_eq!(engine.generation(), 0);
            assert!(!engine.is_playing());
        }

        #[test]
        fn studio_session_refuses_a_score_above_its_configured_hap_budget_before_install() {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some("silent".into()),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine
                .session
                .set_query_hap_budget(8)
                .expect("small hap budget");

            let error = engine
                .session
                .reload_at(
                    r#"setcpm(138/4); $: s("hh").iter(64).stut(64, 1, 1).echo(64, .001, 1)"#,
                    false,
                    0.0,
                )
                .expect_err("a fresh score above the configured hap budget must not start");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert_eq!(engine.generation(), 0);
            assert!(!engine.is_playing());
        }
    }

    /// The pads are a process-global static (`rustel_core::gamepad`), shared
    /// with every other test in this binary; the tests below own pad slot 0
    /// (nothing else in this crate touches it) and still serialize among
    /// themselves with this, the way the `controllers` tests in `app.rs` do
    /// for the slots they own.
    #[cfg(feature = "gamepad")]
    pub(in crate::engine) static GAMEPAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn constructor_passes_the_score_sample_policy_to_imported_sources() {
        let mut granted = rustel_runtime::samples::ScoreSampleAccess::denied();
        granted.permit_origin("https://samples.example").unwrap();
        for access in [
            rustel_runtime::samples::ScoreSampleAccess::denied(),
            granted,
        ] {
            let engine = StudioEngine::new(StudioConfig {
                session: SessionConfig::default().with_score_sample_access(access.clone()),
                ..StudioConfig::default()
            })
            .expect("studio");
            let library = engine.session.sample_library().expect("sample library");
            assert_eq!(library.import_policy_for_test(), Some(access));
        }
    }

    /// A stopped engine over a library whose `kick`, `pad`, `snare`, `hat`
    /// and `bell` are each decoded and retained, 4 KiB apiece, with the
    /// given preview ceiling and a one-second idle sweep.
    pub(in crate::engine) fn engine_with_five_sounds(preview_budget_bytes: usize) -> StudioEngine {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let library = Arc::new(rustel_runtime::samples::SampleLibrary::empty_without_loading());
        library
                .register_trusted_custom(
                    r#"{"_base":"https://samples.invalid/","kick":"kick.wav","pad":"pad.wav","snare":"snare.wav","hat":"hat.wav","bell":"bell.wav"}"#,
                    None,
                )
                .expect("bank map");
        for (index, name) in ["kick", "pad", "snare", "hat", "bell"]
            .into_iter()
            .enumerate()
        {
            let id = SampleId(70 + index as u32);
            library.remember_ready_for_test(&format!("https://samples.invalid/{name}.wav"), id);
            engine.retain_sample_for_test(
                id,
                DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
            );
        }
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        // Left to follow the process-wide biggest-sound ceiling, the
        // recent tabs' allowance would move under these tests whenever
        // another one in the process moves that, and protection with it.
        engine.recent_tabs_bytes = Some(RECENT_TABS_BYTES);
        engine
    }

    /// What is protected, worked out afresh.
    pub(in crate::engine) fn protected_afresh(
        engine: &mut StudioEngine,
    ) -> std::collections::HashSet<SampleId> {
        engine.protection = None;
        assert!(engine.refresh_protection(Instant::now()));
        engine.protection.as_ref().expect("worked out").ids.clone()
    }

    pub(in crate::engine) fn retained_names(engine: &StudioEngine) -> Vec<&'static str> {
        ["kick", "pad", "snare", "hat", "bell"]
            .into_iter()
            .enumerate()
            .filter(|(index, _)| {
                engine
                    .retained_samples
                    .contains_key(&SampleId(70 + *index as u32))
            })
            .map(|(_, name)| name)
            .collect()
    }

    /// The live material with the kick's tab always kept, the given tabs
    /// by their last visit, and the given count of tabs let go.
    pub(in crate::engine) fn kick_kept_with(recent: &[&str], tabs_closed: u64) -> LiveMaterial {
        LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\")")],
            recent: recent.iter().map(|text| Arc::from(*text)).collect(),
            tabs_closed,
            setups_select_variants: false,
            ..LiveMaterial::default()
        }
    }

    /// A stopped engine over a library whose `recordings` bank holds four
    /// takes, each decoded and retained at 4 KiB under ids 90 to 93 - the
    /// way a browser that previewed every take would leave them.
    pub(in crate::engine) fn engine_with_four_takes() -> StudioEngine {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let library = Arc::new(rustel_runtime::samples::SampleLibrary::empty_without_loading());
        library
                .register_trusted_custom(
                    r#"{"_base":"https://samples.invalid/","recordings":["take0.wav","take1.wav","take2.wav","take3.wav"]}"#,
                    None,
                )
                .expect("bank map");
        for take in 0..4u32 {
            let id = SampleId(90 + take);
            library.remember_ready_for_test(&format!("https://samples.invalid/take{take}.wav"), id);
            engine.retain_sample_for_test(
                id,
                DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
            );
        }
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        engine.recent_tabs_bytes = Some(RECENT_TABS_BYTES);
        engine
    }

    /// Which of the four takes are still retained.
    pub(in crate::engine) fn kept_takes(engine: &StudioEngine) -> Vec<u32> {
        (0..4)
            .filter(|take| engine.retained_samples.contains_key(&SampleId(90 + take)))
            .collect()
    }

    /// The live material with one always-kept tab.
    pub(in crate::engine) fn pinned_tab(text: &str) -> LiveMaterial {
        LiveMaterial {
            pinned: vec![Arc::from(text)],
            ..LiveMaterial::default()
        }
    }

    /// A silent studio playing `s("piano")` from the text `score`, its piano
    /// decoded: answers the sample.
    pub(in crate::engine) fn playing_piano(score: &str) -> (StudioEngine, SampleId) {
        use rustel_core::Value;

        let mut engine = silent_engine_with_pattern(rustel_core::pure(Value::object([
            ("s".into(), Value::Str("piano".into())),
            ("gain".into(), Value::F64(0.1)),
        ])));
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("piano"));
        let sample = library.finish_loading_sample_frames_for_test(4_800);
        engine.session.set_sample_library_for_test(library);
        engine.evaluate(score, false).expect("the score");
        (engine, sample)
    }

    /// Turn the playing engine until its current text has sounded `sample`.
    pub(in crate::engine) fn tick_until_the_text_sounds(
        engine: &mut StudioEngine,
        sample: SampleId,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !engine.played_by_text.contains(&sample) {
            assert!(Instant::now() < deadline, "{sample:?} never sounded");
            engine.tick(|_| Ok(())).expect("schedule the score");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub(crate) struct HeldRecording {
        pub(crate) engine: StudioEngine,
        pub(crate) gate: WriterGate,
    }

    impl HeldRecording {
        pub(crate) fn new(
            path: std::path::PathBuf,
            sample_rate: u32,
            read_only: bool,
            panic_on_release: bool,
        ) -> Self {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .unwrap();
            // Exercise recording ownership, real producer ticks and Stop.
            // This internal fixture does not arm the allocation canary or
            // test guarded startup; studio_live_engine covers that boundary.
            engine.session.evaluate_mini("~").unwrap();
            engine.session.transport().start();
            let device = LiveScalarDevice::start_silent(48_000, engine.generation()).unwrap();
            device.check_health().unwrap();
            engine
                .session
                .bind_audio_confirmations(device.confirmations())
                .expect("bind recording output confirmations");
            engine.session.restart_transport_at(device.clock_seconds());
            engine.live = Some(StudioPlayback {
                registry: Arc::new(rustel_runtime::capability_registry_for_dispatch(
                    device.dispatch(),
                )),
                device,
                producer: LiveFileProducer::unwatched(engine.config.poll_interval).unwrap(),
                initial_start_generation: None,
                pressure_monitor: EnginePressureMonitor::default(),
                started_at: Instant::now(),
                progress_at: Duration::ZERO,
                progress_clock: 0,
                last_recycle_at: None,
                last_step_error: None,
                last_fx_reverb_refusals: 0,
                draining: None,
                audition: None,
                run: None,
                audition_owned: false,
            });
            let (writer, gate) =
                writer_at_return_gate(path, sample_rate, read_only, panic_on_release);
            let capture = engine
                .live
                .as_ref()
                .unwrap()
                .device
                .start_recording_capture()
                .unwrap();
            engine.recording = Some(Recording {
                writer,
                input: Some(RecordingInput {
                    capture,
                    sample_rate: 48_000,
                }),
                sample_rate,
                started: Instant::now(),
                frames: 0,
                dropped: 0,
                tap_dropped_seen: 0,
                scratch: Vec::with_capacity(1 << 16),
            });
            Self { engine, gate }
        }

        pub(crate) fn pump_frame_overflow(&mut self) {
            self.engine.recording.as_mut().unwrap().frames = u64::MAX;
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.engine.is_recording() {
                // The first nonempty tap drain overflows before queue admission.
                self.engine.pump_recording();
                if self.engine.is_recording() {
                    assert!(
                        Instant::now() < deadline,
                        "recording callback supplied no PCM"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let device = &self.engine.live.as_ref().unwrap().device;
            assert!(!device.recording_enabled());
        }

        pub(in crate::engine) fn wait_for_capture_callback(&self) {
            let device = &self.engine.live.as_ref().unwrap().device;
            let deadline = Instant::now() + Duration::from_secs(3);
            // The first completion can belong to a block that checked the
            // tap before it opened. A second advancement follows admission.
            for _ in 0..2 {
                let before = device.clock_frames();
                while device.clock_frames() <= before {
                    assert!(Instant::now() < deadline, "capture received no callback");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }

        pub(crate) fn closing_finished(&self) -> bool {
            self.engine.closing_recording.as_ref().is_some_and(
                |closing| matches!(closing, ClosingRecording::Writer(writer) if is_finished(writer)),
            )
        }

        pub(crate) fn wait_until_held(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while matches!(
                self.engine.closing_recording,
                Some(ClosingRecording::Tap { .. })
            ) {
                self.engine.advance_recording_close();
                assert!(Instant::now() < deadline, "recording tap did not retire");
                std::thread::sleep(Duration::from_millis(1));
            }
            self.gate.wait_until_held();
        }

        pub(crate) fn release_and_wait(&mut self) {
            self.gate.release();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.closing_finished() {
                self.engine.advance_recording_close();
                assert!(Instant::now() < deadline, "writer did not terminate");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    impl Drop for HeldRecording {
        fn drop(&mut self) {
            // An assertion must release the writer before engine destruction
            // reaches its recording fields and their blocking joins.
            self.gate.release();
        }
    }

    #[test]
    fn recoverability_does_not_downgrade_runtime_errors() {
        let diagnostic =
            StudioDiagnostic::runtime(&RuntimeError::ResourceLimit("deadline".into()), true);
        assert!(diagnostic.recoverable);
        assert_eq!(diagnostic.level, StudioDiagnosticLevel::Error);

        assert_eq!(
            StudioDiagnostic::message("notice", "warning").level,
            StudioDiagnosticLevel::Warning
        );
        assert_eq!(
            StudioDiagnostic::info("launch", "waiting").level,
            StudioDiagnosticLevel::Info
        );
        assert_eq!(
            StudioDiagnostic::note("audio", "silent · silent · 48000 Hz · 256 frames").level,
            StudioDiagnosticLevel::Note
        );
    }

    /// Engine log lines do not take the status line. A launch decision is
    /// a `Note` and reads at `Info`. A slider requery repeats several times
    /// a second, so it is a `Trace` and reads at `Debug`.
    #[test]
    fn requery_and_launch_lines_are_log_notes() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.pending_diagnostics.clear();
        engine.requery_for_sliders(Instant::now()).expect("requery");
        let sliders = engine
            .pending_diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == "sliders")
            .expect("a playing engine requeries");
        assert_eq!(sliders.level, StudioDiagnosticLevel::Trace);

        engine.log_launch("rewind fired");
        let launch = engine.pending_diagnostics.back().expect("logged");
        assert_eq!(launch.kind, "launch");
        assert_eq!(launch.level, StudioDiagnosticLevel::Note);
    }

    /// Two launch lines are not commentary: the press was answered, and
    /// then the room did not do what the answer promised. A refill that
    /// fails after a withdrawn line cut leaves the room silent until the
    /// score's next window, and the player is told; a refill that runs is
    /// the log's.
    #[test]
    fn a_failed_refill_after_a_withdrawn_line_warns_the_player() {
        let failed = refill_line(Err(RuntimeError::Message("no generation left".into())));
        assert_eq!(
            failed,
            Some(LaunchLine::Warning(
                "line cut withdrawn after the line - refill failed: no generation left".into()
            ))
        );
        assert!(matches!(
            refill_line(Ok(Some((4, 5)))),
            Some(LaunchLine::Note(message)) if message.contains("refilling 4→5")
        ));
        assert_eq!(refill_line(Ok(None)), None, "nothing to refill");

        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.pending_diagnostics.clear();
        engine.say_launch(failed.expect("a line"));
        let warning = engine.pending_diagnostics.back().expect("queued");
        assert_eq!(warning.kind, "launch");
        assert_eq!(warning.level, StudioDiagnosticLevel::Warning);
    }

    use super::*;

    pub(in crate::engine) fn accepted(update: StudioUpdate) -> StudioUpdateSendResult {
        let _ = update;
        Ok(())
    }

    #[test]
    fn config_refuses_zero_worker_intervals() {
        for mutate in [
            |config: &mut StudioConfig| config.poll_interval = Duration::ZERO,
            |config: &mut StudioConfig| config.ui_audio_interval = Duration::ZERO,
            |config: &mut StudioConfig| config.stop_timeout = Duration::ZERO,
        ] {
            let mut config = StudioConfig::default();
            mutate(&mut config);
            assert!(config.validate().is_err());
        }
    }

    pub(crate) fn silent_engine_for_output_selection() -> StudioEngine {
        silent_engine_with_pattern(rustel_core::silence())
    }

    pub(in crate::engine) fn start_selected_output(engine: &mut StudioEngine) {
        let device = engine.open_output().expect("selected output");
        engine.session.transport().start();
        engine
            .session
            .bind_audio_confirmations(device.confirmations())
            .expect("bind output confirmations");
        engine
            .start_on(device, "silence", false, false)
            .expect("start selected output");
    }

    /// Freeze or release the silent device's clock. A fired launch fires a
    /// head-room (about a tenth of a second) before its line, and a test
    /// that looks at the landing afterwards would otherwise race wall time
    /// for that whole window on a loaded machine.
    pub(crate) fn hold_clock(engine: &StudioEngine, hold: bool) {
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .hold_silent_clock_for_test(hold),
            "the test engine plays through the silent output"
        );
    }

    /// Tick an armed launch until it fires. The clock is frozen around each
    /// tick and stays frozen after the one that fires, so the caller sees
    /// the landing exactly as that tick left it; release it with
    /// [`hold_clock`] before driving time forward.
    pub(in crate::engine) fn tick_until_fired(
        engine: &mut StudioEngine,
    ) -> Result<StudioInstall, RuntimeError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            hold_clock(engine, true);
            engine.tick(accepted).expect("tick");
            if let Some(outcome) = engine.take_launch_outcome() {
                return outcome;
            }
            hold_clock(engine, false);
            assert!(Instant::now() < deadline, "the launch never fired");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Arm a quantised rewind and drive it through its fire. Returns the
    /// install the fired evaluation produced, with the clock held on the
    /// tick that fired (see [`tick_until_fired`]).
    pub(in crate::engine) fn arm_and_fire_launch(
        engine: &mut StudioEngine,
        source: &str,
    ) -> StudioInstall {
        assert!(
            engine
                .arm_launch(source, false, 0.25, true)
                .expect("arm")
                .is_some(),
            "a playing engine arms a quantised launch"
        );
        match tick_until_fired(engine) {
            Ok(install) => install,
            Err(error) => panic!("the launch failed: {error:?}"),
        }
    }

    pub(crate) fn clock_now(engine: &StudioEngine) -> f64 {
        device_of(engine).clock_seconds()
    }

    pub(in crate::engine) fn device_of(engine: &StudioEngine) -> &LiveScalarDevice {
        &engine.live.as_ref().expect("playing").device
    }

    /// Wait (wall time, clock running) until the silent device's clock has
    /// passed `seconds`.
    pub(in crate::engine) fn run_clock_past(engine: &StudioEngine, seconds: f64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while device_of(engine).clock_seconds() <= seconds {
            assert!(Instant::now() < deadline, "the silent clock stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Step the silent clock until `until` holds with the clock frozen, and
    /// leave it frozen there. No tick runs, so the caller sees that moment as
    /// a command handled before the next tick does.
    pub(crate) fn hold_clock_until(engine: &StudioEngine, until: impl Fn(&StudioEngine) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            hold_clock(engine, true);
            if until(engine) {
                return;
            }
            hold_clock(engine, false);
            assert!(
                Instant::now() < deadline,
                "the silent clock never got there"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Whether the armed launch's head-room has opened: the test the next
    /// tick's fire makes.
    pub(crate) fn launch_is_due(engine: &StudioEngine) -> bool {
        let now = clock_now(engine);
        engine.pending_launch.as_ref().is_some_and(|pending| {
            now >= engine.pending_line_time(pending, now) - engine.launch_headroom()
        })
    }

    /// Arm a rewind whose line is `line_from_now` seconds from the held
    /// clock, already inside its head-room so the next tick fires it.
    pub(in crate::engine) fn arm_past_headroom(
        engine: &mut StudioEngine,
        source: &str,
        line_from_now: f64,
    ) {
        run_clock_past(engine, 0.3);
        hold_clock(engine, true);
        let now = device_of(engine).clock_seconds();
        let boundary_time = now + line_from_now;
        engine.pending_launch = Some(PendingLaunch {
            source: source.to_owned(),
            mini: false,
            boundary_cycle: engine.session.cycle_at_time(boundary_time),
            boundary_time,
            rewind: true,
            unit_cycles: 0.25,
            preview: false,
            followed: Vec::new(),
            waited: false,
        });
    }

    /// Drive a fired rewind landing past its line, so it settles into the
    /// grace memory the way the worker's ticks leave it.
    pub(in crate::engine) fn settle_rewind_into_grace(
        engine: &mut StudioEngine,
        source: &str,
    ) -> StudioInstall {
        let install = arm_and_fire_launch(engine, source);
        hold_clock(engine, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.recent_rewind.is_none() {
            assert!(
                Instant::now() < deadline,
                "the landing never settled into the grace memory"
            );
            engine.tick(accepted).expect("tick past the line");
            std::thread::sleep(Duration::from_millis(2));
        }
        install
    }

    pub(in crate::engine) fn silent_engine_with_pattern(
        pattern: rustel_core::Pattern,
    ) -> StudioEngine {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("studio");
        engine.session.set_pattern(pattern).expect("native pattern");
        play_silently(&mut engine);
        engine
    }

    /// Play whatever the engine holds through the silent output, as a
    /// start leaves it.
    pub(in crate::engine) fn play_silently(engine: &mut StudioEngine) {
        engine.session.transport().start();
        // Like HeldRecording, this exercises the internal live state without
        // the startup allocation canary or a physical output.
        let device =
            LiveScalarDevice::start_silent(48_000, engine.generation()).expect("silent output");
        engine
            .session
            .bind_audio_confirmations(device.confirmations())
            .expect("bind silent output confirmations");
        engine.session.restart_transport_at(device.clock_seconds());
        engine.live = Some(StudioPlayback {
            registry: Arc::new(rustel_runtime::capability_registry_for_dispatch(
                device.dispatch(),
            )),
            device,
            producer: LiveFileProducer::unwatched(engine.config.poll_interval).unwrap(),
            initial_start_generation: None,
            pressure_monitor: EnginePressureMonitor::default(),
            started_at: Instant::now(),
            progress_at: Duration::ZERO,
            progress_clock: 0,
            last_recycle_at: None,
            last_step_error: None,
            last_fx_reverb_refusals: 0,
            draining: None,
            audition: None,
            run: None,
            audition_owned: false,
        });
    }

    pub(in crate::engine) fn wait_piano_audio(engine: &mut StudioEngine, voices: u64) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            engine.advance_piano_releases();
            let pressure = engine.audio_device().unwrap().report().realtime_pressure;
            if pressure.active_voices == voices
                && pressure.pending_events == 0
                && !engine.piano.iter().flatten().any(|voice| voice.releasing)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "wanted {voices} voices, got {pressure:?}"
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    #[test]
    fn stop_requests_hide_confirmed_audio_before_the_drain_tick() {
        for through_handle in [false, true] {
            let mut engine = silent_engine_with_pattern(rustel_core::silence());
            let generation = engine.generation();
            assert_eq!(engine.snapshot().confirmed_audio_generation, None);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                engine.tick(|_| Ok(())).expect("silent producer tick");
                if engine.snapshot().confirmed_audio_generation == Some(generation) {
                    break;
                }
                assert!(Instant::now() < deadline, "silent window never confirmed");
                std::thread::sleep(Duration::from_millis(2));
            }
            // The native silent graph has no replay text. Its confirmation
            // came from a real callback and the ordinary producer drain.
            assert_eq!(
                engine.session.confirmed_audio_generation(),
                Some(generation)
            );
            assert!(engine.session.active_source().is_none());

            if through_handle {
                engine.stop_handle().request_stop();
            } else {
                engine.request_stop();
            }
            assert!(engine.session.transport().is_stopped());
            assert!(!engine.is_stopping(), "no drain tick has run yet");
            let stopped = engine.snapshot();
            assert!(stopped.playing, "the output owner still exists");
            assert!(!stopped.stopping);
            assert_eq!(stopped.audible_generation, Some(generation));
            assert_eq!(stopped.confirmed_audio_generation, None);

            assert!(matches!(
                engine.tick(|_| Ok(())).expect("begin graceful drain"),
                StudioTick::Stopping
            ));
            assert_eq!(engine.snapshot().confirmed_audio_generation, None);
            engine
                .stop(engine.config.stop_timeout)
                .expect("stop output");
            assert_eq!(engine.snapshot().confirmed_audio_generation, None);
        }
    }

    pub(in crate::engine) fn wait_for_output_notes(
        engine: &mut StudioEngine,
        accepted_before: u64,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut submitted = false;
        let mut sounded = false;
        loop {
            let tick = engine.tick(|_| Ok(())).expect("healthy Studio tick");
            submitted |=
                matches!(tick, StudioTick::Running { accepted_audio, .. } if accepted_audio > 0);
            let device = &engine.live.as_ref().expect("live output").device;
            let report = device.report();
            sounded |= engine.master.take_levels().peak > 0.0;
            if submitted && sounded && report.accepted_events > accepted_before {
                assert_eq!(report.refused_voices, 0);
                assert_eq!(report.callback_errors, 0);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "restored score did not produce audio"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Wait until the silent device has completed at least `count`
    /// callbacks past `from`. The silent callback runs on its own clock, a
    /// couple of milliseconds a block.
    pub(in crate::engine) fn settle_callbacks(device: &LiveScalarDevice, from: u64, count: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while device.callbacks() < from + count {
            assert!(
                Instant::now() < deadline,
                "the silent device stopped running callbacks"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Pad slot 0's state, reset to disconnected and at rest: the gamepad
    /// activity tests below share it with the rest of the binary and must
    /// hand it back clean, panic or not.
    #[cfg(feature = "gamepad")]
    pub(in crate::engine) fn reset_test_pad_0() -> &'static rustel_core::gamepad::Pad {
        let pad = rustel_core::gamepad::pad(0).expect("pad 0");
        pad.set_connected(false);
        pad
    }

    /// Push filler installs until the device's install ring refuses one.
    pub(in crate::engine) fn fill_install_ring_for_test(device: &LiveScalarDevice) {
        let filler = || DecodedSample::from_parts(48_000, 1, vec![0.0; 256]).expect("pcm");
        for slot in 1024u32..1536 {
            let Err(refused) = device.install_sample(SampleId(slot), filler()) else {
                continue;
            };
            drop(refused);
            return;
        }
        panic!("the install ring never filled");
    }

    #[test]
    fn bounded_channel_returns_update_ownership() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let diagnostic = StudioUpdate::Diagnostic(StudioDiagnostic::message("one", "first"));
        try_send_update(&sender, diagnostic).expect("first update");
        let second = StudioUpdate::Diagnostic(StudioDiagnostic::message("two", "second"));
        let (status, returned) = try_send_update(&sender, second.clone()).expect_err("full queue");
        assert_eq!(status, UiEventSendStatus::DroppedFull);
        assert_eq!(returned, second);
        drop(receiver);
    }
}
