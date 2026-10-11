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

mod loading;
mod recovery;
mod shield_hold;
mod ui_state;
pub use loading::LoadingCue;
pub(crate) use loading::import_alert;
use loading::{Followed, LoadState};
use shield_hold::ShieldHold;
#[cfg(test)]
use ui_state::LayoutDelivery;
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

fn graceful_stop_source_finished(
    levels: MasterLevels,
    active_voices: u64,
    pending_events: u64,
    active_orbit_delays: u64,
) -> bool {
    active_voices == 0
        && pending_events == 0
        && active_orbit_delays == 0
        && levels.peak <= GRACEFUL_STOP_SILENCE_PEAK
}

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
        }
    }
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
    /// Output-only helper: no score producer, transport or cycle ownership.
    piano_output: Option<LiveScalarDevice>,
    piano_close_when_idle: bool,
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

    /// Name of the output currently in use, or the pending preference when
    /// playback has not started yet.
    pub fn output_name(&self) -> Option<String> {
        self.audio_device()
            .map(|device| device.name().to_owned())
            .or_else(|| self.preferred_output.clone())
    }

    /// Move playback onto a named output.
    ///
    /// While stopped this only records the choice; the next evaluation opens
    /// it. While playing the stream is recycled onto the new device and the
    /// scheduling horizon is re-armed exactly as it is after a stall.
    /// If opening fails after teardown, the previous output is tried once.
    /// The requested error is still returned; reopening only arms score refill
    /// and does not promise uninterrupted audio or preserve old effect tails.
    pub fn set_output_device(&mut self, name: &str) -> Result<(), RuntimeError> {
        let transport = self.session.transport();
        self.set_output_device_with(name, |device, selector, samples| {
            device.recycle_output_to_with_samples(selector, samples, || transport.is_stopped())
        })
    }

    fn set_output_device_with(
        &mut self,
        name: &str,
        mut recycle: impl FnMut(
            &mut LiveScalarDevice,
            Option<&str>,
            &[(SampleId, DecodedSample)],
        ) -> Result<(), DevicePlaybackError>,
    ) -> Result<(), RuntimeError> {
        if self.live.is_none() || self.session.transport().is_stopped() {
            self.close_piano_output();
            self.preferred_output = Some(name.to_owned());
            return Ok(());
        }
        self.session.consume_audio_confirmations();
        let samples = self.collect_recycle_samples();
        let live = self.live.as_mut().expect("checked playback");
        let previous = live.device.name().to_owned();
        let requested = recycle(&mut live.device, Some(name), &samples);
        self.session.consume_audio_confirmations();
        let requested_error = match requested {
            Ok(()) => None,
            Err(error) if live.device.has_output() => {
                self.requeue_recycle_samples(samples);
                return Err(runtime_device_error(error));
            }
            Err(error) => Some(error),
        };
        if self.session.transport().is_stopped() {
            let _ = self.stop(self.config.stop_timeout);
            return Err(output_recovery_cancelled(requested_error.as_ref()));
        }
        if let Some(error) = &requested_error {
            // Discovery rejection returned above without touching playback.
            // Teardown lost all voices and queued audio: reopen the exact old
            // output once, then refill through the ordinary recovery path.
            let recovered = recycle(&mut live.device, Some(&previous), &samples).and_then(|()| {
                if live.device.name() != previous {
                    Err(DevicePlaybackError::Unavailable(format!(
                        "reopened output {:?} instead of {previous:?}",
                        live.device.name()
                    )))
                } else {
                    Ok(())
                }
            });
            self.session.consume_audio_confirmations();
            if let Err(recovery) = recovered {
                let message =
                    format!("{error}; could not reopen previous output {previous:?}: {recovery}");
                let _ = self.stop(self.config.stop_timeout);
                return Err(RuntimeError::Audio(message));
            }
        }
        if self.session.transport().is_stopped() {
            let _ = self.stop(self.config.stop_timeout);
            return Err(output_recovery_cancelled(requested_error.as_ref()));
        }
        if let Err(error) = self.rearm_recycled_output() {
            let message = match requested_error {
                Some(requested) => {
                    format!("{requested}; previous output could not refill: {error}")
                }
                None => format!("replacement output could not refill: {error}"),
            };
            let _ = self.stop(self.config.stop_timeout);
            return Err(RuntimeError::Audio(message));
        }
        if self.session.transport().is_stopped() {
            let _ = self.stop(self.config.stop_timeout);
            return Err(output_recovery_cancelled(requested_error.as_ref()));
        }
        if let Some(error) = requested_error {
            return Err(RuntimeError::Audio(format!(
                "{error}; previous output {previous:?} reopened; score refill pending"
            )));
        }
        self.preferred_output = Some(name.to_owned());
        Ok(())
    }

    /// Test hook: see the field of the same name.
    #[cfg(test)]
    pub(crate) fn set_fail_output_recycles_for_test(&mut self, fail: bool) {
        self.fail_output_recycles_for_test = fail;
    }

    /// Adopt an output buffer size, in frames. `None` returns to the
    /// automatic policy. A playing set recycles its output so the choice is
    /// heard now, exactly as a device switch does; stopped or silent, it is
    /// remembered for the next open.
    pub fn set_output_buffer_frames(&mut self, frames: Option<u32>) -> Result<(), RuntimeError> {
        let preference = frames.map_or(rustel_audio::AudioBufferPreference::Auto, |frames| {
            rustel_audio::AudioBufferPreference::Frames(frames)
        });
        preference
            .validate()
            .map_err(|error| RuntimeError::Audio(error.to_string()))?;
        self.config.output_buffer_frames = frames;
        self.close_piano_output();
        if self.live.is_none() || self.session.transport().is_stopped() {
            return Ok(());
        }
        #[cfg(test)]
        if self.fail_output_recycles_for_test {
            return self.set_output_device_with_frames(preference, |device, _, _| {
                device.fail_output_replacement_for_test()
            });
        }
        let transport = self.session.transport();
        self.set_output_device_with_frames(preference, |device, selector, samples| {
            device.recycle_output_to_with_samples(selector, samples, || transport.is_stopped())
        })
    }

    /// The device-switch recycle with one difference: the replacement keeps
    /// the current output and adopts the just-validated preference first, so
    /// every open this recycle makes - the requested one and any recovery
    /// reopen of the previous device - carries the new size.
    fn set_output_device_with_frames(
        &mut self,
        preference: rustel_audio::AudioBufferPreference,
        recycle: impl FnMut(
            &mut LiveScalarDevice,
            Option<&str>,
            &[(SampleId, DecodedSample)],
        ) -> Result<(), DevicePlaybackError>,
    ) -> Result<(), RuntimeError> {
        let Some(name) = self.live.as_ref().map(|live| live.device.name().to_owned()) else {
            return Ok(());
        };
        self.live
            .as_mut()
            .expect("checked playback")
            .device
            .set_buffer_preference(preference);
        // Recycling the output by its name is not choosing it: a set that
        // follows the host default keeps following it, so the preference
        // the device-switch path records is put back afterwards.
        let preferred = self.preferred_output.clone();
        let recycled = self.set_output_device_with(&name, recycle);
        if recycled.is_ok() {
            self.preferred_output = preferred;
        }
        recycled
    }

    fn sync_polyphony(&mut self) {
        self.session
            .set_default_max_polyphony(self.master.max_polyphony());
        let override_voices = self.session.max_polyphony_override();
        self.master
            .max_polyphony_override
            .store(override_voices.unwrap_or(0), Ordering::Relaxed);
        let effective = override_voices.unwrap_or(self.session.config().max_polyphony);
        self.master
            .effective_max_polyphony
            .store(effective, Ordering::Relaxed);
        if let Some(device) = self.audio_device() {
            device.set_max_polyphony(effective);
        }
    }

    fn rearm_recycled_output(&mut self) -> Result<(), RuntimeError> {
        self.piano = [None; PIANO_KEYS];
        self.sync_polyphony();
        // The frame domain the ports' timelines lived in went with the old
        // stream: silence them before the session's generation moves on.
        self.external_outputs().forget();
        let live = self.live.as_mut().expect("recycled playback");
        live.device.set_master_gain(self.master.gain());
        live.device.set_limiter(self.master.limiter());
        live.device.set_limiter_makeup(self.master.limiter_makeup());
        for (orbit, gain) in self.orbit_gains.iter().enumerate() {
            live.device.set_orbit_gain(orbit, *gain);
        }
        live.device.set_input_gain(self.input_gain);
        live.progress_clock = live.device.clock_nanos();
        live.progress_at = live.started_at.elapsed();
        live.last_recycle_at = Some(live.progress_at);
        self.session
            .set_schedule_lead(live.device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(live.device.continuity_margin_seconds());
        arm_output_recovery_after_recycle(
            &mut self.session,
            &mut live.producer,
            live.device.clock_seconds(),
            live.device.sample_rate(),
        )?;
        // Say what opened, as the first open does: a buffer or device
        // change is exactly when the player wants to read what the host
        // granted. A log note, never the status line.
        let facts = live.device.audio_facts().output().describe_one_line();
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::note("audio", facts),
        );
        // The replacement was seeded before callbacks activated. The old
        // bank and queued events are gone; forgotten ids were excluded from
        // that snapshot, so every retiring id can now return to the library.
        self.release_every_retiring_sample();
        Ok(())
    }

    /// Pad notices. Polling is a log note, so it does not toast.
    /// `watch_pads: false` leaves the process-global queue alone.
    fn collect_gamepad_notices(&mut self) {
        if !self.config.watch_pads {
            return;
        }
        for notice in rustel_core::gamepad::take_notices() {
            let diagnostic = match notice {
                rustel_core::gamepad::Notice::Polling => {
                    StudioDiagnostic::note("gamepad", "gamepads: watching for pads")
                }
                rustel_core::gamepad::Notice::Connected { pad, name } => {
                    // A pad that went while a button was held leaves this
                    // slot's last-seen state stuck at that value; a fresh
                    // connect really does start at rest (`set_connected`
                    // clears it on the poller side), so the baseline is
                    // reset here too, or the first real read afterwards
                    // would misread the snap back to rest as a release.
                    #[cfg(feature = "gamepad")]
                    {
                        if let Some(seen) = self.gamepad_buttons_seen.get_mut(pad) {
                            *seen = [0.0; rustel_core::gamepad::BUTTONS];
                        }
                        if let Some(logged) = self.gamepad_axes_logged.get_mut(pad) {
                            *logged = [None; rustel_core::gamepad::AXES];
                        }
                    }
                    StudioDiagnostic::info("gamepad", format!("gamepad({pad}) connected: {name}"))
                }
                rustel_core::gamepad::Notice::Disconnected { pad, name } => StudioDiagnostic::info(
                    "gamepad",
                    format!("gamepad({pad}) disconnected: {name}"),
                ),
                rustel_core::gamepad::Notice::Problem(problem) => {
                    StudioDiagnostic::message("gamepad", problem)
                }
            };
            self.pending_diagnostics.push_back(diagnostic);
        }
        #[cfg(feature = "gamepad")]
        self.collect_gamepad_activity();
    }

    /// A pad's buttons and sticks, from turn to turn. A press or a release
    /// is a discrete event, like a MIDI key, and reaches the log at a level
    /// a reader sees. A stick position is not discrete: a sweep across its
    /// throw changes it on nearly every engine turn. So a stick reaches the
    /// log as [`StudioDiagnostic::trace`], and only after it moves past
    /// [`GAMEPAD_AXIS_LOG_THRESHOLD`] from the last value logged for it.
    /// `studio.log` records every line, `Debug` lines included, so the
    /// threshold keeps a held stick from filling the file. Both kinds also
    /// reach the mixer's feed beside MIDI.
    #[cfg(feature = "gamepad")]
    fn collect_gamepad_activity(&mut self) {
        for (slot, name) in rustel_core::gamepad::connected_pads() {
            let Some(pad) = rustel_core::gamepad::pad(slot) else {
                continue;
            };
            if let Some(seen) = self.gamepad_buttons_seen.get_mut(slot) {
                for (index, was) in seen.iter_mut().enumerate() {
                    let now = pad.button(index) as f32;
                    let verb = if *was < 0.5 && now >= 0.5 {
                        Some("pressed")
                    } else if *was >= 0.5 && now < 0.5 {
                        Some("released")
                    } else {
                        None
                    };
                    if let Some(verb) = verb {
                        let line = format!(
                            "gamepad({slot}) {name}: {} {verb}",
                            rustel_runtime::gamepad::slot_name(index)
                        );
                        rustel_core::gamepad::note_activity(line.clone());
                        queue_diagnostic(
                            &mut self.pending_diagnostics,
                            StudioDiagnostic::note("gamepad-activity", line),
                        );
                    }
                    *was = now;
                }
            }
            if let Some(logged) = self.gamepad_axes_logged.get_mut(slot) {
                for (axis, last) in logged.iter_mut().enumerate() {
                    let now = pad.axis(axis) as f32;
                    let moved_enough = match *last {
                        Some(previous) => (now - previous).abs() >= GAMEPAD_AXIS_LOG_THRESHOLD,
                        // Centre is not itself news: a stick at rest reads
                        // 0.0 and must not write a line the moment it is
                        // first seen.
                        None => now.abs() >= GAMEPAD_AXIS_LOG_THRESHOLD,
                    };
                    if moved_enough {
                        let line = format!(
                            "gamepad({slot}) {name}: {} {now:+.2}",
                            rustel_runtime::gamepad::AXIS_NAMES[axis]
                        );
                        rustel_core::gamepad::note_activity(line.clone());
                        queue_diagnostic(
                            &mut self.pending_diagnostics,
                            StudioDiagnostic::trace("gamepad-activity", line),
                        );
                        *last = Some(now);
                    }
                }
            }
        }
    }

    /// The mixer's orbit fader: stored, and on the live device at once.
    pub fn set_orbit_gain(&mut self, orbit: usize, gain: f32) {
        if let Some(slot) = self.orbit_gains.get_mut(orbit) {
            *slot = gain;
        }
        if let Some(live) = self.live.as_ref() {
            live.device.set_orbit_gain(orbit, gain);
        }
    }

    /// The audio input's fader: stored, and on the live device at once.
    pub fn set_input_gain(&mut self, gain: f32) {
        self.input_gain = gain;
        if let Some(live) = self.live.as_ref() {
            live.device.set_input_gain(gain);
        }
        if let Some(monitor) = self.input_monitor.as_ref() {
            monitor.set_gain(gain);
        }
    }

    /// Every fader onto a device: a new or recycled one opens at unity
    /// and has to be told what the mixer says.
    fn apply_mix_gains(&self, device: &LiveScalarDevice) {
        for (orbit, gain) in self.orbit_gains.iter().enumerate() {
            if device.orbit_gain(orbit) != *gain {
                device.set_orbit_gain(orbit, *gain);
            }
        }
        if device.input_gain() != self.input_gain {
            device.set_input_gain(self.input_gain);
        }
    }

    /// Choose the audio input `s("in")` plays, or none. Opened on the live
    /// device at once when there is one, and again on every device that
    /// follows.
    /// Which notes the set has bound to scene launch. Forwarded to every
    /// live MIDI-input port, where the driver thread reads the set on each
    /// NoteOn: a launch pad's press never enters the musical ring, so the
    /// press-count watcher sees nothing and arms no requery - the launch
    /// lands alone, the way ctrl+enter does.
    pub fn set_launch_pads(&mut self, pads: Vec<(u8, u8)>) {
        // The bus keeps the set and stamps it onto every port it creates or
        // publishes later, so a keyboard a score names after this send is
        // born knowing its transport buttons.
        self.session.midi_input_bus().set_launch_pads(&pads);
        self.launch_pads = pads;
    }

    /// Choose the audio input that `s("in")` plays, or none. The next turn
    /// opens it, and the choice is kept across device switches.
    pub fn set_input_device(&mut self, name: Option<&str>) {
        let changed = self.preferred_input.as_deref() != name;
        self.preferred_input = name.map(str::to_owned);
        self.input_refused = None;
        self.input_attempts = 0;
        self.input_retry_at = Instant::now();
        // A different choice lets the old input go, wherever it was open:
        // the next turn opens the new one. (It used to stay open until the
        // device was rebuilt, so choosing another microphone did nothing.)
        if changed {
            self.close_input();
        }
    }

    /// Whether opening the input for `wanted` is news: the first time for
    /// this choice, yes; the handoff between the monitor and the live
    /// device, no - the microphone was open and metering all along.
    fn announce_input(&mut self, wanted: &str) -> bool {
        if self.input_announced.as_deref() == Some(wanted) {
            return false;
        }
        self.input_announced = Some(wanted.to_owned());
        true
    }

    /// Let go of the input wherever it is open.
    fn close_input(&mut self) {
        if let Some(device) = self
            .live
            .as_mut()
            .map(|live| &mut live.device)
            .or(self.piano_output.as_mut())
        {
            device.close_input();
        }
        self.input_monitor = None;
        self.input_opened_for = None;
        self.sync_input_channels();
    }

    /// Only a healthy, open stream proves which channels are available. A
    /// disconnected or unopened input keeps the ordinary silent-source policy.
    fn input_channels(&self) -> usize {
        if let Some(device) = self.audio_device()
            && device.input_name().is_some()
            && !device.input_failed()
        {
            return device.input_channels();
        }
        self.input_monitor
            .as_ref()
            .filter(|input| !input.failed())
            .map_or(0, |input| input.channels())
    }

    fn sync_input_channels(&mut self) {
        let channels = self.input_channels();
        self.session
            .set_audio_input_channels((channels > 0).then_some(channels));
    }

    /// Keep the chosen input open. Delay retries after opening or stream failures.
    fn ensure_input(&mut self) {
        self.ensure_input_with(
            Instant::now(),
            |device, monitor| match device {
                Some(device) => (device.input_name().is_some(), device.input_failed()),
                None => (
                    monitor.is_some(),
                    monitor.is_some_and(rustel_audio::AudioInput::failed),
                ),
            },
            Self::open_selected_input,
        );
    }

    fn ensure_input_with(
        &mut self,
        now: Instant,
        status: impl FnOnce(
            Option<&LiveScalarDevice>,
            Option<&rustel_audio::AudioInput>,
        ) -> (bool, bool),
        open_input: impl FnOnce(&mut Self, &str) -> Result<(String, usize), DevicePlaybackError>,
    ) {
        self.sync_input_channels();
        let Some(wanted) = self.preferred_input.clone() else {
            return;
        };
        let input_device = self.live.as_ref().map(|live| &live.device).or(self
            .piano_output
            .as_ref()
            .filter(|_| self.piano_has_score_tail));
        let (open, failed) = status(input_device, self.input_monitor.as_ref());
        if open && !failed && self.input_opened_for.as_deref() == Some(wanted.as_str()) {
            self.input_refused = None;
            self.input_attempts = 0;
            return;
        }
        let refused_before = self.input_refused.as_deref() == Some(&wanted);
        if open {
            self.close_input();
            if failed {
                self.defer_input_retry(wanted, now);
                if !refused_before {
                    self.pending_diagnostics
                        .push_back(StudioDiagnostic::message(
                            "input",
                            "audio in: the input stopped - will try again",
                        ));
                }
                return;
            }
        }
        // A new choice opens at once. A failed input waits for its retry.
        if refused_before && now < self.input_retry_at {
            return;
        }
        match open_input(self, &wanted) {
            Ok((name, channels)) => {
                // A later healthy observation clears the failure episode.
                if self.announce_input(&wanted) {
                    self.pending_diagnostics.push_back(StudioDiagnostic::info(
                        "input",
                        format!(
                            "audio in: {name} - {channels} channel(s); s(\"in\") plays channel 0, in:1 the next"
                        ),
                    ));
                }
                self.input_opened_for = Some(wanted);
            }
            Err(error) => {
                self.defer_input_retry(wanted, now);
                if !refused_before {
                    self.pending_diagnostics
                        .push_back(StudioDiagnostic::message(
                            "input",
                            format!("audio in: {error} - will keep trying"),
                        ));
                }
            }
        }
        self.sync_input_channels();
    }

    fn defer_input_retry(&mut self, wanted: String, now: Instant) {
        self.input_attempts = self.input_attempts.saturating_add(1);
        self.input_retry_at = now
            .checked_add(rustel_audio::input_retry_delay(self.input_attempts))
            .unwrap_or(now);
        self.input_refused = Some(wanted);
        self.input_announced = None;
    }

    fn open_selected_input(
        &mut self,
        wanted: &str,
    ) -> Result<(String, usize), DevicePlaybackError> {
        let input_device = self.live.as_mut().map(|live| &mut live.device).or(self
            .piano_output
            .as_mut()
            .filter(|_| self.piano_has_score_tail));
        match input_device {
            Some(device) => device
                .open_input(Some(wanted))
                .map(|name| (name, device.input_channels())),
            None => rustel_audio::AudioInput::open(
                Some(wanted),
                std::sync::Arc::new(rustel_audio::input::InputRing::new()),
                None,
            )
            .map(|input| {
                input.set_gain(self.input_gain);
                let facts = (input.name().to_owned(), input.channels());
                self.input_monitor = Some(input);
                facts
            }),
        }
    }

    /// Fetch the map of a `samples("…")` a score names, ahead of the
    /// score's evaluation, under the session's own grant.
    pub fn look_up_samples(&mut self, spec: &str) -> Result<(), RuntimeError> {
        self.session.look_up_samples_source(spec)
    }

    /// Play one sound once, outside the score: `bd:2` from the browser.
    ///
    /// The sound is resolved through the same voice path the scheduler
    /// uses and pushed straight into the device ring on the device's
    /// current generation, so the playing score is untouched. A sample
    /// that is still loading is retried for a few seconds. With nothing
    /// playing, a silent score is started first so there is a device to
    /// preview through.
    pub fn audition(&mut self, sound: &str, gain: f32) -> Result<(), RuntimeError> {
        self.audition_request(AuditionRequest {
            sound: sound.to_owned(),
            notes: Vec::new(),
            gain,
            step_secs: 0.0,
            slot: 0,
            at: None,
        })
    }

    /// Play notes one after another, `step_secs` apart, outside the score:
    /// a scale as it is heard, not as a cluster. At most
    /// [`MAX_AUDITION_RUN_NOTES`] notes sound; the rest are dropped.
    pub fn audition_run(
        &mut self,
        notes: &[f32],
        sound: &str,
        gain: f32,
        step_secs: f64,
    ) -> Result<(), RuntimeError> {
        let notes = notes
            .iter()
            .copied()
            .filter(|note| note.is_finite())
            .take(MAX_AUDITION_RUN_NOTES)
            .collect::<Vec<_>>();
        if notes.is_empty() {
            return Ok(());
        }
        if self.is_stopping() {
            let _ = self.stop(self.config.stop_timeout);
        }
        let opened_for_audition = self.live.is_none();
        if opened_for_audition {
            self.start_silence()?;
            if let Some(live) = self.live.as_mut() {
                live.audition_owned = true;
            }
        }
        // The first note now, the rest as their moments come: a run that
        // handed the device all of its notes at once could not be stopped
        // partway, because a queued event cannot be unqueued.
        let request = AuditionRequest {
            sound: sound.to_owned(),
            notes,
            gain,
            step_secs: step_secs.clamp(0.0, 1.0),
            slot: 0,
            at: None,
        };
        self.stop_audition();
        self.audition_horizon_frame = 0;
        self.note_preview();
        // When the first note sounds, not when it is handed over. Each
        // later note is one whole step after this time, so the run keeps
        // its tempo. A run timed from `now` plays its first note at
        // `now + lead` and its second one step after `now`, so the first
        // gap is one lead too short.
        let due = self
            .live
            .as_ref()
            .map(|live| live.device.clock_seconds() + AUDITION_LEAD_SECS)
            .unwrap_or_default();
        if let Some(live) = self.live.as_mut() {
            live.run = Some(RunningAudition {
                request,
                next: 0,
                due,
            });
        }
        self.advance_run();
        Ok(())
    }

    /// Hand the device every note of the run whose moment has come.
    fn advance_run(&mut self) {
        let Some(mut run) = self.live.as_ref().and_then(|live| live.run.clone()) else {
            return;
        };
        let now = match self.live.as_ref() {
            Some(live) => live.device.clock_seconds(),
            None => return,
        };
        while let Some(at) = run.next_onset(now) {
            let note = run.request.notes[run.next];
            let one = AuditionRequest {
                sound: run.request.sound.clone(),
                notes: vec![note],
                gain: run.request.gain,
                // One note at a time: the run's step decides its length,
                // not a chord's.
                step_secs: run.request.step_secs,
                // Its own slot, so it rings over the note before it the
                // way a hand does not lift off a piano between notes.
                slot: run.next % MAX_AUDITION_RUN_NOTES,
                // The moment this note belongs to, not the moment the tick
                // loop happened to hand it over.
                at: Some(at),
            };
            match self.push_audition(&one) {
                Ok(AuditionPush::Played) => {}
                Ok(AuditionPush::Loading) => break,
                Err(error) => {
                    queue_diagnostic(
                        &mut self.pending_diagnostics,
                        StudioDiagnostic::message("preview", error.to_string()),
                    );
                    if let Some(live) = self.live.as_mut() {
                        live.run = None;
                    }
                    return;
                }
            }
            run.sounded();
        }
        if let Some(live) = self.live.as_mut() {
            live.run = if run.next >= run.request.notes.len() {
                None
            } else {
                Some(run)
            };
        }
    }

    /// Play several notes at once, outside the score: a chord from the
    /// browser, `[60, 64, 67]` on `piano`.
    ///
    /// Each note is resolved as its own voice on `sound`: a bank picks the
    /// sample and the rate for that pitch, and a synth name plays the pitch
    /// directly. All notes are pushed at one target time, so they sound
    /// together. Each note takes its own choke group from the preview's
    /// block, so the notes ring together and the next preview cuts only the
    /// note it replaces. [`Self::stop_audition`] silences all of them. At
    /// most [`MAX_AUDITION_NOTES`] notes sound; the rest are dropped. With
    /// no notes this is [`Self::audition`].
    pub fn audition_notes(
        &mut self,
        notes: &[f32],
        sound: &str,
        gain: f32,
    ) -> Result<(), RuntimeError> {
        let notes = notes
            .iter()
            .copied()
            .filter(|note| note.is_finite())
            .take(MAX_AUDITION_NOTES)
            .collect::<Vec<_>>();
        self.audition_request(AuditionRequest {
            sound: sound.to_owned(),
            notes,
            gain,
            step_secs: 0.0,
            slot: 0,
            at: None,
        })
    }

    pub fn set_piano_settings(&mut self, sound: super::PianoSound, volume: u16) {
        self.piano_sound = sound;
        self.piano_volume = volume.min(super::piano::MAX_PIANO_VOLUME);
    }

    /// Open keyboard audio when F12 opens, without loading samples or touching transport.
    pub fn prepare_piano(&mut self) -> Result<(), RuntimeError> {
        self.open_piano_output()?;
        self.piano_close_when_idle = false;
        Ok(())
    }

    fn open_piano_output(&mut self) -> Result<(), RuntimeError> {
        if self.audio_device().is_none() {
            let mut device = self.open_output().map_err(runtime_device_error)?;
            device
                .arm_callback_tripwire_checked()
                .map_err(|error| RuntimeError::Audio(error.to_string()))?;
            device.set_master_gain(self.master.gain());
            device.set_limiter(self.master.limiter());
            device.set_limiter_makeup(self.master.limiter_makeup());
            device.set_max_polyphony(self.session.max_polyphony());
            self.apply_mix_gains(&device);
            if let Some(library) = self.session.sample_library() {
                requeue_retained_samples(library, &self.retained_samples);
            }
            self.piano_output = Some(device);
        }
        Ok(())
    }

    /// Press one physical computer-piano key. Repeats do not retrigger it;
    /// the slot, rather than pitch, owns release across octave changes.
    pub fn piano_note_on(&mut self, key: u8, note: u8, velocity: u8) -> Result<(), RuntimeError> {
        let key = usize::from(key);
        if key >= PIANO_KEYS || note > 127 {
            return Ok(());
        }
        if velocity == 0 {
            self.piano_note_off(key as u8);
            return Ok(());
        }
        if self.piano[key].is_some_and(|voice| !voice.releasing) {
            return Ok(());
        }
        self.open_piano_output()?;
        let event = self.piano_event(key, note, velocity)?;
        if !self
            .audio_device()
            .expect("piano output")
            .push_immediate(event)
        {
            return Err(RuntimeError::Message(
                "audio busy; try the piano key again".into(),
            ));
        }
        self.piano_close_when_idle = false;
        self.piano[key] = Some(PianoVoice {
            releasing: false,
            release_at: None,
        });
        self.note_preview();
        Ok(())
    }

    fn piano_event(&self, key: usize, note: u8, velocity: u8) -> Result<AudioEvent, RuntimeError> {
        let device = self.audio_device().expect("piano output started");
        // Resolution needs a time base, but callback admission chooses the
        // actual onset. No tempo quantisation or score lookahead is involved.
        // Bypass the session library so even a bank named "triangle" cannot
        // replace the oscillator or delay a keypress with sample loading.
        let mut onset = rustel_voice::resolve_voice(
            &serde_json::json!({"s":self.piano_sound.key(),"note":note}),
            u64::MAX,
            1.0,
            0.0,
            device.sample_rate(),
            self.session.cps(),
        )
        .map_err(RuntimeError::Message)?;
        // This simple oscillator has no modulators or time-dependent FX.
        // Its envelope stays in sustain until its key's choke arrives;
        // this is a held gate, not a periodically retriggered preview.
        onset.duration_secs = f32::INFINITY;
        onset.controls.lfo_end_secs = f32::INFINITY;
        onset.controls.filter_lfo_end_secs = f32::INFINITY;
        onset.controls.envelope = rustel_audio::Envelope {
            attack_secs: 0.005,
            decay_secs: 0.1,
            sustain: 0.7,
            release_secs: 0.02,
        };
        onset.controls.piano = true;
        Ok(AudioEvent {
            onset_id: u64::MAX,
            generation: device.generation(),
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: onset.freq_hz,
            gain: onset.gain
                * (f32::from(velocity.min(127)) / 127.0)
                * 0.3
                * f32::from(self.piano_volume)
                / 100.0,
            duration_secs: onset.duration_secs,
            // It must not enter the browser waveform or generator ownership.
            ui_visuals: 0,
            controls: onset.controls,
            sample: onset.sample,
            wavetable: onset.wavetable,
            synth: onset.synth,
            cut: Some(piano_cut_group(key)),
        })
    }

    /// The sole output, whether a score is playing or the keyboard is idle.
    fn audio_device(&self) -> Option<&LiveScalarDevice> {
        self.live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
    }

    pub fn piano_note_off(&mut self, key: u8) {
        if let Some(voice) = self
            .piano
            .get_mut(usize::from(key))
            .and_then(Option::as_mut)
        {
            voice.releasing = true;
        }
        self.advance_piano_releases();
    }

    pub fn stop_piano(&mut self) {
        self.piano_close_when_idle = true;
        for voice in self.piano.iter_mut().flatten() {
            voice.releasing = true;
        }
        self.advance_piano_releases();
    }

    /// Retry callback queue backpressure; score generations cannot cancel a
    /// direct note or its release.
    fn advance_piano_releases(&mut self) {
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            self.piano = [None; PIANO_KEYS];
            return;
        };
        let now = device.clock_frames();
        let rate = f64::from(device.sample_rate());
        for (key, slot) in self.piano.iter_mut().enumerate() {
            let Some(voice) = slot.as_mut().filter(|voice| voice.releasing) else {
                continue;
            };
            if let Some((_, frame)) = voice.release_at {
                if now > frame.saturating_add((PIANO_RELEASE_SECS * rate).ceil() as u64) {
                    *slot = None;
                }
                continue;
            }
            // At most 64 commands are admitted, one frame apart; include that
            // bounded queue time before considering the release complete.
            let frame = (device.render_frontier_seconds() * rate).ceil() as u64 + 64;
            if device.push_immediate(AudioEvent {
                onset_id: u64::MAX,
                generation: device.generation(),
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.0,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: rustel_audio::OscillatorControls {
                    choke_only: true,
                    piano: true,
                    ..Default::default()
                },
                sample: None,
                wavetable: None,
                synth: None,
                cut: Some(piano_cut_group(key)),
            }) {
                voice.release_at = Some((0, frame));
            }
        }
    }

    /// Retire only transport ownership. The callback, microphone and capture
    /// handle stay intact so already-sounding audio finishes unchanged.
    fn detach_stopped_output_for_piano(&mut self) {
        let Some(live) = self.live.take() else { return };
        // As for a stop: what its last text sounded is kept for its tab.
        self.end_played();
        self.stopped_device_time
            .get_or_insert_with(|| live.device.clock_seconds());
        // As for a stop: the score is no longer kept for being the score.
        self.idle_swept = false;
        self.session.transport().stop();
        self.session.consume_audio_confirmations();
        self.supersede_launch();
        self.recent_rewind = None;
        if let Some(out) = self.clock_out.as_mut() {
            out.advance(false, 0.0, 1.0, Instant::now());
        }
        self.external_outputs().forget();
        hush(&live.device);
        self.piano_output = Some(live.device);
        self.piano_has_score_tail = true;
        self.piano_tail_silent_since = None;
    }

    fn close_piano_output(&mut self) {
        let Some(device) = self.piano_output.take() else {
            return;
        };
        device.stop_and_wait(self.config.stop_timeout);
        drop(device);
        self.piano = [None; PIANO_KEYS];
        self.piano_has_score_tail = false;
        self.piano_tail_silent_since = None;
        if let Some(library) = self.session.sample_library() {
            requeue_retained_samples(library, &self.retained_samples);
        }
        self.release_every_retiring_sample();
    }

    fn service_idle_piano(&mut self) {
        self.install_ready_samples();
        self.advance_piano_releases();
        if let Some(device) = self.piano_output.as_ref() {
            device.set_master_gain(self.master.gain());
            device.set_limiter(self.master.limiter());
            device.set_limiter_makeup(self.master.limiter_makeup());
            self.apply_mix_gains(device);
            let levels = device.take_levels();
            self.master.publish(levels);
            let closing = self.piano_close_when_idle && self.piano.iter().all(Option::is_none);
            let tail_done = if closing && self.piano_has_score_tail {
                let pressure = device.report().realtime_pressure;
                if graceful_stop_source_finished(
                    levels,
                    pressure.active_voices,
                    pressure.pending_events,
                    pressure.active_orbit_delays,
                ) {
                    self.piano_tail_silent_since
                        .get_or_insert_with(Instant::now)
                        .elapsed()
                        >= GRACEFUL_STOP_SILENCE_HOLD
                } else {
                    self.piano_tail_silent_since = None;
                    false
                }
            } else {
                !self.piano_has_score_tail
            };
            if device.output_failed() || (closing && tail_done) {
                self.close_piano_output();
            }
        }
    }

    /// The one path both previews take: start something to preview
    /// through if nothing is playing, push, and retry a sound that is
    /// still downloading for a few seconds.
    fn audition_request(&mut self, request: AuditionRequest) -> Result<(), RuntimeError> {
        if self.is_stopping() {
            let _ = self.stop(self.config.stop_timeout);
        }
        let opened_for_audition = self.live.is_none();
        if opened_for_audition {
            self.start_silence()?;
            if let Some(live) = self.live.as_mut() {
                live.audition_owned = true;
            }
        }
        // A new preview chokes the last one. Its own end, rather than the
        // old sound's longer tail, decides when an audition-only transport
        // can be retired.
        self.audition_horizon_frame = 0;
        self.note_preview();
        match self.push_audition(&request)? {
            AuditionPush::Played => {
                if let Some(live) = self.live.as_mut() {
                    live.audition = None;
                }
                Ok(())
            }
            AuditionPush::Loading => {
                if let Some(live) = self.live.as_mut() {
                    live.audition = Some(PendingAudition {
                        request,
                        deadline: Instant::now() + AUDITION_LOAD_TIMEOUT,
                    });
                }
                Ok(())
            }
        }
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

    /// Silence whatever the browser is previewing, leaving the score alone:
    /// a zero-gain member of the audition choke group, which chokes the
    /// sounding preview exactly as the next preview would and is itself
    /// inaudible. Nothing to do when nothing is live.
    pub fn stop_audition(&mut self) {
        if let Some(live) = self.live.as_mut() {
            live.audition = None;
            // A run's remaining notes are the engine's, not the device's:
            // dropping it is what makes a scale stoppable partway.
            live.run = None;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let target_time = live.device.clock_seconds() + AUDITION_LEAD_SECS;
        let target_frame = frame_at(target_time, live.device.sample_rate());
        // An explicit stop makes the choke itself the preview horizon. The
        // next engine turn may then retire a transport that exists only for
        // this audition instead of waiting out the sound's original length.
        self.audition_horizon_frame = target_frame;
        // Every note of the widest preview has its own group, so silence
        // takes one choke each: a chord must not keep ringing because
        // only its first note was told to stop.
        for slot in 0..=MAX_AUDITION_RUN_NOTES {
            let _ = live.device.push(AudioEvent {
                onset_id: u64::MAX,
                generation: live.device.generation(),
                target_frame,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.0,
                duration_secs: 0.01,
                ui_visuals: AUDITION_VISUALS,
                controls: Default::default(),
                sample: None,
                wavetable: None,
                synth: None,
                cut: Some(audition_cut_group(slot)),
            });
        }
    }

    fn push_audition(&mut self, request: &AuditionRequest) -> Result<AuditionPush, RuntimeError> {
        let Some(library) = self.session.sample_library().cloned() else {
            return Err(RuntimeError::Message(
                "no sample library to preview from".into(),
            ));
        };
        let Some(live) = self.live.as_ref() else {
            return Err(RuntimeError::Message("nothing is playing".into()));
        };
        let (name, index) = match request.sound.split_once(':') {
            Some((name, index)) => (name, index.parse::<f64>().unwrap_or(0.0)),
            None => (request.sound.as_str(), 0.0),
        };
        // A chord is one value per note on the same sound; a sample
        // preview is the same thing with no note to carry.
        let values: Vec<serde_json::Value> = if request.notes.is_empty() {
            vec![serde_json::json!({ "s": name, "n": index })]
        } else {
            request
                .notes
                .iter()
                .map(|note| serde_json::json!({ "s": name, "n": index, "note": note }))
                .collect()
        };
        let gate = if request.step_secs > 0.0 {
            // A note of a run rings well past the next one: a scale on a
            // piano is a hand rolling through it, not a row of clicks.
            (request.step_secs * 5.0).clamp(0.45, 0.9)
        } else if request.notes.is_empty() {
            AUDITION_GATE_SECS
        } else {
            AUDITION_CHORD_GATE_SECS
        };
        // Eight voices at full gain is eight times the level of one. Each
        // note takes its share of the room the way a mix does, so a chord
        // sits beside a one-shot rather than clipping over it. One voice
        // divides by one and is untouched. A run sounds one note at a
        // time, so it keeps its full level.
        let spread = if request.step_secs > 0.0 {
            1.0
        } else {
            1.0 / (values.len() as f32).sqrt()
        };
        // One target time for every voice makes a chord; a step between
        // them makes a run. A request that names its own moment lands on
        // it, never earlier than the device can take it.
        let earliest = live.device.clock_seconds() + AUDITION_LEAD_SECS;
        let first_time = request.at.map_or(earliest, |at| at.max(earliest));
        let mut events = Vec::with_capacity(values.len());
        for (step, value) in values.iter().enumerate() {
            let target_time = first_time + step as f64 * request.step_secs;
            let onset = match rustel_voice::resolve_voice_with_samples(
                value,
                u64::MAX,
                gate,
                target_time,
                live.device.sample_rate(),
                self.session.cps(),
                library.as_ref(),
            ) {
                Ok(onset) => onset,
                Err(message) if message.contains("still loading") => {
                    return Ok(AuditionPush::Loading);
                }
                Err(message) => return Err(RuntimeError::Message(message)),
            };
            // The device only plays samples it has been handed; one that just
            // finished downloading arrives through `install_ready_samples` on
            // a later tick. A chord waits for all of its notes rather than
            // sounding half of itself.
            if let Some(sample) = &onset.sample
                && !self.retained_samples.contains_key(&sample.sample)
            {
                return Ok(AuditionPush::Loading);
            }
            if let Some(sample) = &onset.sample {
                self.sample_last_used.insert(sample.sample, Instant::now());
            }
            events.push(AudioEvent {
                onset_id: u64::MAX,
                generation: live.device.generation(),
                target_frame: onset.onset_frame,
                onset_lead: onset.onset_lead,
                freq_hz: onset.freq_hz,
                // The preview's own volume, over the onset's: a browser next to
                // a playing set needs headroom past unity to be heard.
                gain: onset.gain * request.gain * spread,
                duration_secs: onset.duration_secs,
                ui_visuals: AUDITION_VISUALS,
                controls: onset.controls,
                sample: onset.sample,
                wavetable: onset.wavetable,
                synth: onset.synth,
                // One preview at a time: a new audition cuts the preview that
                // still rings, so auditions down a bank do not pile up. Each
                // note takes a choke group of its own, so the notes of a chord
                // ring together and the note of the next preview cuts the note
                // in the same position.
                cut: Some(audition_cut_group(request.slot + step)),
            });
        }
        let sample_rate = u64::from(live.device.sample_rate());
        self.audition_horizon_frame = self.audition_horizon_frame.max(
            events
                .iter()
                .map(|event| {
                    let duration_frames =
                        (event.duration_secs.max(0.0) * sample_rate as f32).ceil() as u64;
                    event.target_frame.saturating_add(duration_frames)
                })
                .max()
                .unwrap_or(0),
        );
        let mut reverbs = LiveReverbBatch::default();
        for event in &events {
            reverbs.observe(&live.device, event);
        }
        reverbs.flush(&live.device);
        // A three-note chord after a seven-note one leaves four groups
        // holding voices nobody cut: hush the tail of the block before the
        // new preview lands. A run's notes arrive one push at a time and
        // must not hush each other, so only a chord clears the tail.
        let tail = if request.step_secs > 0.0 {
            MAX_AUDITION_RUN_NOTES + 1
        } else {
            values.len()
        };
        for slot in tail..=MAX_AUDITION_RUN_NOTES {
            let _ = live.device.push(AudioEvent {
                onset_id: u64::MAX,
                generation: live.device.generation(),
                target_frame: frame_at(first_time, live.device.sample_rate()),
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.0,
                duration_secs: 0.01,
                ui_visuals: AUDITION_VISUALS,
                controls: Default::default(),
                sample: None,
                wavetable: None,
                synth: None,
                cut: Some(audition_cut_group(slot)),
            });
        }
        for event in events {
            if !live.device.push(event) {
                return Err(RuntimeError::Message(
                    "the device is busy; try the preview again".into(),
                ));
            }
            note_sample_use(
                &event,
                &self.retained_samples,
                self.session.sample_library().map(Arc::as_ref),
                &mut self.sample_use_until,
                live.device.sample_rate(),
                live.device.render_frontier_frames(),
            );
        }
        Ok(AuditionPush::Played)
    }

    /// Each turn's preview upkeep: keyboard releases, a preview whose
    /// sample was loading, and the notes of a run whose moments have come.
    fn advance_previews(&mut self) {
        self.advance_piano_releases();
        self.advance_audition();
        self.advance_run();
    }

    /// Retry a preview whose sample was loading, once per tick.
    fn advance_audition(&mut self) {
        let Some(pending) = self.live.as_ref().and_then(|live| live.audition.clone()) else {
            return;
        };
        let outcome = self.push_audition(&pending.request);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match outcome {
            Ok(AuditionPush::Loading) if Instant::now() < pending.deadline => {}
            Ok(AuditionPush::Loading) => {
                live.audition = None;
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message(
                        "preview",
                        format!("{} did not load in time to preview", pending.request.sound),
                    ),
                );
            }
            Ok(AuditionPush::Played) => live.audition = None,
            Err(error) => {
                live.audition = None;
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("preview", error.to_string()),
                );
            }
        }
    }

    /// Stop the silent score opened solely to host a browser audition once
    /// no pending note remains and the last scheduled voice has ended. A
    /// real score never carries `audition_owned`, so previewing over a set
    /// cannot stop the performance underneath it.
    fn retire_finished_audition_transport(&mut self) {
        if self.recording.is_some() || self.closing_recording.is_some() {
            return;
        }
        // A score waiting to land takes this output over when it does.
        if self.load.held_edit.is_some() || self.pending_launch.is_some() {
            return;
        }
        let finished = self.live.as_ref().is_some_and(|live| {
            if !live.audition_owned || live.audition.is_some() || live.run.is_some() {
                return false;
            }
            let now = (live.device.clock_seconds().max(0.0) * f64::from(live.device.sample_rate()))
                .floor() as u64;
            now >= self.audition_horizon_frame
        });
        if finished {
            if self.piano.iter().any(Option::is_some) {
                self.detach_stopped_output_for_piano();
            } else {
                self.request_stop();
            }
        }
    }

    /// Move one of the running score's sliders.
    ///
    /// The cell the pattern reads is updated in place and the active graph
    /// is re-queried under a fresh generation, so events already sent to
    /// the device before the takeover keep sounding while everything after
    /// it carries the new value - the same path a MIDI-mapped control takes
    /// in the CLI. Nothing here evaluates source.
    pub fn set_slider(&mut self, id: &str, value: f64) -> Result<(), RuntimeError> {
        self.set_slider_with_transition(id, value, false)
    }

    /// Commit the exact cell value and ramp eligible sustained gain, cutoff
    /// and resonance.
    pub fn set_slider_smoothed(&mut self, id: &str, value: f64) -> Result<(), RuntimeError> {
        self.set_slider_with_transition(id, value, true)
    }

    pub(super) fn set_slider_with_transition(
        &mut self,
        id: &str,
        value: f64,
        smooth: bool,
    ) -> Result<(), RuntimeError> {
        if !value.is_finite() {
            return Err(RuntimeError::Message(format!(
                "slider {id} was given a non-finite value"
            )));
        }
        let Some(slider) = self.ui.layout.slider(id) else {
            return Err(RuntimeError::Message(format!(
                "slider {id} is not part of the running score"
            )));
        };
        if value < slider.min || value > slider.max {
            return Err(RuntimeError::Message(format!(
                "slider {id} value {value} is outside {}..{}",
                slider.min, slider.max
            )));
        }
        self.session.consume_audio_confirmations();
        let device_time = self.device_time();
        let result = self
            .session
            .with_panic_recovery(device_time, |session| session.set_slider_value(id, value));
        self.sync_recovered_session();
        match result? {
            true => self.ui.layout.set_slider_value(id, value),
            false => {
                return Err(RuntimeError::Message(format!(
                    "the running score refused slider {id}"
                )));
            }
        }
        if self.is_stopping() || self.session.transport().is_stopped() {
            return Ok(());
        }
        if let Some(binding) = self.session.slider_binding(id)
            && (value as f32).is_finite()
        {
            let update = rustel_audio::live_control::LiveControlUpdate {
                binding,
                value: value as f32,
                smooth,
            };
            if let Some(pending) = self
                .pending_live_controls
                .iter_mut()
                .find(|pending| pending.binding == binding)
            {
                *pending = update;
            } else {
                self.pending_live_controls.push(update);
            }
            self.flush_live_controls();
        }
        let now = Instant::now();
        let due = self
            .slider_requery_at
            .is_none_or(|at| now.duration_since(at) >= SLIDER_REQUERY_INTERVAL);
        if due {
            self.requery_for_sliders(now)?;
        } else {
            self.slider_requery_pending = true;
        }
        Ok(())
    }

    /// Bring the sliders' current values forward into the queried horizon:
    /// one generation flip, one takeover seam.
    fn requery_for_sliders(&mut self, now: Instant) -> Result<(), RuntimeError> {
        self.sync_input_channels();
        self.slider_requery_pending = false;
        self.slider_requery_at = Some(now);
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        // A start's first query has not run: it reads the slider as it
        // stands, and a requery now would move cycle zero off the start.
        if live.initial_start_generation.is_some() {
            return Ok(());
        }
        if let Some((generation_before, generation_after)) = self
            .session
            .requery_active_at(live.device.clock_seconds())?
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                // A knob moving requeries about eight times a second, so
                // this is the machinery talking to itself: `Debug`, where
                // it is there for the reader who goes looking and nowhere
                // near the reader who does not.
                StudioDiagnostic::trace(
                    "sliders",
                    format!("slider requery {generation_before}→{generation_after}"),
                ),
            );
            live.producer
                .arm_control_requery(generation_before, generation_after);
        }
        Ok(())
    }

    /// A slider move that arrived inside the interval gets its re-query
    /// once the interval has passed, so the last value of a drag always
    /// lands in the horizon.
    fn flush_slider_requery(&mut self) {
        if !self.slider_requery_pending
            || self.is_stopping()
            || self.session.transport().is_stopped()
            || self.live.is_none()
        {
            return;
        }
        let now = Instant::now();
        if self
            .slider_requery_at
            .is_none_or(|at| now.duration_since(at) >= SLIDER_REQUERY_INTERVAL)
            && let Err(error) = self.requery_for_sliders(now)
        {
            self.pending_diagnostics
                .push_back(StudioDiagnostic::message("slider", error.to_string()));
        }
    }

    fn flush_live_controls(&mut self) {
        if self.is_stopping() || self.session.transport().is_stopped() {
            self.pending_live_controls.clear();
            return;
        }
        let Some(live) = self.live.as_mut() else {
            self.pending_live_controls.clear();
            return;
        };
        self.pending_live_controls
            .retain(|update| !live.device.try_set_live_control(*update));
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

    /// What the retained sound costs, kept and otherwise, and the limits
    /// it is held to.
    fn sample_memory(&mut self) -> SampleMemory {
        let mut memory = SampleMemory {
            preview_budget_bytes: self.preview_budget_bytes,
            unused_idle: self.unused_sample_idle,
            recent_limit_bytes: self.recent_tabs_allowance(),
            ..SampleMemory::default()
        };
        if self.retained_samples.is_empty() {
            return memory;
        }
        // A display: a stale answer does until the next due pass.
        self.refresh_protection(Instant::now());
        let Some(protection) = &self.protection else {
            return memory;
        };
        for (id, sample) in &self.retained_samples {
            if protection.ids.contains(id) {
                memory.live_bytes += sample.pcm_bytes();
                if protection.recent.contains(id) {
                    memory.recent_bytes += sample.pcm_bytes();
                }
            } else {
                memory.preview_bytes += sample.pcm_bytes();
            }
        }
        memory
    }

    /// What the audio side holds: the open output's own account, the
    /// inputs the engine opened itself and what the engine keeps for the
    /// output. `None` with neither an output nor an input open. Each input
    /// is counted by the stream that fills it: a sample recorded from the
    /// chosen input reads the output's ring or the monitor's, and only one
    /// recorded from the default input has a ring of its own.
    fn audio_memory(&self) -> Option<AudioMemory> {
        let device = self.audio_device().map(LiveScalarDevice::memory);
        let monitor = self.input_monitor.as_ref();
        let recording = self
            .sample_recording
            .as_ref()
            .and_then(|recording| recording.own_input.as_ref());
        if device.is_none() && monitor.is_none() && recording.is_none() {
            return None;
        }
        let device = device.unwrap_or_default();
        let own_inputs: usize = monitor
            .into_iter()
            .chain(recording)
            .map(|input| input.ring().touched_bytes())
            .sum();
        let backlog = self
            .live
            .as_ref()
            .map_or(0, |live| live.producer.backlog_bytes());
        Some(AudioMemory {
            event_ring: device.event_rings,
            input: device.input + own_inputs,
            record: device.record,
            reverbs: device.reverbs,
            backend: device.backend,
            other: device.analysis + backlog,
        })
    }

    // ---- clocks ----------------------------------------------------------

    /// Send MIDI clock to a port, or to none.
    pub fn set_clock_out(&mut self, port: Option<&str>) -> Result<(), String> {
        if let Some(mut previous) = self.clock_out.take() {
            previous.advance(false, 0.0, 1.0, Instant::now());
        }
        if let Some(port) = port {
            self.clock_out = Some(ClockOut::open(port)?);
        }
        Ok(())
    }

    /// Say which ports may be opened from here on.
    ///
    /// Changing this does not close what is already open: a port that was
    /// allowed when the set started keeps its sender until the set ends,
    /// because pulling a live output mid-phrase leaves whatever it was
    /// playing stuck on. It decides what may be opened NEXT, and the
    /// refusals already said are forgotten so a port turned back on can
    /// report its own trouble again.
    pub fn set_midi_enablement(&mut self, enabled: super::devices::MidiEnablement) {
        if self.midi_enabled != enabled {
            self.midi_disabled_reported.clear();
        }
        self.midi_enabled = enabled;
    }

    /// Follow MIDI clock from a port, or from none.
    pub fn set_clock_in(&mut self, port: Option<&str>) -> Result<(), String> {
        self.clock_in = None;
        self.clock_locked = false;
        self.clock_external_cps = None;
        if let Some(port) = port {
            self.clock_in = Some(ClockIn::open(port)?);
        }
        Ok(())
    }

    /// Join, lead or leave the Link session.
    /// Lead or follow the Link session this tick.
    fn advance_clock_out(&mut self) {
        let Some(out) = self.clock_out.as_mut() else {
            return;
        };
        let playing = self
            .live
            .as_ref()
            .is_some_and(|live| live.draining.is_none())
            && !self.session.transport().is_stopped();
        let (cycle_now, cps) = match self.live.as_ref() {
            Some(live) => {
                let now = live.device.clock_seconds();
                (self.session.cycle_at_time(now), self.session.cps())
            }
            None => (0.0, self.session.cps()),
        };
        out.advance(playing, cycle_now, cps, Instant::now());
    }

    /// The MIDI clock in, unless Link is being followed instead.
    fn follow_clock_in(&mut self) {
        let Some(clock) = self.clock_in.as_ref() else {
            return;
        };
        let now_instant = Instant::now();
        let Some(estimate) = clock.estimate(now_instant) else {
            self.clock_locked = false;
            self.clock_external_cps = None;
            return;
        };
        self.clock_external_cps = Some(estimate.cps);
        self.steer_to(estimate, now_instant);
    }

    /// Steer the scheduler onto an outside clock: bend the tempo a little
    /// towards it while the phase is close, jump when it is far, and
    /// re-query so the device hears the change. The arithmetic itself is
    /// shared with the live command, so both follow a clock identically.
    fn steer_to(&mut self, estimate: ClockEstimate, now_instant: Instant) {
        if !estimate.running
            || now_instant.duration_since(self.clock_followed_at) < midi_clock::FOLLOW_INTERVAL
        {
            return;
        }
        self.clock_followed_at = now_instant;
        if self.is_stopping() || self.session.transport().is_stopped() {
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let now = live.device.clock_seconds();
        let (steer, locked) = midi_clock::steer(
            estimate,
            self.session.cycle_at_time(now),
            self.session.cps(),
        );
        self.clock_locked = locked;
        let midi_clock::Steer::To { cps, cycle } = steer else {
            return;
        };
        self.session.retime(now, cps, cycle);
        if let Ok(Some((before, after))) = self.session.requery_active_at(now) {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                // The same, once per steer while an outside clock leads.
                StudioDiagnostic::trace("clock", format!("clock steer requery {before}→{after}")),
            );
            live.producer.arm_control_requery(before, after);
        }
    }

    // ---- orbits ----------------------------------------------------------

    /// The orbits heard in the last few seconds, with their held peaks.
    fn orbit_levels(&self) -> Vec<OrbitLevel> {
        let now = Instant::now();
        self.orbit_last_heard
            .iter()
            .enumerate()
            .filter(|(_, heard)| heard.is_some_and(|at| now.duration_since(at) < ORBIT_LINGER))
            .map(|(orbit, _)| OrbitLevel {
                orbit: orbit as u8,
                peak: self.orbit_peak[orbit],
                pair: self.orbit_routing[orbit],
            })
            .collect()
    }

    /// Send an orbit to an output pair; remembered across devices.
    pub fn route_orbit(&mut self, orbit: usize, pair: u8) {
        if let Some(slot) = self.orbit_routing.get_mut(orbit) {
            *slot = pair;
        }
        if let Some(live) = self.live.as_ref() {
            live.device.set_orbit_output(orbit, pair);
        }
    }

    /// Read the orbit meters off the device and keep the routing applied
    /// to whichever device is open.
    fn observe_orbits(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if live.device.orbit_outputs() != self.orbit_routing {
            for (orbit, pair) in self.orbit_routing.iter().enumerate() {
                live.device.set_orbit_output(orbit, *pair);
            }
        }
        let levels = live.device.take_orbit_levels();
        let now = Instant::now();
        for (orbit, level) in levels.iter().enumerate() {
            if *level > 0.0005 {
                self.orbit_last_heard[orbit] = Some(now);
            }
            self.orbit_peak[orbit] = level.max(self.orbit_peak[orbit] * ORBIT_PEAK_DECAY);
        }
    }

    // ---- quantised launch --------------------------------------------

    fn launch_info(&self) -> Option<LaunchInfo> {
        let now = self.live.as_ref()?.device.clock_seconds();
        self.launch_info_at(now)
    }

    /// The countdown at device time `now`: a launch still armed, or one
    /// that fired within its head-room while its line is still ahead. Past
    /// the line there is nothing to wait for and no countdown to show -
    /// the grace memory answers a repeat press, but it is not a launch.
    fn launch_info_at(&self, now: f64) -> Option<LaunchInfo> {
        let (boundary_cycle, boundary_time) = match (&self.pending_launch, self.landing.as_ref()) {
            (Some(pending), _) => (pending.boundary_cycle, self.pending_line_time(pending, now)),
            (None, Some(landing)) if now < landing.boundary_time => {
                (landing.boundary_cycle, landing.boundary_time)
            }
            _ => return None,
        };
        let cycle_now = self.session.cycle_at_time(now);
        Some(LaunchInfo {
            boundary_cycle,
            cycles_left: (boundary_cycle - cycle_now).max(0.0),
            seconds_left: (boundary_time - now).max(0.0),
        })
    }

    /// When an armed launch's line falls under the mapping in force NOW.
    /// The line is a cycle; an outside clock that retimes the transport
    /// during the countdown moves the time of that cycle, and the takeover,
    /// cycle zero and the device cut must all land on the moved bar line,
    /// not where it stood when the pad was pressed.
    fn pending_line_time(&self, pending: &PendingLaunch, now: f64) -> f64 {
        let cps = self.session.cps();
        if !(cps.is_finite() && cps > 0.0) {
            return pending.boundary_time;
        }
        let time = now + (pending.boundary_cycle - self.session.cycle_at_time(now)) / cps;
        if time.is_finite() {
            time
        } else {
            pending.boundary_time
        }
    }

    fn launch_headroom(&self) -> f64 {
        self.live
            .as_ref()
            .map(|live| {
                live.device.schedule_lead_seconds()
                    + live.device.continuity_margin_seconds()
                    + LAUNCH_HEADROOM_SECS
            })
            .unwrap_or(LAUNCH_HEADROOM_SECS)
    }

    /// Arm `source` to take over on the next line of `unit_cycles`. With
    /// nothing playing there is no line to wait for: `Ok(None)` says
    /// "evaluate now". A launch already armed is replaced.
    pub fn arm_launch(
        &mut self,
        source: &str,
        mini: bool,
        unit_cycles: f64,
        rewind: bool,
    ) -> Result<Option<LaunchInfo>, RuntimeError> {
        if self.live.is_none() || self.is_stopping() {
            self.pending_launch = None;
            self.recent_rewind = None;
            return Ok(None);
        }
        // A start still holding its cycle zero has no line yet: the launch
        // replaces it now, from its own cycle zero.
        if self.start_is_held() {
            return Ok(None);
        }
        let now = self.live.as_ref().expect("checked").device.clock_seconds();
        self.arm_launch_at(now, source, mini, unit_cycles, rewind)
    }

    /// [`Self::arm_launch`] decided at one device time, `now`: every guard
    /// and every answer reads it, never the clock again, so an answer cannot
    /// disagree with the guard that chose it.
    fn arm_launch_at(
        &mut self,
        now: f64,
        source: &str,
        mini: bool,
        unit_cycles: f64,
        rewind: bool,
    ) -> Result<Option<LaunchInfo>, RuntimeError> {
        // Settle first. A press can arrive after the line has passed and
        // before the next tick runs, and the landing is then still set.
        // Settling here turns it into the memory that the guard below
        // reads.
        self.settle_landing_at(now);
        // A launch that has fired but not yet landed already owns the line:
        // re-arming the same score would install it a second time, one line
        // later, and the ear hears the first beat twice. The same source
        // pressed again is the same request - answer it with the launch in
        // flight. A different source is an edit and must supersede, so it
        // replaces the landing's countdown below.
        if let Some(landing) = &self.landing
            && now < landing.boundary_time
            && landing.install.source_revision == source_revision(source)
            && landing.rewind == rewind
        {
            // The worker arms this request, and the next outcome poll
            // answers it with the landing's own install: same success,
            // no second install. The answer is built from the `now` that
            // passed the guard: reading the clock again could cross the
            // line in between and turn it into "evaluate now" - a second
            // restart on the downbeat.
            let answer = LaunchInfo {
                boundary_cycle: landing.boundary_cycle,
                cycles_left: (landing.boundary_cycle - self.session.cycle_at_time(now)).max(0.0),
                seconds_left: (landing.boundary_time - now).max(0.0),
            };
            self.launch_outcome = Some(Ok(StudioInstall {
                answered_repeat: true,
                ..landing.install.clone()
            }));
            self.log_launch("repeat press before the line - answered with the launch in flight");
            return Ok(Some(answer));
        }
        // The line has passed and the landing is settled. The second press
        // of a fast double press can still arrive here: the first press
        // fires one head-room early, so the second finds no pending launch
        // and no landing. Arming the same score again installs it from zero
        // one line later, and the first beat sounds twice. So the fired
        // rewind is remembered briefly past its line, and the same score
        // pressed inside the grace is answered with it. Past the grace the
        // memory expires and the press restarts the score. The grace does
        // not apply while a launch is armed or landing.
        if self.pending_launch.is_none()
            && self.landing.is_none()
            && rewind
            && let Some(recent) = &self.recent_rewind
            && now < recent.landed_at + recent.grace_secs
            && recent.install.source_revision == source_revision(source)
        {
            let answer = LaunchInfo {
                boundary_cycle: recent.boundary_cycle,
                cycles_left: 0.0,
                seconds_left: 0.0,
            };
            self.launch_outcome = Some(Ok(StudioInstall {
                answered_repeat: true,
                ..recent.install.clone()
            }));
            // Some, not None: None sends the worker into an immediate
            // evaluate - a third restart, right now. The info says the
            // answered press waits for nothing.
            self.log_launch("repeat press just after the line - answered with the fired rewind");
            return Ok(Some(answer));
        }
        let cps = self.session.cps().max(1e-6);
        let cycle_now = self.session.cycle_at_time(now);
        let headroom_cycles = self.launch_headroom() * cps;
        let boundary_cycle = next_boundary(cycle_now, unit_cycles, headroom_cycles);
        let boundary_time = now + (boundary_cycle - cycle_now) / cps;
        // The countdown gives the decoder a head start: the incoming
        // score's sounds start to load at arm time, not at fire time. One
        // beat is usually enough for the replacement's first window to
        // render complete, so the line needs no loading hold.
        // Warming does not install. Until the launch lands, the playing
        // score stays the current score and the sample sweep keeps what
        // it names.
        self.warm_incoming(source);
        self.look_up_imports_of(source);
        self.pending_launch = Some(PendingLaunch {
            source: source.to_owned(),
            mini,
            boundary_cycle,
            boundary_time,
            rewind,
            unit_cycles,
            preview: std::mem::take(&mut self.next_install_preview),
            followed: self.new_in_text(source, mini),
            waited: false,
        });
        self.ask_for_waited();
        // The launch now promises a boundary on this timeline.
        self.live
            .as_mut()
            .expect("checked")
            .initial_start_generation = None;
        self.launch_outcome = None;
        let info = self.launch_info_at(now);
        debug_assert!(info.is_some(), "a freshly armed launch has a countdown");
        Ok(info)
    }

    /// Log one launch decision with the transport cycle, so a reader can
    /// match the line to the beat. It is a note: it reaches the studio log
    /// and the log file on disk, and does not take the status line.
    fn log_launch(&mut self, message: &str) {
        let cycle = self.session.cycle_at_time(self.device_time());
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::note("launch", format!("{message} (cycle {cycle:.2})")),
        );
    }

    /// A launch outcome the player has to be told about. The press was
    /// already answered - "updated, swaps in on the next cycle" - and then
    /// the room did not do what that promised: the score was refused, or
    /// the room fell silent at the line and could not be refilled. As a
    /// log note nobody would know why nothing changed, so it takes the
    /// status line as a warning.
    fn warn_launch(&mut self, message: &str) {
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::message("launch", message),
        );
    }

    fn say_launch(&mut self, line: LaunchLine) {
        match line {
            LaunchLine::Note(message) => self.log_launch(&message),
            LaunchLine::Warning(message) => self.warn_launch(&message),
        }
    }

    /// Forget a launch still waiting for its fire, and an edit held for its
    /// sounds; the outcome says it was cancelled. A launch that has already
    /// fired is untouched: its generation is built, its line is armed, and
    /// a newer launch armed over it (a pad race) lands after it rather than
    /// unmaking it - the in-flight launch still answers a repeat of itself.
    pub fn cancel_pending_launch(&mut self) {
        let launch = self.pending_launch.take().is_some();
        let held = self.load.held_edit.take().is_some();
        if launch || held {
            self.launch_outcome = Some(Err(RuntimeError::Cancelled));
        }
    }

    /// End every launch in flight: an edit played now, or a stop. The
    /// pending launch is cancelled, and a fired launch that has not been
    /// heard gives back its line cut. A cut that stays armed silences what
    /// plays at the line: the edit, or the tail that a stop lets ring out.
    pub fn supersede_launch(&mut self) {
        self.cancel_pending_launch();
        self.abandon_landing(false, false);
    }

    /// Let go of a fired launch before, or instead of, its landing.
    ///
    /// A landing whose line has passed and whose generation reached the
    /// device was a rewind the ear has heard: it stays answerable through
    /// the grace. Anything else never sounded, and its pre-armed line cut
    /// is withdrawn so the rendition still playing keeps playing. With
    /// `refill`, a withdrawal that came too late (a block across the line
    /// had already cut the outgoing onsets) asks the still-active score
    /// again from now. `refused` is a replacement the producer rolled back:
    /// even if the rollback's own flip has since reached the device, the
    /// refused install never sounded and must not answer a repeat.
    fn abandon_landing(&mut self, refill: bool, refused: bool) {
        let Some(landing) = self.landing.take() else {
            return;
        };
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let heard = !refused
            && live.device.clock_seconds() >= landing.boundary_time
            && live.device.generation() >= landing.install.generation;
        if heard {
            if landing.rewind {
                self.recent_rewind = Some(RecentRewind {
                    grace_secs: rewind_grace_secs(landing.unit_cycles, self.session.cps()),
                    install: landing.install,
                    boundary_cycle: landing.boundary_cycle,
                    landed_at: landing.boundary_time,
                });
            }
            return;
        }
        if landing.rewind {
            self.withdraw_line_cut(refill);
        }
    }

    /// Withdraw the line cut a fired rewind armed, because no flip of its
    /// is coming. The consumer lets a fired arm go on the withdrawal, but
    /// the outgoing onsets it retired at the line are gone. So when the arm
    /// was still set and the render frontier, read after the withdrawal, has
    /// passed its line, what the external outputs owed from the line goes
    /// too. With `refill` the still-active score is then asked again from
    /// now, so the room refills at the continuity margin. A line a published
    /// flip already cleared is left alone: what sounds from it is the
    /// launched score.
    fn withdraw_line_cut(&mut self, refill: bool) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let armed = live.device.armed_line_frame();
        live.device.clear_line_arm();
        self.line_cut_withdrawn = false;
        let Some(line_frame) = armed else {
            return;
        };
        if !self.retire_past_fired_line(line_frame) || !refill {
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let now = live.device.clock_seconds();
        let requery = self.session.requery_active_at(now);
        if let Ok(Some((before, after))) = requery {
            live.producer.arm_control_requery(before, after);
        }
        if let Some(line) = refill_line(requery) {
            self.say_launch(line);
        }
    }

    /// A fired launch whose replacement the producer refused (its first
    /// window could not sound, or it ran out of room) will never flip in,
    /// whether the producer rolled back, is still rolling back, or latched.
    /// The producer names that generation; the session cannot, because the
    /// commonest pad rewind restarts the score already playing, and a
    /// rollback then puts back the very text the launch installed. Its line
    /// cut is withdrawn at once - the rollback's own flip carries the
    /// handoff and its re-query refills the room - and it is forgotten
    /// rather than promoted to the grace memory, since the ear never heard
    /// it. A landing the line already settled into that memory on this
    /// turn is forgotten there too.
    fn abandon_rolled_back_landing(&mut self) {
        let Some(abandoned) = self
            .live
            .as_mut()
            .and_then(|live| live.producer.take_abandoned_generation())
        else {
            return;
        };
        if self
            .recent_rewind
            .as_ref()
            .is_some_and(|recent| recent.install.generation == abandoned)
        {
            self.recent_rewind = None;
        }
        if self
            .landing
            .as_ref()
            .is_none_or(|landing| landing.install.generation != abandoned)
        {
            return;
        }
        self.warn_launch("the launch's score was refused - the playing score keeps the line");
        self.abandon_landing(false, true);
    }

    /// The generation a repeat launch press was just answered with, while
    /// that answer waits for the worker to collect it: nothing is armed and
    /// nothing will install, so there is no line to announce.
    pub fn answered_repeat_generation(&self) -> Option<u64> {
        match &self.launch_outcome {
            Some(Ok(install)) if install.answered_repeat => Some(install.generation),
            _ => None,
        }
    }

    /// What became of the armed launch, once it fired or was dropped.
    pub fn take_launch_outcome(&mut self) -> Option<Result<StudioInstall, RuntimeError>> {
        self.launch_outcome.take()
    }

    /// Withdraw the pre-armed line cut while a replacement waits for its
    /// samples.
    ///
    /// `advance_launch` arms the cut at the line before the replacement's
    /// evaluation runs. If the replacement's first window then defers for
    /// loading (the producer's rewind loading hold), the flip is held. A
    /// cut that fires with no replacement ready fades the outgoing score
    /// into silence for as long as the decode takes, and the flip's own
    /// choke ramp then lands on silence, which clicks. So the arm is
    /// withdrawn while the hold is on, and the outgoing score keeps
    /// playing. When loading finishes the flip lands with its own
    /// AtTakeover intent, which is the cut at the line; the arm is not put
    /// back. The producer never sees the withdrawal: the arm is its cue to
    /// cut, not a prerequisite for holding.
    fn sync_line_cut_with_loading_holds(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let deferred = live.producer.cut_takeover_deferred_for_loading();
        if deferred && !self.line_cut_withdrawn {
            let line = live.device.armed_line_frame();
            live.device.clear_line_arm();
            self.line_cut_withdrawn = true;
            // An arm that fired first has retired the outgoing onsets
            // already, and the withdrawal brings none of them back.
            if let Some(line) = line {
                self.retire_past_fired_line(line);
            }
            self.log_launch("line cut held - replacement samples still loading");
        } else if !deferred && self.line_cut_withdrawn {
            // The hold is over because the flip landed (or the replacement
            // rolled back). Nothing is re-armed: the flip carries its own
            // AtTakeover intent, which fades the countdown at the line and
            // drops the old ring events from it. The consumer's arm does
            // not check the generation, so a second arm on a landed flip
            // would retire the new generation's onsets at the line, and
            // the restarted loop would lose its first beat. After a
            // rollback it would cut the score still playing into silence.
            self.line_cut_withdrawn = false;
        }
    }

    /// Fire the armed launch if its head-room has opened, as the next
    /// tick would. A launch that fires here owns its line: a command
    /// handled before that tick finds it in flight, not waiting. Does
    /// nothing while stopped, when no tick runs.
    pub fn fire_due_launch(&mut self) {
        if !self.is_playing() {
            return;
        }
        self.sync_line_cut_with_loading_holds();
        self.advance_launch();
    }

    /// Fire the armed launch once its line is within the head-room: the
    /// reload is told the takeover time, so the old generation sounds
    /// until the line and the new one is queried from it.
    fn advance_launch(&mut self) {
        let Some(pending) = self.pending_launch.as_ref() else {
            return;
        };
        let Some(live) = self.live.as_ref() else {
            self.pending_launch = None;
            self.launch_outcome = Some(Err(RuntimeError::Cancelled));
            return;
        };
        let now = live.device.clock_seconds();
        let boundary_time = self.pending_line_time(pending, now);
        if now < boundary_time - self.launch_headroom() {
            return;
        }
        if self.launch_waits_for_sounds(now) {
            return;
        }
        let pending = self.pending_launch.take().expect("checked");
        if pending.rewind {
            self.log_launch("rewind fired - from zero at the line");
        }
        self.session.set_next_takeover_time(boundary_time);
        // A rewinding launch puts cycle zero on the line it lands on, so
        // the score starts from its beginning exactly where the bar does.
        if pending.rewind {
            self.session.start_next_from_zero();
        }
        // Pre-arm the line before the evaluation, for a rewinding launch
        // only. A rewind replaces the room at the line. However long the
        // evaluation takes, the outgoing rendition falls silent there and
        // its later onsets are dropped, so the old score cannot play
        // through the countdown and sound its first beat twice. A launch
        // that does not rewind arms nothing: it hands over with the
        // ring-out contract. A failed evaluation withdraws the arm below,
        // and the room keeps playing.
        if pending.rewind {
            let device_cut = frame_at(
                boundary_time,
                self.live.as_ref().expect("checked").device.sample_rate(),
            );
            self.live
                .as_ref()
                .expect("checked")
                .device
                .arm_line_cut(device_cut, true);
            // A fresh arm owns the line: any earlier withdrawal described a
            // previous launch's held flip, not this one.
            self.line_cut_withdrawn = false;
        }
        self.next_install_preview = pending.preview;
        let outcome = self.reload_live(&pending.source, pending.mini);
        if let Ok(install) = &outcome {
            self.landing = Some(Landing {
                boundary_cycle: pending.boundary_cycle,
                boundary_time,
                install: install.clone(),
                rewind: pending.rewind,
                unit_cycles: pending.unit_cycles,
            });
        } else {
            // A takeover time nobody consumed would silently delay the next
            // ordinary update to a line that has since passed.
            self.session.clear_next_takeover_time();
            self.session.clear_next_from_zero();
            // The failed evaluation installed nothing, so the old score is
            // still the active one: withdraw its cut, and refill it if the
            // evaluation outlasted the line and the cut already fired.
            if pending.rewind {
                self.withdraw_line_cut(true);
            }
        }
        self.launch_outcome = Some(outcome);
    }

    /// A fired launch is landed once its line has passed; the countdown
    /// ends there, not a head-room early.
    fn settle_landing(&mut self) {
        if let Some(live) = self.live.as_ref() {
            self.settle_landing_at(live.device.clock_seconds());
        }
    }

    fn settle_landing_at(&mut self, now: f64) {
        let boundary_time = self.landing.as_ref().map(|landing| landing.boundary_time);
        if let Some(boundary_time) = boundary_time
            && now >= boundary_time
            && let Some(landing) = self.landing.take()
            && landing.rewind
        {
            // The line has passed and the fired rewind is now the room:
            // keep it briefly past the line so the second press of an
            // eager double - which lands after it - is answered with it
            // rather than re-installed from zero one line later.
            self.recent_rewind = Some(RecentRewind {
                grace_secs: rewind_grace_secs(landing.unit_cycles, self.session.cps()),
                install: landing.install,
                boundary_cycle: landing.boundary_cycle,
                landed_at: landing.boundary_time,
            });
        }
    }

    // ---- takes ---------------------------------------------------------

    /// Start writing the final mix to `path`. With nothing playing the
    /// device is opened on a silent score first, so the take starts now.
    pub fn start_recording(&mut self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        if self.closing_recording.is_some() {
            return Err(RuntimeError::Message(
                "the previous take is still closing".into(),
            ));
        }
        if self.recording.is_some() {
            return Err(RuntimeError::Message("already recording".into()));
        }
        if self.sample_recording.is_some() {
            return Err(RuntimeError::Message(
                "a sample is recording - finish the sample before recording a take".into(),
            ));
        }
        if self.live.is_none() {
            self.start_silence()?;
        }
        let live = self.live.as_mut().expect("device opened");
        // A preview may have opened this otherwise-idle transport. Recording
        // takes ownership now, so the preview horizon must not retire it.
        live.audition_owned = false;
        let sample_rate = live.device.sample_rate();
        let writer = TakeWriter::start(path, sample_rate)
            .map_err(|error| RuntimeError::Message(format!("cannot record: {error}")))?;
        let capture = live
            .device
            .start_recording_capture()
            .map_err(|error| RuntimeError::Message(format!("cannot record: {error}")))?;
        self.recording = Some(Recording {
            writer,
            input: Some(RecordingInput {
                capture,
                sample_rate,
            }),
            sample_rate,
            started: Instant::now(),
            frames: 0,
            dropped: 0,
            tap_dropped_seen: 0,
            scratch: Vec::with_capacity(1 << 16),
        });
        Ok(())
    }

    /// Close and join the take. On success, everything queued reaches disk
    /// and the header carries the final sizes; inspect the returned status
    /// for writer errors.
    ///
    /// The wait is bounded. The audio callback retires the tap, and a
    /// device that never calls again (a wedged driver, an unplugged
    /// interface) must not block this thread: a blocked studio cannot read
    /// a key or handle a signal. Frames the tap never hands over are not
    /// on disk; the writer is still joined and its status is still
    /// reported.
    pub fn stop_recording(&mut self) -> Option<TakeStatus> {
        self.stop_recording_with(RecordCapture::is_closed)
    }

    /// `stop_recording` with the tap's retirement test injected, so a
    /// driver that never calls again can be modelled without one.
    fn stop_recording_with(
        &mut self,
        retired: impl Fn(&RecordCapture) -> bool,
    ) -> Option<TakeStatus> {
        const TAP_CLOSE_WAIT: Duration = Duration::from_millis(500);
        // Ask first: a take nobody has asked to close yet (the engine
        // dropping on quit with the tape still rolling) has no closure to
        // advance, and returning None there left the writer unjoined and
        // the header without its sizes.
        self.request_recording_close();
        let deadline = Instant::now() + TAP_CLOSE_WAIT;
        loop {
            self.advance_recording_close_with(|capture| retired(capture));
            match self.closing_recording.take()? {
                ClosingRecording::Writer(writer) => return Some(writer.finish()),
                pending @ ClosingRecording::Tap { .. } => {
                    if Instant::now() >= deadline {
                        // Return the tap so `advance_recording_close` can
                        // keep trying on later turns; the caller hears the
                        // same "nothing closed" a fresh take would say.
                        self.closing_recording = Some(pending);
                        return None;
                    }
                    self.closing_recording = Some(pending);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// Close the tap without waiting for its writer. An existing closure
    /// remains owned here until it is collected, including after audio Stop.
    pub(super) fn request_recording_close(&mut self) -> bool {
        if !self.close_recording_for_rate_change() {
            self.close_recording(true);
        }
        self.closing_recording.is_some()
    }

    fn close_recording_for_rate_change(&mut self) -> bool {
        let (Some(recording), Some(device)) = (self.recording.as_ref(), self.audio_device()) else {
            return false;
        };
        if device.sample_rate() == recording.sample_rate {
            return false;
        }
        let path = recording.writer.status().path;
        self.close_recording(true);
        self.pending_diagnostics
            .push_back(StudioDiagnostic::message(
                "record",
                format!(
                    "take ended at {}: the output changed sample rate",
                    path.display()
                ),
            ));
        true
    }

    fn close_recording(&mut self, mut flush: bool) {
        let Some(mut recording) = self.recording.take() else {
            return;
        };
        debug_assert!(self.closing_recording.is_none());
        if let Some(input) = recording.input.as_ref()
            && let Err(error) = input.capture.request_close()
        {
            recording.writer.fail(&error.to_string());
            flush = false;
            recording.input = None;
        }
        let target = recording.target_frames();
        self.closing_recording = Some(ClosingRecording::Tap {
            recording,
            target,
            has_output: self.audio_device().is_some(),
            flush,
        });
    }

    fn advance_recording_close(&mut self) {
        self.advance_recording_close_with(RecordCapture::is_closed);
    }

    fn advance_recording_close_with(&mut self, retired: impl FnOnce(&RecordCapture) -> bool) {
        let Some(closing) = self.closing_recording.take() else {
            return;
        };
        let ClosingRecording::Tap {
            mut recording,
            target,
            has_output,
            flush,
        } = closing
        else {
            self.closing_recording = Some(closing);
            return;
        };
        if recording
            .input
            .as_ref()
            .is_some_and(|input| !retired(&input.capture))
        {
            self.closing_recording = Some(ClosingRecording::Tap {
                recording,
                target,
                has_output,
                flush,
            });
            return;
        }
        if flush {
            recording.scratch.clear();
            let result = recording.drain_input(None).and_then(|loss| {
                recording
                    .submit(loss, target, has_output)
                    .map_err(str::to_owned)
            });
            if let Err(error) = result {
                recording.writer.fail(&error);
            }
        }
        let mut writer = recording.writer;
        writer.request_close();
        self.closing_recording = Some(ClosingRecording::Writer(writer));
    }

    /// Collect once, without joining an unfinished writer. The caller must
    /// reserve space for the final reply before removing its only owner.
    pub(super) fn try_finish_recording(&mut self) -> Option<TakeStatus> {
        self.advance_recording_close();
        let ClosingRecording::Writer(writer) = self.closing_recording.as_mut()? else {
            return None;
        };
        let status = writer.try_finish()?;
        drop(self.closing_recording.take());
        Some(status)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// The ring the chosen input writes into right now: the live device's
    /// while one plays, the monitor's while nothing does.
    fn chosen_input_ring(&self) -> Option<Arc<rustel_audio::input::InputRing>> {
        match self.live.as_ref() {
            Some(live) => live.device.input_ring(),
            None => self
                .input_monitor
                .as_ref()
                .map(|input| Arc::clone(input.ring())),
        }
    }

    /// Start recording a sample from the audio input into `path`.
    ///
    /// The chosen input when there is one - the one `s("in")` plays,
    /// through its fader - and otherwise the host's default input, opened
    /// for this recording alone. Refused while a take records, and refused
    /// as a machine with no input refuses it when the config opens no
    /// default input ([`StudioConfig::open_default_input`], a harness's
    /// setting).
    pub fn start_sample_recording(&mut self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        if self.recording.is_some() || self.closing_recording.is_some() {
            return Err(RuntimeError::Message(
                "a take is recording - finish the take before recording a sample".into(),
            ));
        }
        if self.sample_recording.is_some() {
            return Err(RuntimeError::Message("already recording a sample".into()));
        }
        let mut own_input = None;
        let ring = if self.preferred_input.is_some() {
            self.ensure_input();
            self.chosen_input_ring().ok_or_else(|| {
                RuntimeError::Message(
                    "the chosen audio input is not open - check it under Devices".into(),
                )
            })?
        } else {
            let no_input = |why: &dyn std::fmt::Display| {
                RuntimeError::Message(format!("no audio input to record from: {why}"))
            };
            if !self.config.open_default_input {
                return Err(no_input(&"this studio opens no default input"));
            }
            let ring = Arc::new(rustel_audio::input::InputRing::new());
            let input = rustel_audio::AudioInput::open(None, Arc::clone(&ring), None)
                .map_err(|error| no_input(&error))?;
            own_input = Some(input);
            ring
        };
        let sample_rate = ring.sample_rate();
        if sample_rate == 0 {
            return Err(RuntimeError::Message(
                "the audio input has not started yet - press again in a moment".into(),
            ));
        }
        let writer = TakeWriter::start(path, sample_rate)
            .map_err(|error| RuntimeError::Message(format!("cannot record: {error}")))?;
        self.sample_recording = Some(SampleRecording {
            writer,
            cursor: ring.written(),
            ring,
            own_input,
            dropped: 0,
        });
        Ok(())
    }

    pub fn is_recording_sample(&self) -> bool {
        self.sample_recording.is_some()
    }

    /// One turn of the sample recorder. The input can move between the
    /// monitor and the live device as playback starts and stops; the
    /// recording follows it, and what fell between the two is not in it.
    fn pump_sample_recording(&mut self) {
        let chosen = self.chosen_input_ring();
        let Some(recording) = self.sample_recording.as_mut() else {
            return;
        };
        if recording.own_input.is_none()
            && let Some(ring) = chosen
            && !Arc::ptr_eq(&ring, &recording.ring)
        {
            recording.cursor = ring.written();
            recording.ring = ring;
        }
        recording.drain();
    }

    /// Finish the sample: what the input wrote since the last turn, then
    /// the writer joined. No tap has to retire, so this does not wait on
    /// the audio callback. `None` when no sample is recording.
    pub fn stop_sample_recording(&mut self) -> Option<TakeStatus> {
        self.pump_sample_recording();
        let mut recording = self.sample_recording.take()?;
        recording.drain();
        // The default input opened for this recording goes with it.
        drop(recording.own_input.take());
        if recording.dropped > 0 {
            self.pending_diagnostics
                .push_back(StudioDiagnostic::message(
                    "record",
                    format!(
                        "the sample lost {:.2}s the input wrote faster than it was read",
                        recording.dropped as f64 / f64::from(recording.ring.sample_rate().max(1))
                    ),
                ));
        }
        Some(recording.writer.finish())
    }

    fn recording_info(&self) -> Option<RecordingInfo> {
        let recording = self.recording.as_ref()?;
        let status = recording.writer.status();
        Some(RecordingInfo {
            path: status.path,
            seconds: recording.frames as f64 / f64::from(recording.sample_rate.max(1)),
            bytes: status.bytes,
            dropped_seconds: recording.dropped as f64 / f64::from(recording.sample_rate.max(1)),
            error: status.error,
        })
    }

    /// Hand one drained PCM batch and its trailing padding to the writer.
    /// Queue refusal still drops the whole batch; gap positions are unchanged.
    fn pump_recording(&mut self) {
        self.advance_recording_close();
        if self.close_recording_for_rate_change() {
            return;
        }
        let device = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref());
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        recording.scratch.clear();
        let result = recording.drain_input(device).and_then(|loss| {
            recording
                .submit(loss, recording.target_frames(), device.is_some())
                .map_err(str::to_owned)
        });
        if let Err(error) = result {
            recording.writer.fail(&error);
            self.close_recording(false);
            return;
        }
        if recording.input.is_none()
            && let Some(device) = device
        {
            match device.start_recording_capture() {
                Ok(capture) => {
                    recording.input = Some(RecordingInput {
                        capture,
                        sample_rate: device.sample_rate(),
                    })
                }
                Err(error) => {
                    recording.writer.fail(&error.to_string());
                    self.close_recording(false);
                }
            }
        }
    }

    /// Evaluate the editor buffer. If playback is stopped this also opens a
    /// fresh device and starts at cycle zero; otherwise it stages a continuous
    /// replacement and lets the producer publish the cutover transactionally.
    pub fn evaluate(&mut self, source: &str, mini: bool) -> Result<StudioInstall, RuntimeError> {
        self.evaluate_guarded(source, mini, || false)
    }

    /// Evaluate only while `cancelled` remains false. On a stopped engine the
    /// transport is armed before the guard is rechecked and is never cleared
    /// again inside startup, so a newer UI Stop cannot be lost to a restart
    /// race while the device is opening.
    pub fn evaluate_guarded(
        &mut self,
        source: &str,
        mini: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<StudioInstall, RuntimeError> {
        // A rewind sets its flag on the session before this call, and the
        // install consumes it. A refused score, a throw or a cancellation
        // installs nothing. A flag that stays set would rewind the next
        // unrelated save, so a failed evaluation clears it below.
        self.sync_polyphony();
        let result = self.evaluate_guarded_inner(source, mini, cancelled);
        self.sync_recovered_session();
        self.sync_polyphony();
        if result.is_err() {
            self.session.clear_next_from_zero();
        }
        // Taken by the install; one that never ran leaves it for nobody.
        self.next_install_preview = false;
        result
    }

    fn evaluate_guarded_inner(
        &mut self,
        source: &str,
        mini: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<StudioInstall, RuntimeError> {
        // Evaluating while a tail is still ringing out means "play this
        // now": finish the stop at once and start fresh from cycle zero.
        if self.is_stopping() {
            let _ = self.stop(self.config.stop_timeout);
        }
        if self.live.is_none() {
            self.session.transport().start();
            if cancelled() {
                self.session.transport().stop();
                return Err(RuntimeError::Cancelled);
            }
            return self.evaluate_and_start_armed(source, mini);
        }

        if cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if self.start_is_held() {
            return self.replace_held_start(source, mini);
        }
        // An edit played now supersedes a launch still waiting, and one
        // fired but not yet heard.
        self.supersede_launch();
        self.reload_live(source, mini)
    }

    /// Run one setup file on the session heap.
    ///
    /// Setup is not a score and this is not [`Self::evaluate_guarded`]: no
    /// transport is started, no device opened, no graph replaced, no
    /// generation cut, no launch cancelled. What it leaves behind is what
    /// the next score can call.
    ///
    /// `shutdown` cancels it, not the transport's stopped flag. The stopped
    /// flag is set from the moment a stop is requested until the next
    /// evaluate starts the transport again. If the evaluator read it, every
    /// setup applied from a stopped studio would be cancelled, and most
    /// setups are written there. The interface thread sets it as soon as
    /// the key is pressed, so it could also abort a setup mid-turn and
    /// leave the heap partly changed. A Stop concerns the transport, and a
    /// setup does not touch the transport. Shutdown still interrupts a
    /// runaway setup through QuickJS's interrupt handler, and the
    /// evaluation budget bounds it in both cases.
    pub fn evaluate_prebake_guarded(
        &mut self,
        source: &str,
        shutdown: &std::sync::atomic::AtomicBool,
    ) -> Result<(), RuntimeError> {
        self.session.consume_audio_confirmations();
        let now = self
            .live
            .as_ref()
            .map_or(0.0, |live| live.device.clock_seconds());
        let result = self
            .session
            .evaluate_prebake_cancellable_at(source, now, shutdown);
        self.sync_recovered_session();
        result?;
        // A setup is where `samples(...)` and `preload(...)` belong, so
        // start to load what it names now and do not wait for the first
        // score to ask. The setup is not the score: it does not take the
        // score's place here, so the sweep keeps the playing score's
        // sounds. The setup is already applied, so a failed warm-up is
        // reported and does not fail the setup.
        if let Err(error) = self.warm_names(source, SETUP_SAMPLE_WARM_BUDGET) {
            self.sync_recovered_session();
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::runtime(&error, true),
            );
        }
        Ok(())
    }

    /// Replace the sounding score continuously: the reload is staged, the
    /// producer publishes the cutover transactionally.
    fn reload_live(&mut self, source: &str, mini: bool) -> Result<StudioInstall, RuntimeError> {
        let preview = std::mem::take(&mut self.next_install_preview);
        self.ensure_input();
        self.session.consume_audio_confirmations();
        self.sync_played_score();
        // A reload evaluates the score's own tempo; when an outside clock is
        // followed the next tick puts it straight back, not a fifth of a
        // second later.
        if self.clock_in.is_some() {
            self.clock_followed_at = Instant::now() - midi_clock::FOLLOW_INTERVAL;
        }
        let mut shield_audio = Vec::new();
        {
            let live = self.live.as_mut().expect("checked live playback");
            let device = &live.device;
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
            let shield = live.producer.shield_reload_with_clock(
                &mut self.session,
                || device.clock_seconds(),
                device.sample_rate(),
                |event| {
                    let pushed = device.push(event);
                    if pushed {
                        played.note(&event, device.render_frontier_frames());
                        reverbs.observe(device, &event);
                        shield_audio.push(UiAcceptedOnset {
                            generation: event.generation,
                            onset_id: event.onset_id,
                            frequency_hz: Some(event.freq_hz),
                            gain: Some(event.gain),
                        });
                    }
                    pushed
                },
            );
            let preparation_started = Instant::now();
            reverbs.flush(device);
            self.session
                .record_live_asset_preparation(preparation_started.elapsed());
            shield?;
        }
        self.pending_accepted_audio.extend(shield_audio);
        self.session.consume_audio_confirmations();
        // The replacement's collect would replace what the shield's pass
        // staged before the tick drains: the [`ShieldHold`] takes what must
        // wait, and MIDI short of an armed line goes to the bridge now.
        #[cfg(any(feature = "osc", feature = "serial"))]
        self.shield_hold.take_staged(&mut self.session);
        {
            let (device_now, armed_line, sample_rate) = {
                let device = &self.live.as_ref().expect("checked live playback").device;
                (
                    device.clock_seconds(),
                    device.armed_line_frame(),
                    device.sample_rate(),
                )
            };
            let mut midi = self.session.take_pending_midi();
            self.shield_hold
                .hold_midi_past(&mut midi, armed_line, sample_rate);
            self.dispatch_midi(midi, device_now);
        }
        let (generation_before, generation_after) = {
            let live = self.live.as_ref().expect("checked live playback");
            self.session
                .set_schedule_lead(live.device.schedule_lead_seconds());
            self.session
                .set_continuity_margin(live.device.continuity_margin_seconds());
            let generation_before = self.session.generation();
            let transport = self.session.transport();
            let generation_after = self.session.reload_with_clock_cancellable(
                source,
                mini,
                transport.stopped_flag(),
                || live.device.clock_seconds(),
            )?;
            (generation_before, generation_after)
        };
        let live = self
            .live
            .as_mut()
            .expect("live playback survived evaluation");
        live.producer
            .arm_replacement(generation_before, generation_after);
        // A real score now owns this transport. It may have reused the
        // silent device an audition opened, but the score's cycle must keep
        // running after the audition finishes.
        live.audition_owned = false;
        self.warm_source(source, LIVE_RELOAD_SAMPLE_WARM_BUDGET)?;
        self.note_install_for_played(generation_after, preview, source);
        self.follow_edit(source, mini);
        // The room has been replaced by `source`, whatever the launch or edit
        // that asked for it: a fired rewind remembered for the grace no
        // longer speaks for what sounds.
        self.recent_rewind = None;
        Ok(StudioInstall {
            generation: generation_after,
            source_revision: source_revision(source),
            pending_cutover: true,
            answered_repeat: false,
        })
    }

    /// Open a fresh device and evaluate once. The first producer turn finishes
    /// anchoring cycle zero after input and sample preparation.
    pub fn evaluate_and_start(
        &mut self,
        source: &str,
        mini: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        if self.live.is_some() {
            return self.evaluate(source, mini);
        }

        self.session.transport().start();
        self.evaluate_and_start_armed(source, mini)
    }

    fn evaluate_and_start_armed(
        &mut self,
        source: &str,
        mini: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        debug_assert!(self.live.is_none());
        self.sync_polyphony();
        // The live device carries the input from here: the monitor's stream
        // goes before the output opens, so the two never hold one microphone.
        self.input_monitor = None;
        if self
            .piano_output
            .as_ref()
            .is_none_or(|device| device.input_name().is_none())
        {
            self.input_opened_for = None;
        }
        self.sync_input_channels();

        let was_piano_output = self.piano_output.is_some();
        let mut device = match self
            .piano_output
            .take()
            .map_or_else(|| self.open_output(), Ok)
        {
            Ok(device) => device,
            Err(error) => {
                self.session.transport().stop();
                return Err(runtime_device_error(error));
            }
        };
        if let Err(error) = self
            .session
            .bind_audio_confirmations(device.confirmations())
        {
            self.session.transport().stop();
            self.restore_failed_start_device(device, was_piano_output);
            return Err(error);
        }
        if let Err(error) = device.arm_callback_tripwire_checked() {
            self.session.transport().stop();
            // A piano output may already carry a real callback violation.
            // Retire it so the next launch opens a fresh reporting window.
            device.stop();
            drop(device);
            return Err(RuntimeError::Audio(error.to_string()));
        }

        let lead_deadline = Instant::now() + DEVICE_LEAD_WAIT;
        while device.report().playback_latency_nanos == 0 && Instant::now() < lead_deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.start_on(device, source, mini, was_piano_output)
    }

    /// Evaluate `source` onto a freshly opened `device` and make it the
    /// live playback. Cycle zero is anchored by the first producer turn,
    /// once the start's first window has loaded in wait mode.
    fn start_on(
        &mut self,
        device: LiveScalarDevice,
        source: &str,
        mini: bool,
        was_piano_output: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        let preview = std::mem::take(&mut self.next_install_preview);
        // Log what opened, once: the host, the device and the buffer
        // size. It is a log note, so it does not take the status line
        // from the install that follows ("playing from the top").
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::note("audio", device.audio_facts().output().describe_one_line()),
        );
        self.session
            .set_schedule_lead(device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(device.continuity_margin_seconds());

        let transport = self.session.transport();
        let evaluation = self.session.reload_at_cancellable(
            source,
            mini,
            device.clock_seconds(),
            transport.stopped_flag(),
        );
        let generation = match evaluation {
            Ok(generation) => generation,
            Err(error) => {
                transport.stop();
                self.restore_failed_start_device(device, was_piano_output);
                return Err(error);
            }
        };
        if transport.is_stopped() {
            self.restore_failed_start_device(device, was_piano_output);
            return Err(RuntimeError::Cancelled);
        }

        if let Err(error) = self
            .warm_source(source, STARTUP_SAMPLE_WARM_BUDGET)
            .and_then(|_| self.follow_start(source))
        {
            transport.stop();
            self.restore_failed_start_device(device, was_piano_output);
            return Err(error);
        }
        // Provisional UI mapping. Input opening, sample transfer and the
        // worker's first Hydra turn still happen before audio can be queued;
        // the first producer turn refreshes this anchor after that work.
        let anchor = device.clock_seconds()
            + device.schedule_lead_seconds()
            + self.config.start_preroll.as_secs_f64();
        self.session.restart_transport_at(anchor);
        // A restart has no outgoing audible generation. Discard any continuity
        // cursor created while replacing the retained stopped graph.
        let _ = self.session.take_requery_takeover();
        device.set_generation(generation, 0, TakeoverCut::None);
        device.set_analysis_enabled(false);
        device.set_visual_analysis_mask(0);
        self.sync_polyphony();
        device.set_max_polyphony(self.session.max_polyphony());
        device.set_master_gain(self.master.gain());
        // Set the limiter here as well as on every turn: a set or a setting
        // can ask for a limiter before the first tick reaches the device.
        device.set_limiter(self.master.limiter());
        device.set_limiter_makeup(self.master.limiter_makeup());

        let producer = match LiveFileProducer::unwatched(self.config.poll_interval) {
            Ok(mut producer) => {
                producer.adopt_recovery_epoch(&self.session);
                producer
            }
            Err(error) => {
                transport.stop();
                self.restore_failed_start_device(device, was_piano_output);
                return Err(error);
            }
        };
        self.ui.reset();
        // Each set gets its MIDI troubles told afresh.
        {
            self.midi_trouble_reported = [false; 4];
        }
        // And its serial ones. A port that was not there when the studio
        // opened is remembered as absent for good otherwise, so an adapter
        // plugged in later never gets its frames and never gets a word
        // about why. The outputs themselves stay: an open still in flight
        // from the last set cannot be called back, so this set adopts it
        // rather than asking the driver for the same port a second time.
        #[cfg(feature = "serial")]
        {
            self.serial_outputs.begin_set();
            self.serial_reported = std::collections::BTreeSet::new();
        }
        self.piano_has_score_tail = false;
        self.piano_tail_silent_since = None;
        self.stopped_device_time = None;
        // The install below starts the performer's score, unless it is a
        // preview's.
        self.awaiting_score = true;
        self.live = Some(StudioPlayback {
            registry: Arc::new(rustel_runtime::capability_registry_for_dispatch(
                device.dispatch(),
            )),
            device,
            producer,
            initial_start_generation: Some(generation),
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

        self.note_install_for_played(generation, preview, source);

        Ok(StudioInstall {
            generation,
            source_revision: source_revision(source),
            pending_cutover: false,
            answered_repeat: false,
        })
    }

    /// What an install means for what the score has sounded. A snippet
    /// previewed under the set counts from the sets as they stood, and the
    /// score put back - any install that is not a preview - returns to
    /// them: see [`Self::played_before_preview`]. A different text starts
    /// [`Self::played_by_text`] afresh, and a transport starts on it.
    fn note_install_for_played(&mut self, generation: u64, preview: bool, source: &str) {
        if preview {
            if self.played_before_preview.is_none() {
                self.played_before_preview =
                    Some((self.played_by_score.clone(), self.played_by_text.clone()));
            }
            return;
        }
        if let Some((by_score, by_text)) = self.played_before_preview.take() {
            self.played_by_score = by_score;
            self.played_by_text = by_text;
            self.played_from_generation = generation;
            self.protection_generation = self.protection_generation.wrapping_add(1);
            self.idle_swept = false;
        }
        if self.played_text.as_deref() != Some(source) {
            self.played_by_text.clear();
            self.text_from_generation = generation;
            self.played_text = Some(source.to_owned());
            self.played_reset_pending = true;
        }
        if std::mem::take(&mut self.awaiting_score) {
            self.played_by_score.clone_from(&self.played_by_text);
        }
    }

    /// A published cutover can still be awaiting callback confirmation.
    /// Keep its rollback material until that confirmation, and restore the
    /// conservative union if the producer has put the previous text back.
    fn sync_played_score(&mut self) {
        if !self.played_reset_pending || self.played_before_preview.is_some() {
            return;
        }
        let Some(source) = self.session.active_source() else {
            return;
        };
        let generation = self.session.generation();
        if self.current_score != source {
            let source = source.to_owned();
            self.restore_played_score(&source);
            if let Err(error) = self.warm_source(&source, LIVE_RELOAD_SAMPLE_WARM_BUDGET) {
                self.sync_recovered_session();
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::runtime(&error, true),
                );
                return;
            }
        }
        if self.session.confirmed_audio_generation() == Some(generation) {
            self.played_by_score.clone_from(&self.played_by_text);
            self.played_from_generation =
                self.played_from_generation.max(self.text_from_generation);
            self.played_reset_pending = false;
            self.protection_generation = self.protection_generation.wrapping_add(1);
            self.idle_swept = false;
        }
    }

    fn restore_played_score(&mut self, source: &str) {
        self.played_by_text.clone_from(&self.played_by_score);
        if self.played_text.is_some() {
            self.played_text = Some(source.to_owned());
        }
        self.text_from_generation = self.session.generation();
    }

    /// The transport has ended: what its last text sounded stays while a
    /// tab holds that text, and the rest goes, a snippet previewed under
    /// it included.
    fn end_played(&mut self) {
        if let Some((by_score, by_text)) = self.played_before_preview.take() {
            self.played_by_score = by_score;
            self.played_by_text = by_text;
        }
        self.played_from_generation = 0;
        self.text_from_generation = 0;
        self.played_reset_pending = false;
        if self.played_text.is_none() {
            self.played_by_text.clear();
        }
        self.played_by_score.clone_from(&self.played_by_text);
    }

    /// Open the output on silence, for a preview or a take while nothing
    /// plays. Silence is no score of the performer's, so what the last one
    /// sounded stays kept for it.
    fn start_silence(&mut self) -> Result<(), RuntimeError> {
        self.next_install_preview = true;
        self.evaluate_and_start("silence", false).map(drop)
    }

    /// Say whether the score handed over next is a snippet previewed under
    /// the set rather than the performer's own: see
    /// [`Self::played_before_preview`]. The worker says it with every
    /// evaluation, the way it plants a rewind.
    pub fn mark_next_install_preview(&mut self, preview: bool) {
        self.next_install_preview = preview;
    }

    fn restore_failed_start_device(&mut self, device: LiveScalarDevice, was_piano_output: bool) {
        if was_piano_output {
            self.piano_output = Some(device);
        } else {
            device.stop();
        }
    }

    fn open_output(&self) -> Result<LiveScalarDevice, rustel_audio::DevicePlaybackError> {
        let mut options = rustel_audio::LiveOutputOptions::default()
            .with_dispatch(self.session.config().dsp_dispatch);
        if let Some(frames) = self.config.output_buffer_frames {
            options =
                options.with_buffer_preference(rustel_audio::AudioBufferPreference::Frames(frames));
        }
        let device = LiveScalarDevice::start_output_with_options(
            self.preferred_output.as_deref(),
            self.session.generation(),
            options,
        )?;
        if let Some(library) = self.session.sample_library() {
            library.set_render_rate(device.sample_rate());
        }
        Ok(device)
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
            // The bank went with the device; any batons still owed are owed
            // to nobody, and every retiring id is free.
            self.service_idle_piano();
            self.tend_idle_sample_memory();
            return Ok(StudioTick::Idle);
        }
        if let Some(live) = self.live.as_mut() {
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
                    let _ = self.stop(Duration::from_millis(100));
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
            let _ = self.stop(Duration::from_millis(100));
            return Err(error);
        }

        if let Err(error) = self.recycle_stalled_device(observed_at) {
            let _ = self.stop(self.config.stop_timeout);
            return Err(error);
        }
        let asset_install_started = Instant::now();
        self.retry_pending_uninstalls();
        self.install_ready_samples();
        self.enforce_sample_memory(Instant::now());
        self.release_retired_samples();
        self.session
            .record_live_asset_preparation(asset_install_started.elapsed());
        if self.hold_start(&mut emit) {
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
            let result = live.producer.step_unwatched_with_clock_and_cutover(
                &mut self.session,
                || device.render_frontier_seconds(),
                device.sample_rate(),
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
                    let pushed = device.push(event);
                    if pushed {
                        played.note(&event, device.render_frontier_frames());
                        reverbs.observe(device, &event);
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
            self.piano_has_score_tail = false;
            self.ui.reset();
            StudioTick::Idle
        } else {
            self.stop(self.config.stop_timeout)
                .map(|stop| StudioTick::Stopped(Box::new(stop)))
                .unwrap_or(StudioTick::Idle)
        }
    }

    /// Stop is idempotent at the Session level and consumes the current device
    /// and producer. A later [`Self::evaluate`] creates fresh playback state.
    ///
    /// This is the immediate form: the device declicks over ten milliseconds
    /// and drains. A user-facing Stop goes through [`Self::request_stop`] and
    /// lets the tail ring out first.
    pub fn stop(&mut self, timeout: Duration) -> Option<StudioStop> {
        // What its last text sounded stays for that text's next start.
        self.end_played();
        self.end_loads();
        if let Some(live) = self.live.as_ref() {
            self.stopped_device_time
                .get_or_insert_with(|| live.device.clock_seconds());
            // The score is no longer kept for being the score, so what the
            // last sweep decided was still wanted is decided again: a tab
            // closed while it played goes with the next idle sweep.
            self.idle_swept = false;
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
        let device = self
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
        drop(device);
        // The device's rings, pools and reverbs are tens of megabytes.
        self.owe_free_memory();
        self.sync_input_channels();
        // The callback is joined and its ring popped; the producer went
        // with the device. Nothing can read a retiring id now.
        self.release_every_retiring_sample();
        self.session.consume_audio_confirmations();
        self.ui.reset();
        Some(StudioStop {
            acknowledged: acknowledged && report.stop_acknowledged,
            report,
        })
    }

    fn warm_source(&mut self, source: &str, query_budget: Duration) -> Result<(), RuntimeError> {
        let window = self.warm_names(source, query_budget)?;
        // A different score names different sounds, so what the last sweep
        // decided was still wanted has to be decided again.
        if self.current_score != source {
            self.idle_swept = false;
            self.current_score = source.to_owned();
            self.current_score_names = protected_names(source);
            self.protection_generation = self.protection_generation.wrapping_add(1);
        }
        if window != self.score_window_names {
            self.score_window_names = window;
            self.protection_generation = self.protection_generation.wrapping_add(1);
        }
        Ok(())
    }

    /// Start the sounds a text names loading, and query the installed
    /// pattern's window, without making the text the score. Answers the
    /// sounds that window resolved: see [`Session::kick_sample_loads_within`].
    fn warm_names(
        &mut self,
        source: &str,
        query_budget: Duration,
    ) -> Result<Vec<(String, Variants)>, RuntimeError> {
        let names = rustel_runtime::sounds::to_warm(source, self.every_variant());
        // An update can happen deep into an alternation: warm the window
        // about to play, with its actual note and variant choices first.
        // Source-only bets must not put unused GM fonts ahead of these.
        let from_cycle = if self.live.is_some() {
            self.session.cycle_at_time(self.device_time())
        } else {
            0.0
        };
        // Querying is bounded and delivery remains asynchronous; Ctrl-Enter
        // never waits for a remote sample download.
        let now = self.device_time();
        let (window, warmed) = self.session.with_panic_recovery(now, |session| {
            Ok(session.warm_live_samples_checked(&names, from_cycle, query_budget))
        })?;
        if let Err(error) = warmed {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("sample-prefetch", error.to_string()),
            );
        }
        Ok(window)
    }

    /// Start an armed launch's sounds loading without making it the
    /// current score.
    ///
    /// The playing score still names what the sample sweep must keep. If
    /// the launch became the current score here, the playing score's
    /// samples would count as unused during the countdown, and for good
    /// after a cancelled launch, and the idle sweep could retire them
    /// while the set still sounds them. The playing pattern is not queried
    /// either: until the launch lands, the only installed pattern is the
    /// outgoing one, and its sounds are already loaded. The incoming score
    /// is not installed yet, so its names come from its text. They include
    /// the `<bank>_<name>` that a `.bank()` builds, which a plain scan
    /// misses.
    fn warm_incoming(&mut self, source: &str) {
        let names = rustel_runtime::sounds::to_warm(source, self.every_variant());
        if names.is_empty() {
            return;
        }
        if self.session.sample_library().is_none()
            && let Err(error) = self.session.enable_default_samples()
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("sample-prefetch", error.to_string()),
            );
            return;
        }
        let Some(library) = self.session.sample_library() else {
            return;
        };
        if let Err(error) =
            library.warm_score_sounds_async(&names, &self.session.config().score_sample_access)
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("sample-prefetch", error),
            );
        }
    }

    fn collect_session_diagnostics(&mut self) {
        for failure in self.session.take_sample_failures() {
            let diagnostic = self.import_failure(failure);
            queue_diagnostic(&mut self.pending_diagnostics, diagnostic);
        }
        for diagnostic in self.session.take_diagnostics() {
            // The studio log does not list the voice resolver's notices.
            if diagnostic.kind == rustel_runtime::VOICE_NOTICE_DIAGNOSTIC {
                continue;
            }
            // Said once, in one line, when the load is over.
            if diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC {
                self.note_late(&diagnostic.message);
                continue;
            }
            // Nothing was skipped: the header's loading line follows the
            // sound until its notes play.
            if diagnostic.kind == rustel_runtime::SAMPLE_AWAITED_DIAGNOSTIC {
                continue;
            }
            let level = if diagnostic.kind == "live-error" {
                StudioDiagnosticLevel::Error
            } else if diagnostic.kind == "log" {
                // A `.log()` line is the score talking, not complaining:
                // information, so the studio's log panel keeps it in its
                // quiet floor and nothing paints it as a failure.
                StudioDiagnosticLevel::Info
            } else {
                StudioDiagnosticLevel::Warning
            };
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic {
                    kind: diagnostic.kind,
                    message: diagnostic.message,
                    recoverable: diagnostic.recoverable,
                    level,
                    alert: None,
                },
            );
        }
    }

    fn install_ready_samples(&mut self) {
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            return;
        };
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let mut ready = library.take_ready().into_iter();
        while let Some((id, decoded)) = ready.next() {
            let retained = decoded.clone();
            if let Err(decoded) = device.install_sample(id, decoded) {
                let mut retry = Vec::with_capacity(ready.len().saturating_add(1));
                retry.push((id, decoded));
                retry.extend(ready);
                library.requeue_ready_batch_before_newer(retry);
                break;
            }
            if (id.0 as usize) < SAMPLE_BANK_CAPACITY {
                self.sample_last_used.insert(id, Instant::now());
                self.retained_samples.insert(id, retained);
            }
        }
        device.reclaim_samples();
    }

    /// Snapshot the ready bank before either fallible output preparation.
    /// The library merge keeps newer publications and forgotten ids ahead
    /// of retained copies. Arrivals after this drain stay on its ready queue.
    fn collect_recycle_samples(&mut self) -> Vec<(SampleId, DecodedSample)> {
        let library = self.session.sample_library();
        let ready = if let Some(library) = library {
            requeue_retained_samples(library, &self.retained_samples);
            library.take_ready()
        } else {
            self.retained_samples
                .iter()
                .map(|(id, decoded)| (*id, decoded.clone()))
                .collect()
        };
        let mut slots = vec![None; SAMPLE_BANK_CAPACITY];
        for (id, decoded) in ready {
            let Some(slot) = slots.get_mut(id.0 as usize) else {
                continue;
            };
            if library
                .is_none_or(|library| library.decoded_identity(id) == Some(decoded.identity()))
            {
                *slot = Some(decoded);
            }
        }
        let samples: Vec<_> = slots
            .into_iter()
            .enumerate()
            .filter_map(|(slot, decoded)| decoded.map(|decoded| (SampleId(slot as u32), decoded)))
            .collect();
        // Keep ownership even if preparation tears down the old output and
        // then fails or Stop wins. Stop's normal requeue can recover these.
        let now = Instant::now();
        for (id, decoded) in &samples {
            self.retained_samples.insert(*id, decoded.clone());
            self.sample_last_used.insert(*id, now);
        }
        samples
    }

    fn requeue_recycle_samples(&self, samples: Vec<(SampleId, DecodedSample)>) {
        if let Some(library) = self.session.sample_library() {
            library.requeue_ready_batch_before_newer(samples);
        }
    }

    /// Keep a stopped engine's samples to the memory policy.
    ///
    /// The worker runs the playing tick only while there is a device, so
    /// this is the only place a stopped studio enforces the preview budget
    /// and the idle sweep. It used to live on the tick's no-device branch,
    /// which the worker never reaches: once stopped, nothing was ever
    /// dropped, and what browsing decoded stayed for the session.
    ///
    /// With no device at all the bank went with it, so every retiring id is
    /// free. A piano output keeps a bank, and its own path.
    fn tend_idle_sample_memory(&mut self) {
        if self.live.is_some() || self.piano_output.is_some() {
            return;
        }
        self.release_every_retiring_sample();
        self.adopt_idle_ready_samples();
        let now = Instant::now();
        self.enforce_sample_memory(now);
        self.free_memory_when_due(now);
    }

    /// Count what the loaders finished while no output is open as retained.
    ///
    /// With no device, nothing installs a finished decode. It waits in the
    /// library's ready queue, and the preview budget and the idle sweep
    /// count only what the engine retains. Browsing the Examples while
    /// stopped warms every row the cursor rests on, whole soundfonts
    /// included, so without this step a stopped studio grows by every
    /// sound it passes while the budget reads under its ceiling. Retaining
    /// the queue is the state a stop already leaves behind (retained, and
    /// requeued for the next device), so the policy that follows applies
    /// unchanged: what it forgets leaves the queue with the library's
    /// tables, and the next device installs only what is left.
    fn adopt_idle_ready_samples(&mut self) {
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let retained = &self.retained_samples;
        let waiting = library.peek_ready_unless(|id| retained.contains_key(&id));
        if waiting.is_empty() {
            return;
        }
        let now = Instant::now();
        for (id, decoded) in waiting {
            if (id.0 as usize) < SAMPLE_BANK_CAPACITY {
                self.sample_last_used.insert(id, now);
                self.retained_samples.insert(id, decoded);
            }
        }
    }

    /// Preview RAM policy from the settings sheet. Zero budget means no
    /// ceiling; zero idle means unused samples are never dropped by time.
    pub fn set_sample_memory_policy(&mut self, budget_bytes: usize, idle: Duration) {
        self.preview_budget_bytes = budget_bytes;
        self.unused_sample_idle = idle;
        self.enforce_sample_memory(Instant::now());
    }

    /// What the performer can reach now, from the studio: see
    /// [`LiveMaterial`]. Its sounds are protected from the preview ceiling
    /// and the idle sweep from the next turn.
    pub fn set_live_material(&mut self, material: &LiveMaterial) {
        // Only a tab leaving asks the sweep again, not every text sent: a
        // text goes after each pause in typing, and a name half deleted
        // mid-edit would be swept and decoded again a moment later.
        if material.tabs_closed != self.live_tabs_closed {
            self.live_tabs_closed = material.tabs_closed;
            self.idle_swept = false;
            // The tab gone may be the one the score played from: once no
            // tab holds its text, what it sounded goes at Stop, or now.
            let mut open = material.pinned.iter().chain(&material.recent);
            if self
                .played_text
                .as_deref()
                .is_some_and(|played| !open.any(|text| **text == *played))
            {
                self.played_text = None;
                if self.live.is_none() {
                    self.played_by_text.clear();
                    self.played_by_score.clear();
                }
            }
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut pinned: Vec<(String, Variants)> = Vec::new();
        for text in &material.pinned {
            for (name, variants) in protected_names(text) {
                match seen.get(&name) {
                    Some(&at) => pinned[at].1.merge(&variants),
                    None => {
                        seen.insert(name.clone(), pinned.len());
                        pinned.push((name, variants));
                    }
                }
            }
        }
        let recent: Vec<Vec<(String, Variants)>> = material
            .recent
            .iter()
            .map(|text| protected_names(text))
            .collect();
        if pinned == self.live_pinned
            && recent == self.live_recent
            && material.setups_select_variants == self.live_setups_select_variants
        {
            return;
        }
        self.live_pinned = pinned;
        self.live_recent = recent;
        self.live_setups_select_variants = material.setups_select_variants;
        self.protection_generation = self.protection_generation.wrapping_add(1);
    }

    /// Whether a setup picks variants - one open in a tab, or one this
    /// session has applied - so that every text keeps and warms every
    /// variant of what it names: a setup's helper can set `n` for any
    /// score.
    fn every_variant(&self) -> bool {
        self.live_setups_select_variants || self.session.prebake_selects_variants()
    }

    /// What protection is worked out against: the texts, an armed launch
    /// and whether a score plays, which change only when the performer
    /// does something; and the retained set and the
    /// library's settled epoch, which move with every sound that lands.
    fn protection_key(&self) -> ProtectionKey {
        let launch = self.pending_launch.as_ref().map_or(0, |pending| {
            pending.boundary_cycle.to_bits() ^ ((pending.source.len() as u64) << 1) | 1
        }) ^ self
            .waiting_source()
            .map_or(0, |source| (source.len() as u64) << 32);
        let playing = u64::from(self.live.is_some());
        let every_variant = u64::from(self.every_variant());
        // A font's zones reach the queue, and so the retained set, a moment
        // before the font reads Ready; the library's settled epoch moves
        // once it does, so what a tab names is looked for again then.
        let settled = self
            .session
            .sample_library()
            .map_or(0, |library| library.settled_epoch());
        ProtectionKey {
            texts: self.protection_generation
                ^ launch.rotate_left(17)
                ^ (playing << 62)
                ^ (every_variant << 61),
            landed: (self.retained_fingerprint(), settled),
            played: self.played_by_score.len(),
            in_use: self.sample_use_until.len(),
            allowance: self.recent_tabs_allowance(),
        }
    }

    /// Bring [`Self::protection`] up to date if it may be, and say whether
    /// it is: the ids no memory policy may drop - the bundled bd, what the
    /// score playing names, the always-kept tabs, and the recently visited
    /// tabs that fit (see [`LiveMaterial`]).
    ///
    /// Working it out asks the library for every name of every tab, and the
    /// ceiling asks on every turn once a set's own sounds pass it, so while
    /// sounds keep landing it is worked out again only every
    /// [`PROTECTION_REFRESH`], and reads stale until then.
    fn refresh_protection(&mut self, now: Instant) -> bool {
        let key = self.protection_key();
        if let Some(protection) = &self.protection {
            if protection.key == key {
                return true;
            }
            if protection.key.texts == key.texts
                && protection.key.allowance == key.allowance
                && now.saturating_duration_since(protection.worked_out_at) < PROTECTION_REFRESH
            {
                return false;
            }
        }
        let (ids, recent) = self.work_out_protected_ids();
        self.protection = Some(Protection {
            key,
            ids,
            recent,
            worked_out_at: now,
        });
        true
    }

    /// Every protected id, and those among them kept only for a recently
    /// visited tab.
    fn work_out_protected_ids(
        &self,
    ) -> (
        std::collections::HashSet<SampleId>,
        std::collections::HashSet<SampleId>,
    ) {
        let mut wanted = std::collections::HashSet::new();
        let mut recent = std::collections::HashSet::new();
        wanted.insert(rustel_audio::BUNDLED_BD_SAMPLE_ID);
        let Some(library) = self.session.sample_library() else {
            return (wanted, recent);
        };
        let launch = self
            .pending_launch
            .as_ref()
            .map(|pending| pending.source.as_str())
            .into_iter()
            .chain(self.waiting_source())
            .flat_map(protected_names)
            .collect::<Vec<_>>();
        // A setup that picks variants can set `n` for any text: each name
        // then keeps every variant, whatever its own text says.
        let every_variant = self.every_variant();
        let variants = |sound| widened(sound, every_variant);
        // A launch counting down to its line: what it names must be there
        // when it lands, and a launch cancelled lets it go again.
        // The score is kept for being the score only while it sounds: what
        // its text names, and what its pattern resolved for the window
        // about to play, from the first turn on. Once stopped, its tab
        // keeps what it names if the tab is still open - the one last
        // heard is always kept - and a closed tab, the last one played
        // included, holds nothing.
        let score = self
            .live
            .is_some()
            .then(|| {
                self.current_score_names
                    .iter()
                    .chain(&self.score_window_names)
            })
            .into_iter()
            .flatten();
        let mut ready = library.ready_ids();
        wanted
            .extend(ready.of_variants(score.chain(&self.live_pinned).chain(&launch).map(variants)));
        // A font is forgotten whole, so an in-use zone keeps its siblings.
        wanted.extend(self.played_by_score.iter().copied());
        wanted.extend(library.whole_fonts_of(&self.played_by_score));
        let in_use = self.sample_use_until.keys().copied().collect();
        wanted.extend(library.whole_fonts_of(&in_use));
        wanted.extend(in_use);
        // The oldest visit goes first: each tab is kept whole or not at
        // all, and a tab that does not fit ends the list, so a big tab
        // visited long ago cannot outstay the smaller ones after it. A
        // sound a kept tab shares with it is kept by that tab.
        let allowance = self.recent_tabs_allowance();
        let mut kept_bytes = 0usize;
        for tab in &self.live_recent {
            // Only what the tab can play is its to keep, and only that is
            // counted against the allowance.
            let fresh: Vec<SampleId> = ready
                .of_variants(tab.iter().map(variants))
                .into_iter()
                .filter(|id| !wanted.contains(id))
                .collect();
            let bytes: usize = fresh
                .iter()
                .filter_map(|id| self.retained_samples.get(id))
                .map(DecodedSample::pcm_bytes)
                .sum();
            if kept_bytes.saturating_add(bytes) > allowance {
                break;
            }
            kept_bytes += bytes;
            recent.extend(fresh.iter().copied());
            wanted.extend(fresh);
        }
        (wanted, recent)
    }

    /// How much decoded sound the recently visited tabs may keep:
    /// [`RECENT_TABS_BYTES`], or the most one sound may hold when the
    /// performer has raised that past it. A tab that does not fit ends the
    /// list, so one sound at a raised "biggest sound" in the tab visited
    /// last left every recent tab unprotected.
    fn recent_tabs_allowance(&self) -> usize {
        self.recent_tabs_bytes
            .unwrap_or_else(|| RECENT_TABS_BYTES.max(rustel_audio::sample_pcm_ceiling()))
    }

    /// Changes whenever an id is retained or let go, or a slot is refilled
    /// with a body of another size: what protection is worked out against.
    fn retained_fingerprint(&self) -> u64 {
        self.retained_samples.iter().fold(
            self.retained_samples.len() as u64,
            |hash, (id, sample)| {
                hash ^ ((u64::from(id.0) << 40) ^ sample.pcm_bytes() as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            },
        )
    }

    /// Take an id out of the engine's hands and start it on its way back
    /// to the library: the bank is told to empty the slot, and the id waits
    /// in `retiring` until nothing can read it.
    fn retire_sample(&mut self, id: SampleId, quiet_after_frame: u64) {
        if id == rustel_audio::BUNDLED_BD_SAMPLE_ID
            || self.retiring.iter().any(|retiring| retiring.id == id)
        {
            return;
        }
        // Retained means installed: the retention follows a successful ring
        // push, and a refused install is requeued, never retained.
        let installed = self.retained_samples.remove(&id).is_some();
        self.sample_last_used.remove(&id);
        let Some(live) = self.live.as_ref() else {
            // No device: no bank holds it, no ring or voice reads it, and
            // the stop that took the device dropped the producer with it.
            if let Some(library) = self.session.sample_library() {
                library.release_ids([id]);
            }
            return;
        };
        let baton = if !installed {
            Baton::NotOwed
        } else if live.device.uninstall_sample(id) {
            Baton::Pushed {
                callbacks: live.device.callbacks(),
            }
        } else {
            // The install ring is full: the baton is owed, not lost. The
            // bank keeps the PCM until the retry lands.
            Baton::Owed
        };
        live.device.reclaim_samples();
        self.retiring.push(RetiringSample {
            id,
            baton,
            quiet_after_frame,
        });
    }

    /// Retry the uninstalls the bank refused last turn. The callback drains
    /// the install ring between blocks, so one turn is headroom enough in
    /// practice; whatever is still refused waits for the next. A retiring
    /// id cannot have been re-retained in the meantime: it is in no table,
    /// and the ready queue refuses the tombstone the library left on it.
    fn retry_pending_uninstalls(&mut self) {
        if !self
            .retiring
            .iter()
            .any(|retiring| retiring.baton == Baton::Owed)
        {
            return;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        for retiring in &mut self.retiring {
            if retiring.baton == Baton::Owed && live.device.uninstall_sample(retiring.id) {
                retiring.baton = Baton::Pushed {
                    callbacks: live.device.callbacks(),
                };
            }
        }
        live.device.reclaim_samples();
    }

    /// Hand the library every retiring id nothing can read any more.
    ///
    /// Three things have to be true of an id: its baton landed and two
    /// more callbacks completed, so a whole block ran with the slot empty
    /// and every voice on it retired; the device's committed frontier is
    /// past the last frame any event queued before the forget - score or
    /// preview - could target; and nothing at or before that frame is
    /// still waiting in the producer's backlog.
    fn release_retired_samples(&mut self) {
        if self.retiring.is_empty() {
            return;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let callbacks = live.device.callbacks();
        let frame = live.device.clock_frames();
        let backlog_floor = live.producer.pending_target_floor().unwrap_or(u64::MAX);
        let audition_horizon = self.audition_horizon_frame;
        let mut released = Vec::new();
        self.retiring.retain(|retiring| {
            let cleared = match retiring.baton {
                Baton::NotOwed => true,
                Baton::Owed => false,
                Baton::Pushed { callbacks: pushed } => {
                    callbacks >= pushed.saturating_add(BATON_SETTLE_CALLBACKS)
                }
            };
            let quiet_after = retiring.quiet_after_frame.max(audition_horizon);
            let quiet = frame > quiet_after && backlog_floor > quiet_after;
            if cleared && quiet {
                released.push(retiring.id);
                false
            } else {
                true
            }
        });
        if !released.is_empty() {
            library.release_ids(released);
        }
    }

    /// The device, its bank, its ring and its voices are gone, or rebuilt
    /// from what the engine still retains, and no retiring id is in that
    /// set. The backlog that fed the device is empty. So every retiring id
    /// is free, baton owed or not, and the use horizons of accepted notes
    /// are cleared.
    fn release_every_retiring_sample(&mut self) {
        if !self.sample_use_until.is_empty() {
            self.sample_use_until.clear();
            self.protection_generation = self.protection_generation.wrapping_add(1);
            self.idle_swept = false;
        }
        if self.retiring.is_empty() {
            return;
        }
        let released: Vec<SampleId> = self
            .retiring
            .drain(..)
            .map(|retiring| retiring.id)
            .collect();
        if let Some(library) = self.session.sample_library() {
            library.release_ids(released);
        }
    }

    #[cfg(test)]
    pub(crate) fn retain_sample_for_test(&mut self, id: SampleId, sample: DecodedSample) {
        self.sample_last_used.insert(id, Instant::now());
        self.retained_samples.insert(id, sample);
    }

    #[cfg(test)]
    pub(crate) fn owes_uninstall(&self, id: SampleId) -> bool {
        self.retiring
            .iter()
            .any(|retiring| retiring.id == id && retiring.baton == Baton::Owed)
    }

    #[cfg(test)]
    pub(crate) fn retiring_count(&self) -> usize {
        self.retiring.len()
    }

    #[cfg(test)]
    pub(crate) fn retained_sample_count(&self) -> usize {
        self.retained_samples.len()
    }

    #[cfg(test)]
    pub(crate) fn force_sample_idle_for_test(&mut self, ago: Duration) {
        self.last_preview_at = Instant::now().checked_sub(ago).unwrap_or_else(Instant::now);
        self.idle_swept = false;
        // The same time has passed for what protection was worked out.
        if let Some(protection) = &mut self.protection {
            protection.worked_out_at = protection
                .worked_out_at
                .checked_sub(ago)
                .unwrap_or(protection.worked_out_at);
        }
        self.enforce_sample_memory(Instant::now());
    }

    /// A preview happened: the idle clock restarts, and what the last
    /// sweep decided is no longer the answer.
    fn note_preview(&mut self) {
        self.last_preview_at = Instant::now();
        self.idle_swept = false;
    }

    fn enforce_sample_memory(&mut self, now: Instant) {
        if let Some(live) = &self.live {
            let before = self.sample_use_until.len();
            let frame = live.device.clock_frames();
            self.sample_use_until.retain(|_, until| frame <= *until);
            if self.sample_use_until.len() != before {
                self.protection_generation = self.protection_generation.wrapping_add(1);
                self.idle_swept = false;
            }
        }
        // This runs every turn, so the cheap refusals come first: asking
        // the score what it names scans the source and locks the library's
        // tables, and none of that is owed while there is nothing to drop.
        if self.retained_samples.is_empty() {
            return;
        }
        let idle_elapsed = !self.idle_swept
            && !self.unused_sample_idle.is_zero()
            && now.saturating_duration_since(self.last_preview_at) >= self.unused_sample_idle;
        if !idle_elapsed && self.preview_budget_bytes == 0 {
            return;
        }
        // Extras are bounded by everything retained, so the score is only
        // asked for once the total can actually be over the ceiling.
        if !idle_elapsed {
            let total: usize = self
                .retained_samples
                .values()
                .map(DecodedSample::pcm_bytes)
                .sum();
            if total <= self.preview_budget_bytes {
                return;
            }
        }
        if !self.refresh_protection(now) {
            return;
        }
        let Some(protection) = &self.protection else {
            return;
        };
        // Backpressure can leave dynamic sample choices in the producer
        // before they are accepted by the device and gain a use horizon.
        let mut pending: std::collections::HashSet<_> = self
            .live
            .as_ref()
            .into_iter()
            .flat_map(|live| live.producer.pending_sample_ids())
            .collect();
        if let Some(library) = self.session.sample_library() {
            pending.extend(library.whole_fonts_of(&pending));
        }
        let wanted = |id: &SampleId| protection.ids.contains(id) || pending.contains(id);
        let mut doomed: std::collections::HashSet<SampleId> = std::collections::HashSet::new();
        if idle_elapsed {
            doomed.extend(
                self.retained_samples
                    .keys()
                    .copied()
                    .filter(|id| !wanted(id)),
            );
        }
        let mut spared = None;
        if self.preview_budget_bytes != 0 {
            let mut extras: Vec<(Instant, usize, SampleId)> = self
                .retained_samples
                .iter()
                .filter(|(id, _)| !wanted(id) && !doomed.contains(id))
                .map(|(id, sample)| {
                    let used = self
                        .sample_last_used
                        .get(id)
                        .copied()
                        .unwrap_or(self.last_preview_at);
                    (used, sample.pcm_bytes(), *id)
                })
                .collect();
            extras.sort_by_key(|(used, _, _)| *used);
            // The newest extra is the one the last preview added. It can be
            // in a pending audition: a preview does not sound a sample that
            // is not retained, so dropping it leaves the browser in a
            // Loading wait until the audition times out. Or it can be
            // sounding now, and the callback reads the bank by id when the
            // event fires. So the ceiling spares it, as it spares
            // score-named samples that exceed the ceiling by themselves.
            spared = extras.pop().map(|(_, _, id)| id);
            let mut extra_bytes: usize = extras.iter().map(|(_, bytes, _)| *bytes).sum();
            for (_, bytes, id) in extras {
                if extra_bytes <= self.preview_budget_bytes {
                    break;
                }
                extra_bytes = extra_bytes.saturating_sub(bytes);
                doomed.insert(id);
            }
        }
        if idle_elapsed {
            // One sweep per quiet spell: the next preview, a score that
            // names something else, a stop or a tab leaving is what asks
            // for another.
            self.idle_swept = pending.is_empty();
        }
        if doomed.is_empty() {
            return;
        }
        // The library publishes decoded PCM once, through the ready queue,
        // and afterwards remembers only the id. Dropping that PCM without
        // saying so leaves its tables promising a sample nobody holds: the
        // sound previews as "did not load in time", and a score that names
        // it plays silence, for the rest of the session. Forgetting first
        // is what makes the next ask decode it again - off the sample
        // cache on disk, so it costs no network.
        //
        // A font is forgotten whole, so a part-dropped one names its
        // untouched zones back here: they are no use on their own and the
        // ids about to be re-issued are not theirs.
        // Frozen before the library forgets: nothing scheduled after the
        // forget can name these ids, so this is the last frame anything
        // that can is due at.
        let quiet_after_frame = self
            .live
            .as_ref()
            .map_or(0, |live| live.producer.scheduled_through_frame());
        if let Some(library) = self.session.sample_library().cloned() {
            let orphaned = library.forget_decoded(&doomed);
            doomed.extend(orphaned);
        }
        doomed.retain(|id| Some(*id) != spared);
        if !doomed.is_empty() {
            self.owe_free_memory();
        }
        for id in doomed {
            self.retire_sample(id, quiet_after_frame);
        }
    }

    /// Something large was let go: hand its memory back once idle.
    ///
    /// The first debt sets the moment, and later ones do not move it, so a
    /// stop followed by a string of sample releases ends in one call rather
    /// than waiting for the releases to stop.
    fn owe_free_memory(&mut self) {
        self.free_memory_due
            .get_or_insert_with(|| Instant::now() + FREE_MEMORY_DELAY);
    }

    /// Hand freed memory back to the system when a release is owed, or when
    /// the engine has idled long enough that something it did not see may
    /// have been let go. Idle turns only: with no output open there is no
    /// producer for the allocator's locks to hold up.
    fn free_memory_when_due(&mut self, now: Instant) {
        let since_last = now.saturating_duration_since(self.last_free_memory);
        let owed =
            self.free_memory_due.is_some_and(|due| now >= due) && since_last >= FREE_MEMORY_SPACING;
        if !owed && since_last < FREE_MEMORY_INTERVAL {
            return;
        }
        rustel_runtime::free_memory::release_free_memory();
        self.last_free_memory = now;
        self.free_memory_due = None;
    }

    /// Open the output again after its stream reported an error, keeping
    /// the music: the same reopen a stalled clock gets, asked for by a
    /// different symptom.
    ///
    /// Returns `false` when it is too soon to try again. A device that
    /// fails, is reopened and fails again within the cooldown will not
    /// recover, and a reopen on every turn would load the machine and
    /// hide the failure.
    fn reopen_failed_device(&mut self, observed_at: Duration) -> Result<bool, RuntimeError> {
        let live = self.live.as_ref().expect("checked playback");
        if live
            .last_recycle_at
            .is_some_and(|last| observed_at.saturating_sub(last) < DEVICE_RECYCLE_COOLDOWN)
        {
            return Ok(false);
        }
        let name = live.device.name().to_owned();
        self.session.consume_audio_confirmations();
        let samples = self.collect_recycle_samples();
        let transport = self.session.transport();
        let live = self.live.as_mut().expect("checked playback");
        let recycled = live
            .device
            .recycle_output_to_with_samples(None, &samples, || transport.is_stopped());
        self.session.consume_audio_confirmations();
        if let Err(error) = recycled {
            self.requeue_recycle_samples(samples);
            return Err(runtime_device_error(error));
        }
        if transport.is_stopped() {
            return Err(RuntimeError::Cancelled);
        }
        self.external_outputs().forget();
        self.piano = [None; PIANO_KEYS];
        let live = self.live.as_mut().expect("checked playback");
        live.last_recycle_at = Some(observed_at);
        live.progress_at = observed_at;
        live.progress_clock = live.device.clock_nanos();
        self.session
            .set_schedule_lead(live.device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(live.device.continuity_margin_seconds());
        arm_output_recovery_after_recycle(
            &mut self.session,
            &mut live.producer,
            live.device.clock_seconds(),
            live.device.sample_rate(),
        )?;
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::info(
                "audio-recycled",
                format!("audio output {name} stopped and was opened again"),
            ),
        );
        self.release_every_retiring_sample();
        Ok(true)
    }

    fn recycle_stalled_device(&mut self, observed_at: Duration) -> Result<(), RuntimeError> {
        let live = self.live.as_mut().expect("checked playback");
        let clock = live.device.clock_nanos();
        if clock != live.progress_clock {
            live.progress_clock = clock;
            live.progress_at = observed_at;
            return Ok(());
        }
        if observed_at.saturating_sub(live.progress_at) < DEVICE_PROGRESS_DEADLINE
            || live
                .last_recycle_at
                .is_some_and(|last| observed_at.saturating_sub(last) < DEVICE_RECYCLE_COOLDOWN)
        {
            return Ok(());
        }

        self.session.consume_audio_confirmations();
        let samples = self.collect_recycle_samples();
        let transport = self.session.transport();
        let live = self.live.as_mut().expect("checked playback");
        let recycled = live
            .device
            .recycle_output_to_with_samples(None, &samples, || transport.is_stopped());
        self.session.consume_audio_confirmations();
        if let Err(error) = recycled {
            self.requeue_recycle_samples(samples);
            return Err(runtime_device_error(error));
        }
        if transport.is_stopped() {
            return Err(RuntimeError::Cancelled);
        }
        self.external_outputs().forget();
        let live = self.live.as_mut().expect("checked playback");
        live.last_recycle_at = Some(observed_at);
        live.progress_at = observed_at;
        live.progress_clock = live.device.clock_nanos();
        self.session
            .set_schedule_lead(live.device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(live.device.continuity_margin_seconds());
        arm_output_recovery_after_recycle(
            &mut self.session,
            &mut live.producer,
            live.device.clock_seconds(),
            live.device.sample_rate(),
        )?;
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::info(
                "audio-recycled",
                format!(
                    "audio device {} was reopened after its clock stopped advancing",
                    live.device.name()
                ),
            ),
        );
        // As on an explicit output change, the seeded replacement and its
        // new producer backlog cannot refer to the old retiring ids.
        self.release_every_retiring_sample();
        Ok(())
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
        ..*event
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

impl StudioEngine {
    /// What the snippet shelf wants shown, if anything has changed.
    ///
    /// `Some(None)` means "give the screen back to the score"; `None` means
    /// nothing has been asked for since the last turn.
    #[cfg(feature = "hydra")]
    pub fn set_hydra_preview(&mut self, code: Option<String>) {
        self.hydra_preview = Some(code);
    }

    #[cfg(feature = "hydra")]
    pub fn set_hydra_theme(&mut self, code: Option<String>, webcam: bool) {
        self.hydra_theme = Some((code, webcam));
    }

    #[cfg(feature = "hydra")]
    pub fn set_hydra_frame_size(&mut self, wanted: rustel_runtime::hydra::HydraFrameRequest) {
        self.hydra_frame_size = Some(wanted);
    }

    /// Hand this turn's visuals to the window, and the window this turn's
    /// signals and sound.
    ///
    /// Runs on the engine worker, where the Session lives, so an `H(...)`
    /// pattern is sampled with exactly the settings and at exactly the cycle
    /// the audio is using. Nothing here blocks: the window is on its own
    /// thread behind a channel that drops rather than waits.
    ///
    /// Each distinct failure of the turn is queued with the engine's own
    /// diagnostics, which wait for room in the channel. The returned notice
    /// is informational, and a full channel may skip it.
    #[cfg(feature = "hydra")]
    pub fn drive_hydra(
        &mut self,
        hydra: &mut rustel_runtime::hydra::HydraBridge,
        audio: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>,
    ) -> Option<StudioDiagnostic> {
        let mut note = None;
        let mut trouble = Vec::new();
        let mut failure = |message: String| {
            if !trouble.contains(&message) {
                trouble.push(message);
            }
        };
        if let Some(wanted) = self.hydra_frame_size.take() {
            hydra.set_frames_wanted(wanted);
        }
        if let Some(preview) = self.hydra_preview.take()
            && let Err(message) = hydra.preview(preview.as_deref())
        {
            failure(message);
        }
        if let Some((theme, webcam)) = self.hydra_theme.take()
            && let Err(message) = hydra.theme_with_camera(theme.as_deref(), webcam)
        {
            failure(message);
        }
        if let Some(candidate) = self.session.take_pending_hydra() {
            match rustel_runtime::hydra::HydraUpdate::from_candidate(&candidate) {
                Ok(update) => {
                    if let Err(message) = hydra.apply_with_sample_access(
                        update,
                        &self.session.config().score_sample_access,
                    ) {
                        failure(message);
                    }
                }
                Err(message) => failure(message),
            }
        }
        for event in hydra.take_events() {
            match event {
                rustel_hydra::HydraEvent::Failed { message, .. } => failure(message),
                // Say once, in the log, which surface the visuals landed on.
                // Without it there is no way to tell a sketch drawing behind
                // the code from one drawing in a window somewhere else.
                rustel_hydra::HydraEvent::Ready { renderer } => {
                    note.get_or_insert_with(|| format!("visuals are drawing here ({renderer})"));
                }
                rustel_hydra::HydraEvent::Input {
                    slot,
                    message,
                    failed,
                } => {
                    let message = format!("Hydra s{slot}: {message}");
                    if failed {
                        failure(message);
                    } else {
                        note.get_or_insert(message);
                    }
                }
                _ => {}
            }
        }
        // A stopped set wipes the picture, the way the strudel.cc editor does.
        // The shelf's own picture is a second renderer with its own liveness,
        // so browsing does not keep the score's visuals alive - and inserting
        // a snippet leaves the screen still until you press play.
        hydra.set_drawing(self.is_playing());
        let now = Instant::now();
        if let Err(message) = hydra.tick(
            now,
            self.session.cycle_at_time(self.device_time()),
            self.session.cps(),
        ) {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("hydra", message),
            );
        }
        if let Some(frame) = audio.filter(|_| hydra.wants_audio()) {
            let rms = (frame.scope.iter().map(|s| s * s).sum::<f32>()
                / frame.scope.len().max(1) as f32)
                .sqrt();
            hydra.audio(&rustel_runtime::hydra::audio_frame(rms, &frame.spectrum));
        }
        for message in trouble {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("hydra", message),
            );
        }
        note.map(|message| StudioDiagnostic::info("hydra", message))
    }

    /// Where the audible clock is now; zero when nothing is open, which is
    /// the same answer the status bar gives, and the same one the snapshot
    /// carries.
    fn device_time(&self) -> f64 {
        self.live
            .as_ref()
            .map_or(0.0, |live| live.device.clock_seconds())
    }
}

impl StudioEngine {
    /// Hand this turn's `.osc()` bundles to the network through
    /// [`Self::dispatch_osc`], as the CLI's live loop does.
    #[cfg(feature = "osc")]
    fn send_pending_osc(&mut self, device_now: f64, step_failed: bool) {
        let pending = self.session.take_pending_osc();
        // A failed query pass sends nothing, as `.midi()` does: what it
        // staged is not a schedule, and a receiver that sounded it would
        // disagree with the audio for that cycle.
        if step_failed {
            return;
        }
        self.dispatch_osc(pending, device_now);
    }

    /// Put `.osc()` bundles on the wire: each carries the NTP time its onset
    /// is due - the device clock's distance to the onset's target, from now -
    /// and the receiver schedules on it, which is how Tidal drives
    /// SuperDirt and keeps the sender's jitter out of the sound. A socket
    /// that cannot be opened is reported once; the bundles of that turn are
    /// dropped rather than queued against a port that is not coming. A bundle
    /// once sent is not recallable.
    #[cfg(feature = "osc")]
    fn dispatch_osc(
        &mut self,
        pending: Vec<(f64, rustel_runtime::osc_bridge::OscOnset)>,
        device_now: f64,
    ) {
        if pending.is_empty() {
            return;
        }
        if self.osc_sender.is_none() && !self.osc_open_failed {
            match rustel_osc::OscSender::new() {
                Ok(sender) => self.osc_sender = Some(sender),
                Err(message) => {
                    self.osc_open_failed = true;
                    queue_diagnostic(
                        &mut self.pending_diagnostics,
                        StudioDiagnostic::message(
                            "osc",
                            format!("cannot open a socket for .osc(): {message}"),
                        ),
                    );
                }
            }
        }
        let Some(sender) = self.osc_sender.as_ref() else {
            return;
        };
        for (_lead_secs, intent) in pending {
            let Some(destination) = intent.destination else {
                continue;
            };
            let when = std::time::SystemTime::now() + output_wait(intent.target_time, device_now);
            if sender.send_dirt(destination, when, &intent.args) {
                // The bundle is out: it stands for the copy that a takeover
                // stages of the same onset.
                self.session
                    .note_osc_handed_out(intent.generation, intent.target_time);
                self.pending_accepted_audio.push(UiAcceptedOnset {
                    generation: intent.generation,
                    onset_id: intent.onset_id,
                    frequency_hz: None,
                    gain: None,
                });
            }
        }
    }

    #[cfg(not(feature = "osc"))]
    fn send_pending_osc(&mut self, _device_now: f64, _step_failed: bool) {}

    /// How `.midi()` ports are opened. The default opens the platform's;
    /// a test hands in one that captures what is written.
    pub fn set_midi_port_opener(
        &mut self,
        opener: std::sync::Arc<rustel_runtime::midi_bridge::MidiOpenFn>,
    ) {
        self.midi_outputs = rustel_runtime::midi_bridge::MidiOutputs::with_opener(opener);
        // The replacement counts from zero, so the snapshot the counters
        // are compared against has to start again too: left as it was, no
        // new trouble ever exceeds the old one and none is ever reported.
        self.midi_report = rustel_midi::MidiReport::default();
        self.midi_trouble_reported = [false; 4];
    }

    /// How `.serial()` ports are opened. The default opens the platform's;
    /// a test hands in one that captures what is written.
    ///
    /// Only the opener is swapped, unlike the MIDI hook, which replaces its
    /// outputs: ports already open, remembered or still opening stay as
    /// they are, so an open in flight is still adopted and no port is asked
    /// for twice. Ports named from here on open through `opener`.
    #[cfg(feature = "serial")]
    pub fn set_serial_port_opener(
        &mut self,
        opener: std::sync::Arc<rustel_runtime::serial_bridge::SerialOpenFn>,
    ) {
        self.serial_outputs.set_opener(opener);
    }

    /// How a score's `midin`/`midikeys` ports are opened. A test hands in
    /// one that records what was asked for instead of touching hardware.
    #[cfg(test)]
    pub(crate) fn set_midi_input_opener(
        &mut self,
        opener: std::sync::Arc<dyn rustel_runtime::midi_input::InputOpener>,
    ) {
        self.midi_inputs = Some(rustel_runtime::midi_input::quiet_inputs(opener));
    }

    /// The wall-clock time this turn's `.midi()` onset should sound: the
    /// device-clock instant the intent asked for, plus the master limiter's
    /// runway. The limiter holds the audio back after the device clock, so
    /// MIDI timed straight off that clock would fire ahead of the sound it
    /// belongs to. Off holds nothing back and adds nothing; a character
    /// change lands on the next scheduling turn, never on notes already
    /// placed. The offline `render`/`--export` path needs none of this: it
    /// consumes no pending MIDI, and runs the limiter only when asked.
    fn midi_instant_for(&mut self, device_now: f64, target_time: f64, sample_rate: u32) -> Instant {
        let runway = self.master.limiter_latency_frames(sample_rate);
        self.midi_clock.instant_for(device_now, target_time)
            + Duration::from_secs_f64(runway as f64 / f64::from(sample_rate))
    }

    /// Hand this turn's `.midi()` onsets to their ports through
    /// [`Self::dispatch_midi`], as the CLI's live loop does. A failed query
    /// pass sends nothing: what it staged is not a schedule.
    fn send_pending_midi(&mut self, device_now: f64, step_failed: bool) {
        let mut pending = self.session.take_pending_midi();
        if step_failed {
            pending.clear();
        }
        self.dispatch_midi(pending, device_now);
    }

    /// Hand `.midi()` onsets to their ports. Every intent of the audible or
    /// the session generation is planned whole, note-on and note-off
    /// together, timed from one clock that follows the device's, and
    /// admitted atomically; a port is reserved for its generation before
    /// anything is sent, so the retirement of the old generation's notes
    /// never cuts the new ones.
    fn dispatch_midi(
        &mut self,
        pending: Vec<rustel_runtime::midi_bridge::MidiOnset>,
        device_now: f64,
    ) {
        // Settled before the device is borrowed, because saying a port is
        // not enabled needs the diagnostics queue and the device is held for
        // the rest of this function. One pass over the distinct ports also
        // means the refusal is decided once a turn rather than once an
        // onset, whatever the score is striking.
        let mut refused_ports = std::collections::BTreeSet::new();
        let mut checked = std::collections::BTreeSet::new();
        for port in pending.iter().map(|intent| intent.port.clone()) {
            if !checked.insert(port.clone()) {
                continue;
            }
            if !self.midi_out_allowed(&port) {
                refused_ports.insert(port);
            }
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let device = &live.device;
        let audible = device.generation();
        let session_generation = self.session.generation();
        let live_generation =
            |generation: u64| generation == audible || generation == session_generation;
        for intent in &pending {
            if live_generation(intent.generation)
                && rustel_midi::has_output(&intent.controls)
                && !refused_ports.contains(&intent.port)
                && let Err(message) = self
                    .midi_outputs
                    .reserve_generation_port(intent.generation, &intent.port)
            {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("midi", message),
                );
            }
        }
        for notice in
            self.midi_outputs
                .poll_notices_at(audible, session_generation, device.clock_frames())
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                midi_output_diagnostic(notice),
            );
        }
        let sample_rate = device.sample_rate();
        for intent in pending {
            if !live_generation(intent.generation) {
                continue;
            }
            if refused_ports.contains(&intent.port) {
                continue;
            }
            let planned = rustel_midi::plan(&intent.controls, intent.duration_secs);
            if planned.is_empty() {
                continue;
            }
            let base = self.midi_instant_for(device_now, intent.target_time, sample_rate);
            let batch =
                self.midi_note_order
                    .stamp_batch(&intent.port, intent.target_time, base, &planned);
            match self.midi_outputs.submit_batch_for_generation_at(
                intent.generation,
                &intent.port,
                rustel_runtime::midi_bridge::onset_frame_at(intent.target_time, sample_rate),
                batch,
            ) {
                Ok(true) => self.pending_accepted_audio.push(UiAcceptedOnset {
                    generation: intent.generation,
                    onset_id: intent.onset_id,
                    frequency_hz: None,
                    gain: None,
                }),
                Ok(false) => {}
                Err(message) => queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("midi", message),
                ),
            }
        }
        self.note_midi_troubles();
    }

    /// Ask the pattern again, from close to now, when a key has been
    /// pressed since the last look.
    ///
    /// A `midikeys` press is heard when the scheduler next queries the
    /// pattern, and the scheduler queries up to its half-second horizon
    /// ahead. Without this re-query a key press can wait most of that
    /// horizon before it sounds.
    ///
    /// A slider move does the same: the score is re-queried from one
    /// continuity margin ahead, so the change is heard at once. A key
    /// press gets the same re-query, and its delay is that margin: a few
    /// milliseconds, not half a second.
    ///
    /// The engine counts presses to detect a key. The driver thread writes
    /// the ring and the producer reads it, and a counter is the cheapest
    /// value that crosses between the two threads.
    fn hear_midi_keys_now(&mut self) {
        let audible = self.live.as_ref().map_or_else(
            || self.session.generation(),
            |live| live.device.generation(),
        );
        let session = self.session.generation();
        let pressed: u64 = self
            .session
            .midi_input_bus()
            .snapshot_for(audible, session)
            .iter()
            .map(|port| port.keys.presses())
            .sum();
        if pressed == self.midi_keys_seen {
            return;
        }
        self.midi_keys_seen = pressed;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        // A start's first query reads the keys as they stand.
        if live.initial_start_generation.is_some() {
            return;
        }
        let now = live.device.clock_seconds();
        if let Ok(Some((before, after))) = self.session.requery_active_at(now) {
            let live = self.live.as_mut().expect("checked");
            live.producer.arm_control_requery(before, after);
        }
    }

    /// Open the ports the evaluated score asks to listen to, and close the
    /// ones it stopped asking for.
    ///
    /// Both generations go in, as the CLI's live loop passes them. A
    /// candidate score can be accepted and still miss its first scheduling
    /// window, and its input generation has to retire the failed one's
    /// ports.
    fn open_score_midi_inputs(&mut self) {
        let audible = self.live.as_ref().map_or_else(
            || self.session.generation(),
            |live| live.device.generation(),
        );
        let session = self.session.generation();
        let bus = self.session.midi_input_bus();
        // A studio that never opens a keyboard never starts the manager's
        // thread. Once a score has asked for a port the manager stays, so
        // that it is still there to close what the next score stops asking
        // for.
        if self.midi_inputs.is_none() && bus.snapshot_for(audible, session).is_empty() {
            return;
        }
        // Said once per port, before the gate drops it from what is opened:
        // a controller that simply never answers, with nothing in the log,
        // is the silence a player spends the first half of a set blaming on
        // their cable.
        use super::devices::MidiDirection;
        for port in bus.snapshot_for(audible, session) {
            if self.midi_enabled.allows(MidiDirection::In, &port.selector) {
                continue;
            }
            let name = self
                .midi_enabled
                .port_named(MidiDirection::In, &port.selector)
                .unwrap_or(&port.selector)
                .to_owned();
            if self.midi_disabled_reported.insert(format!("in:{name}")) {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message(
                        "midi",
                        format!(
                            "{name} is not enabled for MIDI in - switch it on in the devices panel"
                        ),
                    ),
                );
            }
        }
        let enabled = &self.midi_enabled;
        let messages = self
            .midi_inputs
            .get_or_insert_with(rustel_runtime::midi_input::MidiInputs::new)
            .sync_allowed(&bus, audible, session, |selector| {
                enabled.allows(MidiDirection::In, selector)
            });
        for message in messages {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("midi", message),
            );
        }
    }

    /// Whether a score may play this port, saying why once when it may not.
    ///
    /// Said once per port rather than once per onset: a disabled port struck
    /// four times a bar would otherwise fill the log with the same sentence
    /// faster than anyone could read the first one, and bury whatever else
    /// the set was trying to say.
    ///
    /// The panel is named rather than the key that opens it, because that key
    /// is rebindable and a message naming the wrong one is worse than a
    /// message naming none.
    ///
    /// The refusal names the port the selector LANDED on rather than what the
    /// score wrote: `.midi("Maschine")` refused as "Maschine MK3 EXT MIDI" is
    /// the row a player then has to find and tick, and "Maschine" is not.
    fn midi_out_allowed(&mut self, selector: &str) -> bool {
        use super::devices::MidiDirection;
        if self.midi_enabled.allows(MidiDirection::Out, selector) {
            return true;
        }
        let port = self
            .midi_enabled
            .port_named(MidiDirection::Out, selector)
            .unwrap_or(selector)
            .to_owned();
        if self.midi_disabled_reported.insert(format!("out:{port}")) {
            let message =
                format!("{port} is not enabled for MIDI out - switch it on in the devices panel");
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("midi", message),
            );
        }
        false
    }

    /// What the output ports' counters say went wrong since last looked,
    /// each kind told once a set: a full queue, late drops, a device
    /// rejecting messages, and errors the ports could not even keep.
    fn note_midi_troubles(&mut self) {
        let report = self.midi_outputs.report();
        let troubles = [
            (
                report.refused_full > self.midi_report.refused_full,
                "MIDI output queue filled; a complete onset was refused",
            ),
            (
                report.dropped_late > self.midi_report.dropped_late,
                "MIDI messages arrived too late and were dropped",
            ),
            (
                report.send_errors > self.midi_report.send_errors,
                "a MIDI output device rejected messages or disconnected",
            ),
            (
                report.errors_dropped > self.midi_report.errors_dropped,
                "MIDI errors were dropped before they could be reported",
            ),
        ];
        for (index, (happened, text)) in troubles.into_iter().enumerate() {
            if happened && !self.midi_trouble_reported[index] {
                self.midi_trouble_reported[index] = true;
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("midi", text),
                );
            }
        }
        self.midi_report = report;
    }

    /// Hand this turn's `.serial()` writes to their ports through
    /// [`Self::dispatch_serial`]. The platform open runs on a thread of the
    /// bridge's own, so a port that cannot be opened, or is still opening
    /// after a couple of seconds, is told here a turn later.
    #[cfg(feature = "serial")]
    fn send_pending_serial(&mut self, device_now: f64, step_failed: bool) {
        let pending = self.session.take_pending_serial();
        // Taken even on a failed pass, as MIDI's poll is, so a completed
        // open installs and a failure or a stalled open is told without
        // waiting for a good pass.
        for news in self.serial_outputs.poll() {
            // Once a set by text. The bridge tells each thing once, but two
            // selectors ("" and "default") can fail in the same words.
            if self.serial_reported.insert(news.message.clone()) {
                // A port still opening is news, not a fault - the CLI prints
                // it as waiting - so it goes to the log and the status line
                // at Info. A failed open and refused or dropped writes are
                // warnings, as MIDI's are.
                let diagnostic = match news.kind {
                    rustel_runtime::serial_bridge::SerialNewsKind::StillOpening => {
                        StudioDiagnostic::info("serial", news.message)
                    }
                    rustel_runtime::serial_bridge::SerialNewsKind::Failure
                    | rustel_runtime::serial_bridge::SerialNewsKind::Trouble => {
                        StudioDiagnostic::message("serial", news.message)
                    }
                };
                queue_diagnostic(&mut self.pending_diagnostics, diagnostic);
            }
        }
        // A failed query pass writes nothing, as `.midi()` does.
        if step_failed {
            return;
        }
        self.dispatch_serial(pending, device_now);
    }

    /// Put `.serial()` writes on the wire. The wire has no notion of later,
    /// so the sender schedules each write for its onset, plus the fixed
    /// latency strudel.cc adds, kept so a sketch tuned in the browser is not
    /// early here. A write in the sender or in the pre-open queue is not
    /// recallable within a set.
    #[cfg(feature = "serial")]
    fn dispatch_serial(
        &mut self,
        pending: Vec<(f64, rustel_runtime::serial_bridge::SerialOnset)>,
        device_now: f64,
    ) {
        for (_lead_secs, intent) in pending {
            let due = Instant::now()
                + output_wait(intent.target_time, device_now)
                + rustel_serial::SERIAL_LATENCY;
            let submitted =
                match self
                    .serial_outputs
                    .submit(&intent.port, intent.baud, due, intent.bytes)
                {
                    Ok(submitted) => submitted,
                    Err(message) => {
                        // Once a set. The refusal repeats on every onset of
                        // every tick otherwise, and the studio log is a bounded
                        // ring, so the repeat evicts everything else in it.
                        if self.serial_reported.insert(message.clone()) {
                            queue_diagnostic(
                                &mut self.pending_diagnostics,
                                StudioDiagnostic::message("serial", message),
                            );
                        }
                        continue;
                    }
                };
            // A baud note is news, not a refusal: the write still goes out,
            // at the baud the port has for this set, and the bridge dedupes
            // the note. That includes a restart's note that the last set's
            // open is being replaced at this set's baud.
            if let Some(note) = submitted.note {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("serial", note),
                );
            }
            // Accepted when written to an open port or held while it opens;
            // either way this turn has nothing left to send.
            if submitted.accepted {
                // The sender has the write: it stands for the copy that a
                // takeover stages of the same onset.
                self.session
                    .note_serial_handed_out(intent.generation, intent.target_time);
                self.pending_accepted_audio.push(UiAcceptedOnset {
                    generation: intent.generation,
                    onset_id: intent.onset_id,
                    frequency_hz: None,
                    gain: None,
                });
            }
        }
    }

    #[cfg(not(feature = "serial"))]
    fn send_pending_serial(&mut self, _device_now: f64, _step_failed: bool) {}

    /// Send what the [`ShieldHold`] has due within the steady cover of
    /// `device_now`, the span a steady drain sends ahead. A line cut that
    /// has fired takes its span out first, and one still ahead keeps what
    /// lies at or past it waiting. A stopped transport sounds nothing more,
    /// so its hold is dropped.
    fn release_shield_hold(&mut self, device_now: f64) {
        if self.session.transport().is_stopped() {
            self.shield_hold.clear();
            return;
        }
        let Some((armed_line, sample_rate)) = self
            .live
            .as_ref()
            .map(|live| (live.device.armed_line_frame(), live.device.sample_rate()))
        else {
            return;
        };
        let unfired_line = match armed_line {
            Some(line) if self.retire_past_fired_line(line) => None,
            line => line,
        };
        let released = self.shield_hold.release(
            device_now,
            self.session.live_producer_schedule_cover(),
            unfired_line,
            sample_rate,
        );
        if !released.midi.is_empty() {
            self.dispatch_midi(released.midi, device_now);
        }
        #[cfg(feature = "osc")]
        self.dispatch_osc(released.osc, device_now);
        #[cfg(feature = "serial")]
        self.dispatch_serial(released.serial, device_now);
    }

    /// Retire what the external outputs owe the outgoing score from
    /// `line_frame` once the render frontier has passed it, since a cut
    /// armed there has retired the score's audio. Returns whether the
    /// frontier had passed it.
    fn retire_past_fired_line(&mut self, line_frame: u64) -> bool {
        let Some((generation, sample_rate)) = self
            .live
            .as_ref()
            .filter(|live| line_passed(&live.device, line_frame))
            .map(|live| (live.device.generation(), live.device.sample_rate()))
        else {
            return false;
        };
        self.external_outputs()
            .retire_from(generation, line_frame, sample_rate);
        true
    }

    /// The outputs outside the device that follow the score's audio.
    fn external_outputs(&mut self) -> ExternalOutputs<'_> {
        ExternalOutputs {
            midi: &mut self.midi_outputs,
            hold: &mut self.shield_hold,
        }
    }
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
    mod input_retry {
        use super::*;

        #[derive(Clone, Copy)]
        enum InputStatus {
            Closed,
            Healthy,
            Failed,
        }

        struct InputHarness {
            engine: StudioEngine,
            live: bool,
            opens: Vec<String>,
        }

        impl InputHarness {
            fn new(live: bool) -> Self {
                let mut engine = if live {
                    silent_engine_for_output_selection()
                } else {
                    StudioEngine::new(StudioConfig::default()).expect("studio")
                };
                engine.set_input_device(Some("microphone"));
                engine.pending_diagnostics.clear();
                Self {
                    engine,
                    live,
                    opens: Vec::new(),
                }
            }

            fn turn(&mut self, now: Instant, status: InputStatus) {
                self.turn_with_result(now, status, Ok(()));
            }

            fn turn_with_result(
                &mut self,
                now: Instant,
                status: InputStatus,
                result: Result<(), DevicePlaybackError>,
            ) {
                let live = self.live;
                let opens = &mut self.opens;
                self.engine.ensure_input_with(
                    now,
                    |device, monitor| {
                        assert_eq!(device.is_some(), live, "live or idle input route");
                        assert!(monitor.is_none(), "the fixture opens no physical input");
                        match status {
                            InputStatus::Closed => (false, false),
                            InputStatus::Healthy => (true, false),
                            InputStatus::Failed => (true, true),
                        }
                    },
                    |_, wanted| {
                        opens.push(wanted.to_owned());
                        result.map(|()| (wanted.to_owned(), 2))
                    },
                );
            }

            fn refuse(&mut self, now: Instant) {
                self.turn_with_result(
                    now,
                    InputStatus::Closed,
                    Err(DevicePlaybackError::Unavailable("input busy".into())),
                );
            }
        }

        #[test]
        fn failed_live_and_idle_inputs_wait_before_reopening() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let now = Instant::now();
                input.turn(now, InputStatus::Closed);
                assert_eq!(input.opens, ["microphone"]);
                assert_eq!(input.engine.input_opened_for.as_deref(), Some("microphone"));

                input.engine.session.set_direct_diagnostic_logging(false);
                input.engine.session.set_audio_input_channels(Some(2));
                input
                    .engine
                    .session
                    .evaluate("s('in').n(3).fast(8)")
                    .expect("input score");
                input.turn(now, InputStatus::Failed);
                let deadline = now + Duration::from_millis(500);
                assert_eq!(input.opens.len(), 1, "failure must not reopen in this turn");
                assert_eq!(input.engine.input_opened_for, None, "failed input closed");
                assert_eq!(input.engine.input_announced, None);
                assert_eq!(input.engine.input_refused.as_deref(), Some("microphone"));
                assert_eq!(input.engine.input_attempts, 1);
                assert_eq!(input.engine.input_retry_at, deadline);
                assert_eq!(input.engine.snapshot().input_channels, 0);
                input.engine.session.transport().start();
                let events = input.engine.session.schedule_audio_at(0.0, 48_000).unwrap();
                assert!(!events.is_empty());
                assert!(events.iter().all(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 3 })
                )));
                let diagnostics = input.engine.pending_diagnostics.len();
                assert_eq!(diagnostics, 2, "one opening and one failure message");
                assert!(
                    input.engine.pending_diagnostics[1]
                        .message
                        .contains("will try again")
                );

                for millis in [0, 2, 100, 499] {
                    input.turn(now + Duration::from_millis(millis), InputStatus::Closed);
                    assert_eq!(input.opens.len(), 1);
                    assert_eq!(input.engine.input_attempts, 1);
                    assert_eq!(input.engine.input_retry_at, deadline);
                    assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);
                }
                input.turn(deadline, InputStatus::Closed);
                assert_eq!(input.opens, ["microphone", "microphone"]);
                assert_eq!(
                    input.engine.input_attempts, 1,
                    "opening is not a health check"
                );
                assert_eq!(input.engine.input_refused.as_deref(), Some("microphone"));
            }
        }

        #[test]
        fn repeated_stream_failures_increase_and_cap_the_delay() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let mut now = Instant::now();
                input.turn(now, InputStatus::Closed);
                for (index, millis) in [500, 1000, 2000, 4000, 8000, 16_000, 16_000, 16_000]
                    .into_iter()
                    .enumerate()
                {
                    now += Duration::from_millis(1);
                    input.turn(now, InputStatus::Failed);
                    let deadline = now + Duration::from_millis(millis);
                    assert_eq!(input.engine.input_attempts, (index + 1) as u64);
                    assert_eq!(input.engine.input_retry_at, deadline);
                    assert_eq!(input.opens.len(), index + 1);
                    let diagnostics = input.engine.pending_diagnostics.len();
                    assert_eq!(
                        input
                            .engine
                            .pending_diagnostics
                            .iter()
                            .filter(|diagnostic| diagnostic.message.contains("the input stopped"))
                            .count(),
                        1,
                        "report the first stream failure in the episode"
                    );

                    input.turn(deadline - Duration::from_nanos(1), InputStatus::Closed);
                    assert_eq!(input.opens.len(), index + 1);
                    assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);
                    input.turn(deadline, InputStatus::Closed);
                    assert_eq!(input.opens.len(), index + 2);
                    assert_eq!(input.engine.input_attempts, (index + 1) as u64);
                    now = deadline;
                }
            }
        }

        #[test]
        fn opening_errors_and_stream_failures_share_the_failure_episode() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let now = Instant::now();
                input.refuse(now);
                assert_eq!(input.engine.input_attempts, 1);
                assert_eq!(input.engine.pending_diagnostics.len(), 1);
                assert!(
                    input.engine.pending_diagnostics[0]
                        .message
                        .contains("input busy")
                );
                let retry = input.engine.input_retry_at;
                input.refuse(retry - Duration::from_nanos(1));
                assert_eq!(input.opens.len(), 1);
                assert_eq!(input.engine.input_retry_at, retry);
                input.refuse(retry);
                assert_eq!(input.opens.len(), 2);
                assert_eq!(input.engine.input_attempts, 2);
                assert_eq!(input.engine.input_retry_at, retry + Duration::from_secs(1));
                assert_eq!(
                    input.engine.pending_diagnostics.len(),
                    1,
                    "no repeated refusal"
                );

                let retry = input.engine.input_retry_at;
                input.turn(retry, InputStatus::Closed);
                assert_eq!(input.engine.input_attempts, 2);
                input.turn(retry, InputStatus::Failed);
                assert_eq!(input.opens.len(), 3);
                assert_eq!(input.engine.input_attempts, 3);
                assert_eq!(input.engine.input_retry_at, retry + Duration::from_secs(2));
                assert_eq!(
                    input.engine.pending_diagnostics.len(),
                    2,
                    "only the first refusal and the successful opening are reported"
                );
            }
        }

        #[test]
        fn a_healthy_observation_resets_the_failure_episode() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let now = Instant::now();
                input.refuse(now);
                input.refuse(input.engine.input_retry_at);
                let retry = input.engine.input_retry_at;
                input.turn(retry, InputStatus::Closed);
                assert_eq!(input.engine.input_attempts, 2);
                let diagnostics = input.engine.pending_diagnostics.len();
                input.turn(retry, InputStatus::Healthy);
                assert_eq!(input.opens.len(), 3, "healthy input is not reopened");
                assert_eq!(input.engine.input_refused, None);
                assert_eq!(input.engine.input_attempts, 0);
                assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);

                input.turn(retry, InputStatus::Failed);
                assert_eq!(input.engine.input_attempts, 1);
                assert_eq!(
                    input.engine.input_retry_at,
                    retry + Duration::from_millis(500)
                );
                assert_eq!(input.engine.pending_diagnostics.len(), diagnostics + 1);
                assert!(
                    input.engine.pending_diagnostics[diagnostics]
                        .message
                        .contains("the input stopped"),
                    "a healthy observation starts a new failure episode"
                );
            }
        }

        #[test]
        fn an_explicit_input_choice_can_retry_at_once() {
            for live in [false, true] {
                for wanted in ["microphone", "other microphone"] {
                    let mut input = InputHarness::new(live);
                    let now = Instant::now();
                    input.refuse(now);
                    let deadline = input.engine.input_retry_at;
                    input.engine.set_input_device(Some(wanted));
                    input.turn(now, InputStatus::Closed);
                    assert_eq!(input.opens, ["microphone", wanted]);
                    assert_eq!(input.engine.input_attempts, 0);
                    assert_eq!(input.engine.input_refused, None);
                    assert_eq!(input.engine.input_opened_for.as_deref(), Some(wanted));
                    assert!(now < deadline);

                    input.engine.set_input_device(None);
                    input.turn(now, InputStatus::Closed);
                    assert_eq!(input.opens.len(), 2, "no input means no opening attempt");
                    assert_eq!(input.engine.input_opened_for, None);
                }
            }
        }

        #[test]
        fn a_healthy_input_handoff_keeps_the_first_attempt_immediate() {
            let mut input = InputHarness::new(false);
            let now = Instant::now();
            input.turn(now, InputStatus::Closed);
            input.turn(now, InputStatus::Healthy);
            let diagnostics = input.engine.pending_diagnostics.len();

            input.engine.close_input();
            play_silently(&mut input.engine);
            input.live = true;
            input.turn(now, InputStatus::Closed);
            assert_eq!(input.opens.len(), 2);
            assert_eq!(input.engine.input_attempts, 0);
            assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);

            input.engine.close_input();
            input.engine.live = None;
            input.live = false;
            input.turn(now, InputStatus::Closed);
            assert_eq!(input.opens.len(), 3);
            assert_eq!(input.engine.input_attempts, 0);
            assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);
        }

        #[test]
        fn a_handoff_preserves_a_pending_input_retry() {
            let mut input = InputHarness::new(false);
            let now = Instant::now();
            input.turn(now, InputStatus::Closed);
            input.turn(now, InputStatus::Failed);
            let deadline = input.engine.input_retry_at;

            play_silently(&mut input.engine);
            input.live = true;
            input.turn(now, InputStatus::Closed);
            assert_eq!(input.opens.len(), 1);
            assert_eq!(input.engine.input_attempts, 1);
            assert_eq!(input.engine.input_retry_at, deadline);
            input.turn(deadline, InputStatus::Closed);
            assert_eq!(input.opens.len(), 2);
            assert_eq!(input.engine.input_attempts, 1);
        }
    }
    mod loading_pressure {
        use super::*;
        use rustel_audio::RealtimeLoadSnapshot;
        use rustel_runtime::{EnginePressureCause, EnginePressureLevel, ProducerLoadSnapshot};

        /// Sample-loading refusals leave the header healthy when device and producer
        /// timing are healthy, including across repeated snippet installations.
        #[test]
        fn rapid_snippet_installs_awaiting_samples_are_not_engine_pressure() {
            let mut engine = silent_engine_for_output_selection();
            engine.session.set_sample_library_for_test(Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("held"),
            ));
            engine
                .evaluate("setcps(0.5)\n$: s(\"held\").fast(2)", false)
                .expect("score whose sample is loading");

            let mut monitor = EnginePressureMonitor::default();
            let start = Instant::now();
            let healthy_device = LiveDeviceReport {
                stream_id: 1,
                realtime_load: RealtimeLoadSnapshot {
                    sample_rate_hz: 48_000,
                    last_callback_frames: 256,
                    last_callback_period_nanos: 5_333_333,
                    fast_load_basis_points: 1_000,
                    slow_load_basis_points: 1_000,
                    peak_load_basis_points: 1_000,
                    ..Default::default()
                },
                ..Default::default()
            };
            for spam in 0..12 {
                let snippet = format!(
                    "setcps(0.5)\n$: note(\"c{}\").s(\"held\").fast(4)",
                    spam % 8
                );
                engine.evaluate(&snippet, false).expect("preview install");
                for _ in 0..6 {
                    engine
                        .tick_at(Duration::from_millis(2), accepted)
                        .expect("tick");
                }
                let pressure = engine.snapshot().pressure.expect("pressure");
                assert_eq!(
                    pressure.producer_refusals, 0,
                    "a preview waiting on its samples is not a capacity refusal: {pressure:#?}"
                );
                assert_eq!(
                    pressure.producer.capacity_refusals(),
                    0,
                    "no capacity refusal may be hidden between snapshots: {pressure:#?}"
                );
                assert_eq!(
                    (
                        pressure.device.callback_errors,
                        pressure.device.ring_refusals
                    ),
                    (0, 0),
                    "loading must not hide a device refusal behind timing pressure: {pressure:#?}"
                );
                assert_ne!(
                    pressure.cause,
                    EnginePressureCause::Refusal,
                    "a preview waiting on its samples is not pressure: {pressure:#?}"
                );

                // Preserve the live refusal counters while controlling timing inputs.
                let healthy_producer = ProducerLoadSnapshot {
                    last_load_basis_points: 1_000,
                    fast_load_basis_points: 1_000,
                    slow_load_basis_points: 1_000,
                    peak_load_basis_points: 1_000,
                    consecutive_over_budget: 0,
                    ..pressure.producer
                };
                let healthy = monitor.sample_at(
                    healthy_device,
                    healthy_producer,
                    256,
                    start + Duration::from_millis(spam * 12),
                );
                assert_eq!(
                    healthy.cause,
                    EnginePressureCause::Healthy,
                    "loading alone must leave the header healthy: {healthy:#?}; live: {pressure:#?}"
                );
                assert_eq!(
                    healthy.level,
                    EnginePressureLevel::Normal,
                    "healthy timing and loading samples must leave the header normal: {healthy:#?}; live: {pressure:#?}"
                );
            }
            let pressure = engine.snapshot().pressure.expect("pressure");
            assert!(
                pressure.producer.loading_refusals > 0,
                "setup really did wait on the library: {pressure:#?}"
            );
        }
    }
    mod midi_diagnostics {
        use super::*;
        use rustel_runtime::midi_bridge::MidiOutputNotice;

        fn failed(port: &str) -> StudioDiagnostic {
            midi_output_diagnostic(MidiOutputNotice::OpenFailed {
                port: port.into(),
                message: format!("could not open {port}"),
            })
        }

        fn recovered(port: &str) -> StudioDiagnostic {
            midi_output_diagnostic(MidiOutputNotice::Opened { port: port.into() })
        }

        fn deliver_alerts(pending: &mut PendingDiagnostics, log: &mut crate::log::StudioLog) {
            emit_pending_diagnostics(pending, &mut |update| {
                let StudioUpdate::Diagnostic(diagnostic) = update else {
                    panic!("expected a diagnostic");
                };
                match diagnostic.alert {
                    Some(DiagnosticAlert::Raise(key)) => log.push_alert(
                        crate::log::Level::Warn,
                        &diagnostic.kind,
                        diagnostic.message,
                        key,
                    ),
                    Some(DiagnosticAlert::Resolve(key)) => {
                        assert!(log.resolve_alert(&key) > 0, "the warning must exist");
                    }
                    None => {}
                }
                Ok(())
            });
        }

        fn block_delivery(pending: &mut PendingDiagnostics) {
            emit_pending_diagnostics(pending, &mut |update| {
                Err((UiEventSendStatus::DroppedFull, update))
            });
        }

        #[test]
        fn recovered_midi_port_clears_only_its_warning_even_when_diagnostics_are_full() {
            assert_eq!(
                failed("Wavetable").alert,
                Some(DiagnosticAlert::Raise("midi:output:Wavetable".into()))
            );
            assert_eq!(
                recovered("Wavetable").alert,
                Some(DiagnosticAlert::Resolve("midi:output:Wavetable".into()))
            );
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(&mut pending, failed("Wavetable"));
            queue_diagnostic(&mut pending, failed("Other"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 2);

            queue_diagnostic(&mut pending, recovered("Wavetable"));
            for index in 0..(MAX_PENDING_DIAGNOSTICS * 2) {
                queue_diagnostic(
                    &mut pending,
                    StudioDiagnostic::message("test", format!("other warning {index}")),
                );
                assert!(pending.len() <= MAX_PENDING_DIAGNOSTICS);
            }
            block_delivery(&mut pending);
            assert_eq!(log.unseen(), 2, "the UI still has both warnings");
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 1, "the other port still needs attention");
        }

        #[test]
        fn successful_opens_without_warnings_do_not_fill_diagnostics() {
            let mut pending = PendingDiagnostics::default();
            for index in 0..(MAX_PENDING_DIAGNOSTICS * 4) {
                queue_diagnostic(&mut pending, recovered(&format!("port-{index}")));
                block_delivery(&mut pending);
                assert!(pending.is_empty());
            }
        }

        #[test]
        fn recovery_slots_stay_bounded_until_the_ui_accepts_them() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            for index in 0..MAX_PENDING_DIAGNOSTICS {
                queue_diagnostic(&mut pending, failed(&format!("port-{index}")));
                deliver_alerts(&mut pending, &mut log);
            }
            assert_eq!(log.unseen(), MAX_PENDING_DIAGNOSTICS);
            for index in 0..MAX_PENDING_DIAGNOSTICS {
                queue_diagnostic(&mut pending, recovered(&format!("port-{index}")));
            }
            for index in MAX_PENDING_DIAGNOSTICS..(MAX_PENDING_DIAGNOSTICS * 4) {
                let port = format!("port-{index}");
                queue_diagnostic(&mut pending, failed(&port));
                queue_diagnostic(&mut pending, recovered(&port));
                queue_diagnostic(
                    &mut pending,
                    StudioDiagnostic::message("test", "other warning"),
                );
                block_delivery(&mut pending);
                assert_eq!(pending.len(), MAX_PENDING_DIAGNOSTICS);
            }
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0, "every admitted warning clears");
            assert!(pending.is_empty());

            queue_diagnostic(&mut pending, failed("later-port"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(
                log.unseen(),
                1,
                "successful handoff releases admission capacity"
            );
            queue_diagnostic(&mut pending, recovered("later-port"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0);
        }

        #[test]
        fn renewed_failure_supersedes_recovery_while_the_ui_is_blocked() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(&mut pending, failed("Wavetable"));
            deliver_alerts(&mut pending, &mut log);
            queue_diagnostic(&mut pending, recovered("Wavetable"));
            block_delivery(&mut pending);
            queue_diagnostic(&mut pending, failed("Wavetable"));
            block_delivery(&mut pending);
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0], failed("Wavetable"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(
                log.unseen(),
                1,
                "the renewed failure must not clear the warning"
            );
            queue_diagnostic(&mut pending, recovered("Wavetable"));
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0, "recovery clears the warning for this port");
        }

        #[test]
        fn sample_recovery_survives_midi_diagnostic_pressure() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(
                &mut pending,
                StudioDiagnostic::message("sample-failed", "missing sample")
                    .raising("samples:import:kit"),
            );
            deliver_alerts(&mut pending, &mut log);
            queue_diagnostic(
                &mut pending,
                StudioDiagnostic::resolve("samples:import:kit"),
            );
            for index in 0..(MAX_PENDING_DIAGNOSTICS * 2) {
                queue_diagnostic(&mut pending, failed(&format!("port-{index}")));
                block_delivery(&mut pending);
                assert!(pending.len() <= MAX_PENDING_DIAGNOSTICS);
            }
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(
                log.resolve_alert("samples:import:kit"),
                0,
                "the queued recovery was delivered"
            );
            assert_eq!(log.unseen(), MAX_PENDING_DIAGNOSTICS - 1);
        }

        #[test]
        fn a_sample_warning_and_its_immediate_recovery_both_reach_the_log() {
            let mut pending = PendingDiagnostics::default();
            let mut log = crate::log::StudioLog::open(None);
            queue_diagnostic(
                &mut pending,
                StudioDiagnostic::message("sample-loading", "notes skipped while loading")
                    .raising("samples:late:1"),
            );
            queue_diagnostic(&mut pending, StudioDiagnostic::resolve("samples:late:1"));
            queue_diagnostic(&mut pending, StudioDiagnostic::resolve("samples:late:1"));
            block_delivery(&mut pending);
            assert_eq!(pending.len(), 2, "retain the warning and one recovery");
            deliver_alerts(&mut pending, &mut log);
            assert_eq!(log.unseen(), 0);
            assert!(pending.is_empty());
        }
    }
    mod panic_recovery {
        use super::*;
        use rustel_runtime::SessionPanicPoint;

        const GOOD: &str = "note('c3').s('sine').fast(8)";
        const CANDIDATE: &str = "note('g4').s('triangle').fast(8)";

        fn playing_score() -> StudioEngine {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine.session.evaluate(GOOD).expect("good score");
            play_silently(&mut engine);
            wait_for_audio(&mut engine);
            engine
        }

        fn wait_for_audio(engine: &mut StudioEngine) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                engine.tick(accepted).expect("engine stays open");
                if engine.session.confirmed_audio_generation() == Some(engine.generation()) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "the restored score did not sound"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn panicking_candidate_keeps_the_device_and_last_good_score() {
            for point in [
                SessionPanicPoint::Evaluation,
                SessionPanicPoint::ReplacementProbe,
                SessionPanicPoint::Install,
            ] {
                let mut engine = playing_score();
                let stream = engine.device_report().unwrap().stream_id;
                engine.session.inject_panic_for_test(point);
                let error = engine
                    .evaluate(CANDIDATE, false)
                    .expect_err("refused score");
                assert!(matches!(error, RuntimeError::Panic(_)), "{error}");
                assert!(engine.is_playing(), "panic closed the output");
                assert_eq!(engine.active_source(), Some(GOOD));
                assert_eq!(engine.device_report().unwrap().stream_id, stream);
                wait_for_audio(&mut engine);
                assert_eq!(engine.active_source(), Some(GOOD));
                engine.evaluate(CANDIDATE, false).expect("next edit works");
                wait_for_audio(&mut engine);
                assert_eq!(engine.active_source(), Some(CANDIDATE));
            }
        }

        #[test]
        fn scheduler_panic_is_reported_and_playback_recovers_on_the_same_device() {
            let mut engine = playing_score();
            let stream = engine.device_report().unwrap().stream_id;
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut panic = None;
            while panic.is_none() {
                engine
                    .tick(|update| {
                        if let StudioUpdate::Diagnostic(diagnostic) = update
                            && diagnostic.kind == "panic"
                        {
                            panic = Some(diagnostic);
                        }
                        Ok(())
                    })
                    .expect("query panic is contained");
                assert!(Instant::now() < deadline, "panic was not reported");
                std::thread::sleep(Duration::from_millis(5));
            }
            let panic = panic.unwrap();
            assert_eq!(panic.level, StudioDiagnosticLevel::Error);
            assert!(panic.recoverable);
            assert!(panic.message.contains("panic"));
            assert!(engine.is_playing());
            assert_eq!(engine.active_source(), Some(GOOD));
            assert_eq!(engine.device_report().unwrap().stream_id, stream);
            wait_for_audio(&mut engine);
        }

        #[test]
        fn visual_preview_panic_is_reported_and_the_next_producer_turn_recovers() {
            let mut engine = playing_score();
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let live = engine.live.as_ref().unwrap();
            engine.ui.after_step(
                &mut engine.session,
                &live.device,
                Duration::from_secs(10),
                true,
                false,
                Vec::new(),
                &mut engine.pending_diagnostics,
                &mut accepted,
            );
            assert!(engine.pending_diagnostics.iter().any(|d| d.kind == "panic"));
            assert_eq!(engine.active_source(), Some(GOOD));
            wait_for_audio(&mut engine);
        }

        #[test]
        fn launch_pad_filter_survives_a_rebuilt_session() {
            let mut engine = playing_score();
            engine.set_launch_pads(vec![(60, 1)]);
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Evaluation);
            engine
                .evaluate(CANDIDATE, false)
                .expect_err("injected panic");
            let (_, port) = engine.session.midi_input_bus().intern("keyboard").unwrap();
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
            let mut hits = Vec::new();
            port.keys.select(
                0.0,
                4.0,
                rustel_core::midi_in::now_nanos(),
                Some((1, 8)),
                &mut hits,
            );
            assert!(
                hits.is_empty(),
                "scene launch pads must not become notes after recovery"
            );
        }

        #[test]
        fn loading_window_query_panic_is_a_refused_score() {
            let mut engine = playing_score();
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let error = engine.follow_start(GOOD).expect_err("loading query panic");
            assert!(matches!(error, RuntimeError::Panic(_)));
            assert_eq!(engine.active_source(), Some(GOOD));
            wait_for_audio(&mut engine);
        }

        #[test]
        fn a_start_whose_first_query_panics_stops_its_transport() {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine.session.transport().start();
            let device =
                LiveScalarDevice::start_silent(48_000, engine.generation()).expect("silent output");
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let error = engine
                .start_on(device, GOOD, false, false)
                .expect_err("start query panicked");
            assert!(matches!(error, RuntimeError::Panic(_)), "{error}");
            assert!(!engine.is_playing());
            assert!(engine.session.transport().is_stopped());
        }

        #[test]
        fn a_setup_stays_applied_when_the_playing_scores_warm_up_panics() {
            let mut engine = playing_score();
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            engine
                .evaluate_prebake_guarded(
                    "globalThis.setupNote = 48;",
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .expect("the setup applied");
            assert_eq!(engine.session.active_source(), Some(GOOD));
        }
    }
    mod sample_reclamation {
        use super::*;

        fn tick_until_the_score_is_confirmed(engine: &mut StudioEngine) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.played_reset_pending
                || engine.session.confirmed_audio_generation() != Some(engine.session.generation())
            {
                assert!(Instant::now() < deadline, "the score never became audible");
                engine
                    .tick(|_| Ok(()))
                    .expect("confirm the score's cutover");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// Replacing a score lets its old sounds go after their last queued note
        /// finishes. A computed name in the replacement stays without stopping
        /// the transport, even though no text scan can find it.
        #[test]
        fn a_live_edit_releases_what_only_the_previous_text_sounded() {
            let mut engine = engine_with_five_sounds(0);
            let (kick, pad) = (SampleId(70), SampleId(71));
            let first = "s(\"kick\")";
            // Built in JavaScript, so only what it sounded keeps the pad.
            let second = "const name = 'p' + 'ad'; s(name)";
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from(first), Arc::from(second)],
                ..LiveMaterial::default()
            });
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate(first, false).expect("the first text");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            engine.evaluate(second, false).expect("the second text");
            tick_until_the_text_sounds(&mut engine, pad);
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(!engine.played_by_score.contains(&kick));
            assert!(!engine.played_by_text.contains(&kick));
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from(second)],
                tabs_closed: 1,
                ..LiveMaterial::default()
            });
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.retained_samples.contains_key(&kick) {
                assert!(
                    Instant::now() < deadline,
                    "the previous score's kick stayed"
                );
                engine
                    .tick(|_| Ok(()))
                    .expect("keep the replacement playing");
                std::thread::sleep(Duration::from_millis(2));
            }

            assert!(engine.is_playing(), "reclamation needs no Stop");
            assert_eq!(retained_names(&engine), ["pad"]);
            assert_eq!(
                engine.played_by_score,
                std::collections::HashSet::from([pad])
            );
        }

        /// An old score's long sample remains available while its accepted notes
        /// can still read it. When they finish, the same idle spell gets another
        /// sweep without a preview, an edit or a Stop to trigger it.
        #[test]
        fn an_outgoing_samples_tail_rearms_the_idle_sweep_when_it_finishes() {
            let mut engine = engine_with_five_sounds(0);
            let kick = SampleId(70);
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine
                .evaluate("s(\"kick\").speed(0.01).slow(4)", false)
                .expect("the long sample");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            engine.evaluate("silence", false).expect("remove the sound");
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(!engine.played_by_score.contains(&kick));
            let end = *engine
                .sample_use_until
                .get(&kick)
                .expect("accepted sample use survives the edit");
            assert!(
                engine.live.as_ref().unwrap().device.clock_frames() < end,
                "the slow sample still has a tail"
            );

            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));
            assert!(engine.idle_swept, "the first sweep kept the playing tail");
            let deadline = Instant::now() + Duration::from_secs(8);
            while engine.retained_samples.contains_key(&kick) {
                assert!(
                    Instant::now() < deadline,
                    "the completed tail stayed retained"
                );
                engine.tick(|_| Ok(())).expect("play through the tail");
                if engine.live.as_ref().unwrap().device.clock_frames() <= end {
                    assert!(
                        engine.retained_samples.contains_key(&kick),
                        "the bank must remain available through the accepted horizon"
                    );
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(engine.is_playing());
            assert!(!engine.sample_use_until.contains_key(&kick));
        }

        /// Repeated replacements do not accumulate the generations heard earlier
        /// in the run, including an outgoing generation shielded during reload.
        #[test]
        fn repeated_live_edits_keep_only_the_current_texts_played_set() {
            let mut engine = engine_with_five_sounds(0);
            engine.unused_sample_idle = Duration::ZERO;
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            for (name, id) in [("kick", 70), ("pad", 71), ("snare", 72), ("hat", 73)] {
                let source = format!("const name = '{name}'; s(name)");
                engine.evaluate(&source, false).expect("replacement");
                tick_until_the_text_sounds(&mut engine, SampleId(id));
                tick_until_the_score_is_confirmed(&mut engine);
                assert_eq!(
                    engine.played_by_score,
                    std::collections::HashSet::from([SampleId(id)]),
                    "only {name} belongs to the current text"
                );
            }
            assert!(engine.is_playing());
        }

        /// A sample published while the producer queries is already resolvable,
        /// though the host cannot install and retain it until the next turn.
        #[test]
        fn an_accepted_sample_gets_its_horizon_before_the_ready_queue_is_installed() {
            let library = rustel_runtime::samples::SampleLibrary::empty_without_loading();
            let sample = SampleId(70);
            let decoded = DecodedSample::from_parts(48_000, 1, vec![0.5; 48_000]).expect("pcm");
            library.publish_ready_for_test("https://samples.invalid/late.wav", sample, decoded);
            let event = AudioEvent {
                onset_id: 1,
                generation: 1,
                ui_visuals: 0,
                target_frame: 12_000,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 1.0,
                duration_secs: 0.1,
                controls: Default::default(),
                sample: Some(rustel_audio::SampleControls {
                    sample,
                    playback_rate: 0.5,
                    begin: 0.0,
                    end: 1.0,
                    hold: rustel_audio::SampleHold::Slice,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                }),
                wavetable: None,
                synth: None,
                cut: None,
            };
            let mut use_until = HashMap::new();
            note_sample_use(
                &event,
                &HashMap::new(),
                Some(&library),
                &mut use_until,
                48_000,
                0,
            );
            assert!(
                use_until[&sample] > event.target_frame + 96_000,
                "the slow sample's full tail stays protected before installation"
            );
            let render_frontier = 48_000;
            note_sample_use(
                &event,
                &HashMap::new(),
                Some(&library),
                &mut use_until,
                48_000,
                render_frontier,
            );
            assert!(
                use_until[&sample] > render_frontier + 96_000,
                "a late onset still gets the full sample lifetime after adoption"
            );
            assert_eq!(
                library.take_ready().len(),
                1,
                "installation still owns the PCM"
            );
        }

        #[test]
        fn a_refused_live_edit_restores_the_audible_scores_dynamic_sample_pins() {
            let mut engine = engine_with_five_sounds(0);
            let kick = SampleId(70);
            let playing = "const name = 'ki' + 'ck'; s(name)";
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from("s('kick')")],
                ..LiveMaterial::default()
            });
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate(playing, false).expect("the playing score");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            engine.set_live_material(&LiveMaterial::default());

            engine
                .evaluate("s(\"nosuchsound*4\")", false)
                .expect("the candidate evaluates before its first query refuses");
            assert!(engine.played_reset_pending);
            assert!(engine.played_by_text.is_empty());
            assert!(engine.played_by_score.contains(&kick));
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));

            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.session.active_source() != Some(playing) {
                assert!(
                    Instant::now() < deadline,
                    "the refused score never rolled back"
                );
                engine.tick(|_| Ok(())).expect("refuse the candidate");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(engine.played_text.as_deref(), Some(playing));
            assert_eq!(engine.current_score, playing);
            assert!(engine.played_by_text.contains(&kick));
            tick_until_the_score_is_confirmed(&mut engine);
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));
            assert!(engine.is_playing());

            assert!(engine.stop(Duration::from_millis(500)).is_some());
            assert!(engine.played_by_score.contains(&kick));
        }

        #[test]
        fn a_panicking_edit_restores_the_audible_scores_dynamic_sample_pins() {
            let mut engine = engine_with_five_sounds(0);
            let kick = SampleId(70);
            let playing = "const name = 'ki' + 'ck'; s(name)";
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate(playing, false).expect("the playing score");
            tick_until_the_text_sounds(&mut engine, kick);
            tick_until_the_score_is_confirmed(&mut engine);
            hold_clock(&engine, true);
            engine
                .evaluate("const name = 'p' + 'ad'; s(name).fast(16)", false)
                .expect("the pending edit");
            assert!(engine.played_reset_pending);
            assert!(engine.played_by_score.contains(&kick));
            assert!(!engine.played_by_text.contains(&kick));
            engine
                .session
                .inject_panic_for_test(rustel_runtime::SessionPanicPoint::Query);

            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.session.active_source() != Some(playing) {
                assert!(
                    Instant::now() < deadline,
                    "the panic never restored the score"
                );
                engine.tick(|_| Ok(())).expect("contain the query panic");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(engine.current_score, playing);
            assert_eq!(engine.played_text.as_deref(), Some(playing));
            assert!(engine.played_by_text.contains(&kick));
            engine.force_sample_idle_for_test(Duration::from_secs(5));
            assert!(engine.retained_samples.contains_key(&kick));

            hold_clock(&engine, false);
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(engine.stop(Duration::from_millis(500)).is_some());
            assert!(engine.played_by_score.contains(&kick));
        }

        #[test]
        fn closing_a_tab_before_its_edit_is_confirmed_does_not_restore_its_stop_pins() {
            let mut engine = engine_with_five_sounds(0);
            engine.set_live_material(&LiveMaterial {
                recent: vec![Arc::from("s('kick')"), Arc::from("s('pad')")],
                ..LiveMaterial::default()
            });
            requeue_retained_samples(
                engine.session.sample_library().unwrap(),
                &engine.retained_samples,
            );
            play_silently(&mut engine);
            engine.evaluate("s(\"kick\")", false).expect("first score");
            tick_until_the_text_sounds(&mut engine, SampleId(70));
            tick_until_the_score_is_confirmed(&mut engine);
            hold_clock(&engine, true);
            engine
                .evaluate("const name = 'p' + 'ad'; s(name).fast(16)", false)
                .expect("replacement");
            tick_until_the_text_sounds(&mut engine, SampleId(71));
            assert!(engine.played_reset_pending);

            engine.set_live_material(&LiveMaterial {
                tabs_closed: 1,
                ..LiveMaterial::default()
            });
            assert!(engine.played_text.is_none());
            assert!(engine.played_by_text.contains(&SampleId(71)));
            hold_clock(&engine, false);
            tick_until_the_score_is_confirmed(&mut engine);
            assert!(
                engine.played_text.is_none(),
                "confirmation reopened the closed text"
            );
            assert_eq!(
                engine.played_by_score,
                std::collections::HashSet::from([SampleId(71)])
            );

            assert!(engine.stop(Duration::from_millis(500)).is_some());
            assert!(engine.played_by_score.is_empty());
            assert!(engine.played_by_text.is_empty());
        }
    }

    /// The pads are a process-global static (`rustel_core::gamepad`), shared
    /// with every other test in this binary; the tests below own pad slot 0
    /// (nothing else in this crate touches it) and still serialize among
    /// themselves with this, the way the `controllers` tests in `app.rs` do
    /// for the slots they own.
    #[cfg(feature = "gamepad")]
    static GAMEPAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// The studio opens the ports a score asks to listen to. The pad
    /// listener does not count: it opens ports for pads and CC and never
    /// feeds the score's bus.
    #[test]
    fn a_score_that_asks_to_listen_gets_its_port_opened() {
        use std::sync::Arc;
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        let opener = Arc::new(rustel_runtime::midi_input::RecordingOpener {
            asked: std::sync::Mutex::new(Vec::new()),
        });
        engine.set_midi_input_opener(opener.clone());
        engine
            .session
            .evaluate("const keys = await midikeys('Bass Station II')\n$: keys().s(\"piano\")\n")
            .expect("the score the docs teach");
        // Through the stopped turn, because that is when a musician
        // evaluates: the keyboard must be live before the transport is.
        assert!(!engine.is_playing());
        engine.idle_turn(|_| Ok(()));
        // The manager opens on its own thread; a bounded wait is the whole
        // of the asynchrony here.
        let asked = std::time::Instant::now();
        loop {
            let seen = opener
                .asked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if seen.iter().any(|port| port == "Bass Station II") {
                break;
            }
            assert!(
                asked.elapsed() < Duration::from_secs(5),
                "the studio never asked to open the port the score named: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A score that stops asking for a keyboard releases the port, and the
    /// transport does not have to run for that.
    #[test]
    fn a_score_that_stops_asking_lets_the_port_go() {
        use std::sync::Arc;
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.set_midi_input_opener(Arc::new(rustel_runtime::midi_input::RecordingOpener {
            asked: std::sync::Mutex::new(Vec::new()),
        }));
        engine
            .session
            .evaluate("const keys = await midikeys('Bass Station II')\n$: keys().s(\"piano\")\n")
            .expect("score");
        engine.open_score_midi_inputs();
        assert!(
            engine
                .session
                .midi_input_bus()
                .find("Bass Station II")
                .is_some(),
            "the score asked for it"
        );
        engine
            .session
            .evaluate("$: s(\"bd*4\")\n")
            .expect("a score with no keyboard in it");
        // Use the turn the worker runs while stopped, not the sync called
        // directly: the release must not depend on playback.
        assert!(!engine.is_playing(), "this is the stopped path");
        engine.idle_turn(|_| Ok(()));
        let generation = engine.session.generation();
        let wanted = engine
            .session
            .midi_input_bus()
            .snapshot_for(generation, generation);
        assert!(
            wanted.is_empty(),
            "a score that names no port wants none, and nothing had to be \
             playing for it to say so"
        );
    }

    /// The limiter reaches the device on an ordinary turn, with the gain,
    /// and not only on the rebuild after an output change.
    #[test]
    fn the_limiter_reaches_the_device_on_an_ordinary_turn() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let settings = rustel_audio::LimiterSettings {
            threshold_db: -9.8,
            character: rustel_audio::LimiterCharacter::Transparent,
        };
        engine.master.set_limiter(Some(settings));
        engine.tick(|_| Ok(())).expect("a turn");
        let live = engine.live.as_ref().expect("the silent device is live");
        assert_eq!(
            live.device.limiter(),
            Some(settings),
            "the ceiling never left the bus"
        );

        // And taking it off travels the same way.
        engine.master.set_limiter(None);
        engine.tick(|_| Ok(())).expect("another turn");
        let live = engine.live.as_ref().expect("still live");
        assert_eq!(live.device.limiter(), None);
    }

    #[test]
    fn max_polyphony_default_and_score_override_reach_the_live_device() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.master.set_max_polyphony(192);
        engine.tick(|_| Ok(())).expect("host default update");
        assert_eq!(engine.session.max_polyphony(), 192);
        assert_eq!(engine.master.effective_max_polyphony(), 192);
        assert_eq!(engine.master.max_polyphony_override(), None);
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 192);
        engine
            .session
            .evaluate("setMaxPolyphony(4); s('sine')")
            .expect("score override");
        engine.tick(|_| Ok(())).expect("override update");
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 4);
        assert_eq!(engine.master.effective_max_polyphony(), 4);
        assert_eq!(engine.master.max_polyphony_override(), Some(4));
        engine.master.set_max_polyphony(256);
        assert!(
            engine
                .session
                .evaluate("setMaxPolyphony(32); throw new Error('no')")
                .is_err()
        );
        engine.tick(|_| Ok(())).expect("rejected override");
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 4);
        assert_eq!(engine.session.config().max_polyphony, 256);
        let replacement =
            LiveScalarDevice::start_silent(48_000, engine.generation()).expect("replacement");
        engine.live.as_mut().unwrap().device = replacement;
        engine.rearm_recycled_output().expect("rearm output");
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 4);
    }

    #[test]
    fn max_polyphony_host_default_stays_current_while_stopped() {
        let mut engine = StudioEngine::new(StudioConfig {
            session: SessionConfig::default().with_max_polyphony(192),
            default_samples: false,
            watch_pads: false,
            ..Default::default()
        })
        .expect("engine");
        assert_eq!(engine.master.max_polyphony(), 192);
        assert_eq!(engine.master.effective_max_polyphony(), 192);
        engine.master.set_max_polyphony(256);
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.session.max_polyphony(), 256);
        assert_eq!(engine.master.effective_max_polyphony(), 256);
        assert_eq!(engine.master.max_polyphony_override(), None);
        engine
            .session
            .evaluate("setMaxPolyphony(256); s('sine')")
            .expect("score override matching default");
        engine.idle_turn(|_| Ok(()));
        assert_eq!(
            engine.master.max_polyphony_override(),
            Some(256),
            "explicitness is retained even when equal to the default"
        );
        engine.master.set_max_polyphony(64);
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.master.effective_max_polyphony(), 256);
        assert_eq!(engine.master.max_polyphony_override(), Some(256));
    }

    /// The MIDI schedule reads the limiter's runway off the master bus: the
    /// studio opens with the limiter off and so with nothing to wait out, a
    /// character change lands on the next read, and off holds nothing back.
    #[test]
    fn the_master_bus_reports_the_limiters_runway() {
        let bus = super::StudioMasterBus::default();
        // The limiter is a setting now, and the studio opens with it off:
        // there is no runway to report until something asks for one.
        assert_eq!(bus.limiter_latency_frames(48_000), 0);
        bus.set_limiter(Some(rustel_audio::LimiterSettings {
            threshold_db: -1.0,
            character: rustel_audio::LimiterCharacter::Transparent,
        }));
        // Transparent holds five milliseconds at 48 kHz.
        assert_eq!(bus.limiter_latency_frames(48_000), 240);
        bus.set_limiter(Some(rustel_audio::LimiterSettings {
            threshold_db: -6.0,
            character: rustel_audio::LimiterCharacter::Hard,
        }));
        assert_eq!(bus.limiter_latency_frames(48_000), 48);
        // Off is not "whatever the last character was": no offset at all.
        bus.set_limiter(None);
        assert_eq!(bus.limiter_latency_frames(48_000), 0);
    }

    /// A `.midi()` note is timed `latency_frames()` later than the device
    /// clock it was placed on, so the hardware fires with the audio the
    /// limiter held back - never ahead of it. Off adds nothing, and a
    /// character change only moves notes scheduled after it.
    #[test]
    fn the_midi_schedule_waits_out_the_limiters_runway() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let anchor = engine.midi_clock.instant_for(0.0, 0.0);
        // The mapping slews towards the wall clock by a thousandth of the
        // elapsed time per call; between two back-to-back calls that is
        // microseconds, so the runway dominates the comparison.
        let runway = |frames: u64| Duration::from_secs_f64(frames as f64 / 48_000.0);
        // The studio opens with the limiter off, so there is nothing to
        // wait out yet.
        assert!(
            engine
                .midi_instant_for(0.0, 0.0, 48_000)
                .duration_since(anchor)
                < Duration::from_millis(1),
            "off holds nothing back"
        );
        engine
            .master
            .set_limiter(Some(rustel_audio::LimiterSettings {
                threshold_db: -1.0,
                character: rustel_audio::LimiterCharacter::Transparent,
            }));
        let due = engine.midi_instant_for(0.0, 0.0, 48_000);
        let wait = due.duration_since(anchor);
        assert!(
            wait >= runway(240) && wait < runway(240) + Duration::from_millis(1),
            "transparent holds five milliseconds, not {wait:?}"
        );
        engine
            .master
            .set_limiter(Some(rustel_audio::LimiterSettings {
                threshold_db: -6.0,
                character: rustel_audio::LimiterCharacter::Hard,
            }));
        let due = engine.midi_instant_for(0.0, 0.0, 48_000);
        let wait = due.duration_since(anchor);
        assert!(
            wait >= runway(48) && wait < runway(48) + Duration::from_millis(1),
            "a change lands on the next scheduling turn: hard holds one \
             millisecond, not {wait:?}"
        );
        engine.master.set_limiter(None);
        let due = engine.midi_instant_for(0.0, 0.0, 48_000);
        assert!(
            due.duration_since(anchor) < Duration::from_millis(1),
            "off holds nothing back"
        );
    }

    /// And a studio that never opens a keyboard never starts the manager's
    /// thread: the cost of this belongs to the scores that use it.
    #[test]
    fn a_score_that_listens_to_nothing_starts_no_input_manager() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.session.evaluate("$: s(\"bd*4\")").expect("score");
        engine.idle_turn(|_| Ok(()));
        assert!(engine.midi_inputs.is_none());
    }

    #[test]
    fn unused_preview_samples_are_dropped_when_over_budget_or_idle() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 2048,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let sample = || DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm");
        for index in 1..6u32 {
            engine.retain_sample_for_test(SampleId(index), sample());
        }
        assert_eq!(engine.retained_sample_count(), 5);
        engine.set_sample_memory_policy(2048, Duration::from_secs(1));
        assert_eq!(
            engine.retained_sample_count(),
            1,
            "budget 2 KiB drops the four oldest 4 KiB extras and spares the newest: a preview refuses to sound a sample that is no longer retained"
        );
        for index in 10..13u32 {
            engine.retain_sample_for_test(SampleId(index), sample());
        }
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(
            engine.retained_sample_count(),
            0,
            "idle drop clears samples the empty score does not name"
        );
    }

    /// Eviction hands the name back to the library. If it does not, the
    /// sound stays Ready with no PCM, and a score that names it plays
    /// silence for the rest of the session.
    #[test]
    fn an_evicted_sample_is_handed_back_to_the_library() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let library = engine
            .session
            .sample_library()
            .cloned()
            .expect("a session sample library");
        const URL: &str = "http://127.0.0.1:9/preview.wav";
        library.remember_ready_for_test(URL, SampleId(21));
        engine.retain_sample_for_test(
            SampleId(21),
            DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm"),
        );
        assert!(library.knows_ready_for_test(URL));

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(
            engine.retained_sample_count(),
            0,
            "the idle sweep dropped it"
        );
        assert!(
            !library.knows_ready_for_test(URL),
            "a sound whose PCM is gone must be unknown again, or nothing ever decodes it a second time"
        );
    }

    /// The idle sweep costs a source scan and two of the library's tables.
    /// Once the idle has elapsed it is owed once, not on every turn of the
    /// engine loop until something is previewed again.
    #[test]
    fn the_idle_sweep_is_owed_once_per_quiet_spell() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        engine.retain_sample_for_test(SampleId(31), pcm());
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(engine.retained_sample_count(), 0);
        assert!(engine.idle_swept);

        // A sample retained after the sweep stays: the quiet spell already
        // had its answer, and asking again is a source scan and two library
        // tables per turn of the engine loop.
        engine.retain_sample_for_test(SampleId(32), pcm());
        engine.enforce_sample_memory(Instant::now());
        assert_eq!(engine.retained_sample_count(), 1);

        engine.note_preview();
        assert!(!engine.idle_swept, "a preview restarts the quiet spell");
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(engine.retained_sample_count(), 0);
    }

    /// The worker runs the playing tick only while there is a device; a
    /// stopped studio takes idle turns. Those turns are where a stopped
    /// engine keeps its samples to the policy, or nothing ever does: what
    /// the loaders finish waits in the library's ready queue, and browsing
    /// the Examples while stopped decodes every sound a row names.
    #[test]
    fn decodes_that_land_while_stopped_are_held_to_the_preview_budget() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 8 * 1024,
            unused_sample_idle: Duration::from_secs(60),
            ..StudioConfig::default()
        })
        .expect("engine");
        assert!(!engine.is_playing());
        let library = engine
            .session
            .sample_library()
            .cloned()
            .expect("a session sample library");
        let url = |index: u32| format!("http://127.0.0.1:9/warmed-{index}.wav");
        // Six 4 KiB decodes land one after another, as rows are passed.
        for index in 0..6u32 {
            let pcm = DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm");
            library.publish_ready_for_test(&url(index), SampleId(50 + index), pcm);
            engine.idle_turn(|_| Ok(()));
            std::thread::sleep(Duration::from_millis(2));
        }
        // While sounds keep landing, what they are is worked out again at
        // most every PROTECTION_REFRESH; the ceiling holds from that pass.
        std::thread::sleep(PROTECTION_REFRESH);
        engine.idle_turn(|_| Ok(()));

        // 8 KiB of extras kept, plus the newest spared beyond them.
        assert_eq!(engine.retained_sample_count(), 3);
        assert!(
            !library.knows_ready_for_test(&url(0)),
            "the oldest is forgotten, so asking for it decodes it again"
        );
        assert!(library.knows_ready_for_test(&url(5)), "the newest stays");
        let waiting: Vec<SampleId> = library
            .peek_ready_unless(|_| false)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            waiting.len(),
            3,
            "what the budget dropped left the queue too, so no device installs it: {waiting:?}"
        );
        assert!(
            engine.free_memory_due.is_some(),
            "and what it dropped is owed back to the system"
        );
    }

    /// A stopped engine over a library whose `kick`, `pad`, `snare`, `hat`
    /// and `bell` are each decoded and retained, 4 KiB apiece, with the
    /// given preview ceiling and a one-second idle sweep.
    fn engine_with_five_sounds(preview_budget_bytes: usize) -> StudioEngine {
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
    fn protected_afresh(engine: &mut StudioEngine) -> std::collections::HashSet<SampleId> {
        engine.protection = None;
        assert!(engine.refresh_protection(Instant::now()));
        engine.protection.as_ref().expect("worked out").ids.clone()
    }

    fn retained_names(engine: &StudioEngine) -> Vec<&'static str> {
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

    /// A tab behind a pad, in a pane or last heard keeps every sound it
    /// names, commented-out lanes included, through the sweep and the
    /// ceiling, whatever was evaluated last.
    #[test]
    fn an_always_kept_tab_keeps_its_sounds_whatever_was_evaluated_last() {
        let mut engine = engine_with_five_sounds(4 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from(
                "$: s(\"kick\")\n// $: s(\"pad\")\n_$: s(\"hat\")",
            )],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
        });
        engine
            .warm_source("silence", Duration::ZERO)
            .expect("warm score");

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["kick", "pad", "hat"]);
    }

    /// Tabs neither pinned nor open are kept by their last visit, each
    /// whole, while their sounds fit: the one visited longest ago goes
    /// first, and a sound a kept tab shares with it stays with that tab.
    #[test]
    fn recent_tabs_are_kept_newest_first_within_their_size() {
        let mut engine = engine_with_five_sounds(0);
        engine.recent_tabs_bytes = Some(8 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\")")],
            recent: vec![
                // Visited last: 4 KiB.
                Arc::from("s(\"snare\")"),
                // Then: the kick is the pinned tab's, so only the hat counts.
                Arc::from("s(\"kick hat\")"),
                // Longest ago, and over the size: let go.
                Arc::from("s(\"bell\")"),
            ],
            tabs_closed: 0,
            setups_select_variants: false,
        });

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["kick", "snare", "hat"]);
    }

    /// Applying a setup warms what it names without taking the score's
    /// place: taken, the playing score's sounds were unwanted, and the
    /// next sweep unloaded them from under the performance.
    #[test]
    fn a_setup_does_not_take_the_scores_place() {
        let mut engine = engine_with_five_sounds(0);
        // The score is kept for being the score only while it plays.
        play_silently(&mut engine);
        engine
            .warm_source("$: s(\"kick pad\")", Duration::ZERO)
            .expect("warm score");
        engine
            .evaluate_prebake_guarded(
                "const setup = 1",
                &std::sync::atomic::AtomicBool::new(false),
            )
            .expect("setup");
        assert_eq!(engine.current_score, "$: s(\"kick pad\")");

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["kick", "pad"]);
    }

    /// A sound the playing score builds in JavaScript is kept once the
    /// window its warm queried resolves it, though no text scan reads it.
    #[test]
    fn a_sound_the_score_builds_in_javascript_is_kept_while_it_plays() {
        let mut engine = engine_with_five_sounds(0);
        let source = "const name = 'sn' + 'are'; s(name)";
        engine.session.evaluate(source).expect("score");
        play_silently(&mut engine);
        engine
            .warm_source(source, Duration::from_secs(1))
            .expect("warm score");
        assert_eq!(
            engine.score_window_names,
            [("snare".to_owned(), Variants::first())]
        );

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(retained_names(&engine), ["snare"]);
    }

    /// An always-kept tab keeps a hyphenated name whole through a stopped
    /// studio's sweep: `mlkr-grsl:3` is one sound, not `mlkr` and `grsl`.
    #[test]
    fn an_always_kept_tab_keeps_a_hyphenated_sound() {
        let mut engine = engine_with_five_sounds(0);
        let library = engine.session.sample_library().cloned().expect("library");
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","mlkr-grsl":["a.wav","b.wav","c.wav","d.wav"]}"#,
                None,
            )
            .expect("bank map");
        let drum = SampleId(90);
        library.remember_ready_for_test("https://samples.invalid/d.wav", drum);
        engine.retain_sample_for_test(
            drum,
            DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
        );
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("$: s(\"mlkr-grsl:3\")")],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
        });

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert!(engine.retained_samples.contains_key(&drum));
        assert!(retained_names(&engine).is_empty());
    }

    /// The live material with the kick's tab always kept, the given tabs
    /// by their last visit, and the given count of tabs let go.
    fn kick_kept_with(recent: &[&str], tabs_closed: u64) -> LiveMaterial {
        LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\")")],
            recent: recent.iter().map(|text| Arc::from(*text)).collect(),
            tabs_closed,
            setups_select_variants: false,
        }
    }

    /// A tab that leaves re-arms the idle sweep, so its sounds go at the
    /// next idle turn and do not wait for a preview.
    #[test]
    fn closing_a_tab_rearms_the_idle_sweep() {
        let mut engine = engine_with_five_sounds(0);
        engine.set_live_material(&kick_kept_with(&["s(\"snare\")"], 0));
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(retained_names(&engine), ["kick", "snare"]);
        assert!(engine.idle_swept, "this quiet spell has had its sweep");

        engine.set_live_material(&kick_kept_with(&[], 1));
        engine.enforce_sample_memory(Instant::now());

        assert_eq!(retained_names(&engine), ["kick"], "the closed tab's snare");
    }

    /// Texts are sent again after every pause in typing. A name half
    /// deleted mid-edit is still the performer's: it must not be swept on
    /// the spot and decoded again once the word is finished.
    #[test]
    fn an_edit_that_drops_a_name_does_not_rearm_the_sweep() {
        let mut engine = engine_with_five_sounds(0);
        engine.set_live_material(&kick_kept_with(&["s(\"snare\")"], 0));
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(retained_names(&engine), ["kick", "snare"]);

        engine.set_live_material(&kick_kept_with(&["s(\"sn\")"], 0));
        engine.enforce_sample_memory(Instant::now());

        assert_eq!(retained_names(&engine), ["kick", "snare"]);
    }

    /// The score last evaluated is kept while it plays. Once stopped, only
    /// an open tab keeps its sounds; a closed tab holds nothing, even the
    /// last one played.
    #[test]
    fn a_stopped_score_is_not_kept_for_itself() {
        let mut engine = engine_with_five_sounds(0);
        play_silently(&mut engine);
        engine
            .warm_source("$: s(\"kick\")", Duration::ZERO)
            .expect("warm score");
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(retained_names(&engine), ["kick"], "kept while it plays");
        assert!(engine.idle_swept, "and the sweep has run");

        assert!(engine.stop(Duration::from_millis(500)).is_some());
        engine.idle_turn(|_| Ok(()));

        assert!(
            retained_names(&engine).is_empty(),
            "stopped, it is nobody's: {:?}",
            retained_names(&engine)
        );
    }

    /// What the sound costs is split the way the policy holds it: kept
    /// whatever its size, kept within the recent tabs' allowance, and held
    /// to the preview ceiling. A sound a pinned tab names is the pinned
    /// tab's, whatever recent tab names it too.
    #[test]
    fn sample_memory_splits_kept_recent_and_previews() {
        let mut engine = engine_with_five_sounds(0);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick pad\")")],
            recent: vec![Arc::from("s(\"kick snare hat\")")],
            tabs_closed: 0,
            setups_select_variants: false,
        });

        let memory = engine.snapshot().sample_memory;

        // 4 KiB apiece: kick and pad pinned, snare and hat recent, bell a
        // preview.
        assert_eq!(memory.live_bytes, 16 * 1024, "{memory:?}");
        assert_eq!(memory.recent_bytes, 8 * 1024, "{memory:?}");
        assert_eq!(memory.preview_bytes, 4 * 1024, "{memory:?}");
    }

    /// The recent tabs' allowance follows **biggest sound**, which the
    /// settings move without sending a text: the split is worked out
    /// again at once, so the breakdown never shows one limit against a split
    /// made under another.
    #[test]
    fn a_changed_recent_allowance_is_worked_out_at_once() {
        let mut engine = engine_with_five_sounds(0);
        engine.recent_tabs_bytes = Some(8 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\")")],
            recent: vec![
                Arc::from("s(\"snare\")"),
                Arc::from("s(\"hat\")"),
                Arc::from("s(\"bell\")"),
            ],
            tabs_closed: 0,
            setups_select_variants: false,
        });
        let before = engine.snapshot().sample_memory;
        assert_eq!(before.recent_bytes, 8 * 1024, "{before:?}");

        engine.recent_tabs_bytes = Some(12 * 1024);
        let raised = engine.snapshot().sample_memory;
        assert_eq!(raised.recent_limit_bytes, 12 * 1024);
        assert_eq!(raised.recent_bytes, 12 * 1024, "the bell fits now");

        engine.recent_tabs_bytes = Some(4 * 1024);
        let lowered = engine.snapshot().sample_memory;
        assert_eq!(lowered.recent_limit_bytes, 4 * 1024);
        assert_eq!(lowered.recent_bytes, 4 * 1024, "only the snare fits");
    }

    /// Each part is shown against the limit holding it, so the limits come
    /// from the engine that applies them: a policy the settings sent and a
    /// full queue refused would otherwise read as applied. They are there
    /// before any sound is.
    #[test]
    fn snapshot_reports_the_limits_the_engine_holds() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        engine.recent_tabs_bytes = Some(12 * 1024);
        engine.set_sample_memory_policy(96 * 1024 * 1024, Duration::from_secs(45));

        let memory = engine.snapshot().sample_memory;

        assert_eq!(memory.preview_budget_bytes, 96 * 1024 * 1024);
        assert_eq!(memory.unused_idle, Duration::from_secs(45));
        assert_eq!(memory.recent_limit_bytes, 12 * 1024);
        assert_eq!(memory.live_bytes + memory.preview_bytes, 0);
    }

    /// The script engine's heap is a part of the studio no other figure
    /// covers, and it is there as soon as a session is.
    #[test]
    fn script_heap_is_reported() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");

        assert!(engine.snapshot().script_heap_bytes > 0);
    }

    /// An armed launch keeps what it names, and lets it go when it is
    /// cancelled. The keyboard plays oscillators and keeps no sound, even
    /// when a bank has the name of its instrument.
    #[test]
    fn an_armed_launch_keeps_its_sounds_and_the_keyboard_keeps_none() {
        let mut engine = engine_with_five_sounds(4 * 1024);
        engine.pending_launch = Some(PendingLaunch {
            source: "$: s(\"bell\")".to_owned(),
            mini: false,
            boundary_cycle: 4.0,
            boundary_time: 8.0,
            rewind: false,
            unit_cycles: 1.0,
            preview: false,
            followed: Vec::new(),
            waited: false,
        });
        assert!(
            protected_afresh(&mut engine).contains(&SampleId(74)),
            "bell"
        );
        engine.pending_launch = None;
        assert!(
            !protected_afresh(&mut engine).contains(&SampleId(74)),
            "a cancelled launch lets it go"
        );

        let library = engine.session.sample_library().cloned().expect("library");
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","triangle":"triangle.wav"}"#,
                None,
            )
            .expect("bank map");
        library.remember_ready_for_test("https://samples.invalid/triangle.wav", SampleId(81));
        engine.retain_sample_for_test(
            SampleId(81),
            DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
        );
        engine.set_piano_settings(super::super::PianoSound::Triangle, 100);
        assert!(
            !protected_afresh(&mut engine).contains(&SampleId(81)),
            "the keyboard's triangle is an oscillator, not the bank"
        );
    }

    /// A stopped engine over a library whose `recordings` bank holds four
    /// takes, each decoded and retained at 4 KiB under ids 90 to 93 - the
    /// way a browser that previewed every take would leave them.
    fn engine_with_four_takes() -> StudioEngine {
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
    fn kept_takes(engine: &StudioEngine) -> Vec<u32> {
        (0..4)
            .filter(|take| engine.retained_samples.contains_key(&SampleId(90 + take)))
            .collect()
    }

    /// The live material with one always-kept tab.
    fn pinned_tab(text: &str) -> LiveMaterial {
        LiveMaterial {
            pinned: vec![Arc::from(text)],
            ..LiveMaterial::default()
        }
    }

    /// A tab keeps the takes it can play and no others: one naming
    /// `recordings:0` keeps take 0, not the take 3 the browser previewed.
    #[test]
    fn a_tab_naming_one_take_keeps_that_take_and_not_a_previewed_one() {
        let mut engine = engine_with_four_takes();
        engine.set_live_material(&pinned_tab("$: s(\"recordings:0\")"));

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(kept_takes(&engine), [0]);
    }

    /// An `n` written as a literal keeps the takes it writes, and only
    /// those.
    #[test]
    fn a_literal_n_keeps_the_takes_it_writes() {
        let mut engine = engine_with_four_takes();
        engine.set_live_material(&pinned_tab("$: s(\"recordings\").n(\"<0 1>\")"));

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(kept_takes(&engine), [0, 1]);
    }

    /// An `n` the text computes can land on any take, so every take stays.
    #[test]
    fn a_computed_n_keeps_every_take() {
        let mut engine = engine_with_four_takes();
        engine.set_live_material(&pinned_tab("$: s(\"recordings\").n(irand(4))"));

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(kept_takes(&engine), [0, 1, 2, 3]);
    }

    /// A setup that picks variants can set `n` for any score, so every tab
    /// keeps every take: a setup open in a tab, and one already applied.
    #[test]
    fn a_setup_that_picks_variants_keeps_every_take() {
        let mut engine = engine_with_four_takes();
        engine.set_live_material(&LiveMaterial {
            setups_select_variants: true,
            ..pinned_tab("s(\"recordings\")")
        });
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(kept_takes(&engine), [0, 1, 2, 3], "open in a tab");

        let mut engine = engine_with_four_takes();
        engine.set_live_material(&pinned_tab("s(\"recordings\")"));
        engine
            .evaluate_prebake_guarded(
                "globalThis.shuffled = pat => pat.n(irand(4))",
                &std::sync::atomic::AtomicBool::new(false),
            )
            .expect("setup");
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(kept_takes(&engine), [0, 1, 2, 3], "applied");
    }

    /// A recently visited tab is charged for the takes it can play, not
    /// for its whole bank: one take fits an allowance four would not.
    #[test]
    fn a_recent_tab_is_charged_only_for_the_takes_it_can_play() {
        let mut engine = engine_with_four_takes();
        engine.recent_tabs_bytes = Some(4 * 1024);
        engine.set_live_material(&LiveMaterial {
            recent: vec![Arc::from("s(\"recordings:2\")")],
            ..LiveMaterial::default()
        });

        engine.force_sample_idle_for_test(Duration::from_secs(5));

        assert_eq!(kept_takes(&engine), [2]);
        assert_eq!(engine.snapshot().sample_memory.recent_bytes, 4 * 1024);
    }

    /// A silent studio playing `s("piano")` from the text `score`, its piano
    /// decoded: answers the sample.
    fn playing_piano(score: &str) -> (StudioEngine, SampleId) {
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
    fn tick_until_the_text_sounds(engine: &mut StudioEngine, sample: SampleId) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !engine.played_by_text.contains(&sample) {
            assert!(Instant::now() < deadline, "{sample:?} never sounded");
            engine.tick(|_| Ok(())).expect("schedule the score");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// What the current score text sounded stays, including names that no
    /// scan finds (a take picked by code, a name built at runtime). It
    /// stays while the score plays and after Stop, until its text is
    /// replaced or its last tab leaves. A preview of the same sample adds
    /// nothing.
    #[test]
    fn what_the_score_sounded_is_kept_after_stop_until_its_tab_leaves() {
        let score = "s(\"piano\").gain(0.1)";
        let (mut engine, sample) = playing_piano(score);
        engine.audition("piano", 0.5).expect("a preview");
        assert!(
            engine.played_by_score.is_empty(),
            "a preview is not the score"
        );

        tick_until_the_text_sounds(&mut engine, sample);

        assert!(
            engine.played_by_score.contains(&sample),
            "{:?}",
            engine.played_by_score
        );
        assert!(
            protected_afresh(&mut engine).contains(&sample),
            "no text names it, and it is kept"
        );

        assert!(engine.stop(Duration::from_millis(500)).is_some());
        assert!(
            protected_afresh(&mut engine).contains(&sample),
            "kept after Stop"
        );
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from(score)],
            tabs_closed: 1,
            ..LiveMaterial::default()
        });
        assert!(
            protected_afresh(&mut engine).contains(&sample),
            "another tab left"
        );
        engine.set_live_material(&LiveMaterial {
            tabs_closed: 2,
            ..LiveMaterial::default()
        });
        assert!(engine.played_by_score.is_empty());
        assert!(!protected_afresh(&mut engine).contains(&sample));
    }

    /// The score's tab closed while it plays keeps what it sounded until
    /// Stop, and Stop lets it go.
    #[test]
    fn a_tab_closed_while_its_score_plays_keeps_its_sounds_until_stop() {
        let (mut engine, sample) = playing_piano("s(\"piano\").gain(0.1)");
        tick_until_the_text_sounds(&mut engine, sample);
        engine.set_live_material(&LiveMaterial {
            tabs_closed: 1,
            ..LiveMaterial::default()
        });
        assert!(protected_afresh(&mut engine).contains(&sample));

        assert!(engine.stop(Duration::from_millis(500)).is_some());

        assert!(engine.played_by_score.is_empty());
        assert!(!protected_afresh(&mut engine).contains(&sample));
    }

    /// A snippet previewed under the set sounds as the score while it
    /// plays, so nothing is dropped from under it; the score put back
    /// returns to what the set itself had sounded, and the preview playing
    /// on to its cutover adds nothing more.
    #[test]
    fn a_snippet_previewed_under_the_set_is_taken_back_out_with_it() {
        let score = "s(\"piano\").gain(0.1)";
        let (mut engine, sample) = playing_piano(score);
        tick_until_the_text_sounds(&mut engine, sample);
        assert!(engine.played_by_score.contains(&sample));

        engine.mark_next_install_preview(true);
        let previewed = engine.evaluate("s(\"piano\")", false).expect("the preview");
        // What the snippet sounds besides the set's own.
        let snippet = SampleId(999);
        engine.played_by_score.insert(snippet);
        assert!(
            protected_afresh(&mut engine).contains(&snippet),
            "kept while it plays"
        );

        let put_back = engine.evaluate(score, false).expect("the score put back");
        assert_eq!(
            engine.played_by_score,
            std::collections::HashSet::from([sample])
        );
        assert!(!protected_afresh(&mut engine).contains(&snippet));
        assert!(previewed.generation < put_back.generation);
        assert_eq!(
            engine.played_from_generation, put_back.generation,
            "the preview's own generation is no longer the score's"
        );

        // An armed launch carries the mark to its line, and Stop keeps the
        // set as it stood.
        engine.mark_next_install_preview(true);
        engine
            .arm_launch("s(\"piano\")", false, 1.0, false)
            .expect("arm");
        assert!(
            engine
                .pending_launch
                .as_ref()
                .is_some_and(|pending| pending.preview)
        );
        assert!(!engine.next_install_preview);
        assert!(engine.stop(Duration::from_millis(500)).is_some());
        assert!(engine.played_before_preview.is_none());
        assert_eq!(
            engine.played_by_score,
            std::collections::HashSet::from([sample])
        );
    }

    /// Stopped while a snippet previewed under the set plays, the studio
    /// keeps what the set sounded and lets the snippet's own sounds go.
    #[test]
    fn a_stop_during_a_snippet_preview_keeps_the_sets_sounds_alone() {
        let (mut engine, sample) = playing_piano("s(\"piano\").gain(0.1)");
        tick_until_the_text_sounds(&mut engine, sample);
        engine.mark_next_install_preview(true);
        engine.evaluate("s(\"piano\")", false).expect("the preview");
        engine.played_by_score.insert(SampleId(999));

        assert!(engine.stop(Duration::from_millis(500)).is_some());

        assert_eq!(
            engine.played_by_score,
            std::collections::HashSet::from([sample])
        );
    }

    /// Protection is worked out once per change, not per turn. A sound that
    /// lands after its tab was sent is kept: nothing is dropped before the
    /// next due pass catches up.
    #[test]
    fn protection_follows_sounds_that_land_after_their_tab() {
        let mut engine = engine_with_five_sounds(4 * 1024);
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\").n(\"<0 1>\")")],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
        });
        let start = Instant::now();
        assert!(engine.refresh_protection(start));
        let protected =
            |engine: &StudioEngine| engine.protection.as_ref().expect("worked out").ids.clone();
        assert!(protected(&engine).contains(&SampleId(70)));
        let generation = engine.protection_generation;
        engine.set_live_material(&LiveMaterial {
            pinned: vec![Arc::from("s(\"kick\").n(\"<0 1>\")")],
            recent: Vec::new(),
            tabs_closed: 0,
            setups_select_variants: false,
        });
        assert_eq!(
            engine.protection_generation, generation,
            "the same material sent again changes nothing"
        );
        assert!(engine.refresh_protection(start), "and needs no new pass");

        // A take the tab plays lands afterwards, under a new id.
        let library = engine.session.sample_library().cloned().expect("library");
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","kick":["kick.wav","kick2.wav"]}"#,
                None,
            )
            .expect("bank map");
        library.remember_ready_for_test("https://samples.invalid/kick2.wav", SampleId(80));
        engine.retain_sample_for_test(
            SampleId(80),
            DecodedSample::from_parts(48_000, 1, vec![0.0; 1024]).expect("pcm"),
        );

        let soon = start + Duration::from_millis(10);
        assert!(!engine.refresh_protection(soon), "not every turn");
        engine.enforce_sample_memory(soon);
        assert_eq!(
            engine.retained_sample_count(),
            6,
            "nothing is dropped on a stale answer, over the ceiling or not"
        );

        assert!(engine.refresh_protection(start + PROTECTION_REFRESH));
        assert!(protected(&engine).contains(&SampleId(80)));
    }

    /// The idle sweep is a stopped studio's, above all: the last preview's
    /// clock runs out while nothing plays.
    #[test]
    fn the_idle_sweep_runs_on_the_stopped_path() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        engine.retain_sample_for_test(SampleId(61), pcm());
        engine.retain_sample_for_test(SampleId(62), pcm());
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.retained_sample_count(), 2, "not idle long enough");

        engine.last_preview_at = Instant::now() - Duration::from_secs(5);
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.retained_sample_count(), 0, "swept while stopped");
    }

    /// Memory the engine lets go returns to the system once the engine is
    /// idle: a moment after the release, not at once, and not again
    /// immediately for the next release. After a long idle, the rest of
    /// the released memory returns too.
    #[test]
    fn memory_let_go_is_handed_back_once_idle() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            preview_budget_bytes: 0,
            unused_sample_idle: Duration::from_secs(1),
            ..StudioConfig::default()
        })
        .expect("engine");
        let start = Instant::now();
        engine.last_free_memory = start;
        assert_eq!(engine.free_memory_due, None, "nothing let go yet");

        let pcm = DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        engine.retain_sample_for_test(SampleId(41), pcm);
        engine.force_sample_idle_for_test(Duration::from_secs(5));
        assert_eq!(engine.retained_sample_count(), 0, "the sweep retired it");
        let due = engine
            .free_memory_due
            .expect("a retired sample owes a hand-back");

        engine.free_memory_when_due(due - Duration::from_millis(1));
        assert_eq!(engine.free_memory_due, Some(due), "not before its moment");
        let spaced = start + FREE_MEMORY_SPACING;
        if due < spaced {
            engine.free_memory_when_due(due);
            assert_eq!(
                engine.free_memory_due,
                Some(due),
                "not sooner than the spacing after the last hand-back"
            );
        }
        let paid = spaced.max(due);
        engine.free_memory_when_due(paid);
        assert_eq!(engine.free_memory_due, None, "paid");
        assert_eq!(engine.last_free_memory, paid);

        // Nothing owed, but idle for the interval: what the engine did not
        // see let go is handed back too.
        let quiet = paid + FREE_MEMORY_INTERVAL;
        engine.free_memory_when_due(quiet - Duration::from_millis(1));
        assert_eq!(engine.last_free_memory, paid);
        engine.free_memory_when_due(quiet);
        assert_eq!(engine.last_free_memory, quiet);
    }

    #[test]
    fn recording_padding_keeps_tap_loss_distinct_from_wall_clock_padding() {
        assert_eq!(recording_padding(100, 10, 20, 135, 100, true), Ok(20));
        assert_eq!(recording_padding(100, 10, 20, 190, 100, true), Ok(80));
        assert_eq!(recording_padding(100, 0, 0, 350, 100, false), Ok(100));
        assert_eq!(
            recording_padding(0, 1, 1 << 40, 0, 48_000, true),
            Ok(1 << 40)
        );
        assert_eq!(
            recording_padding(u64::MAX, 1, 0, 0, 48_000, true),
            Err("recording frame count overflow")
        );
        assert_eq!(
            recording_padding(0, 1, u64::MAX, 0, 48_000, true),
            Err("recording frame count overflow")
        );
    }

    #[test]
    fn recording_submission_preserves_attempted_frames_and_whole_item_drops() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut recording = Recording {
            writer: TakeWriter::start(directory.path().join("counts.wav"), 48_000).unwrap(),
            input: None,
            sample_rate: 48_000,
            started: Instant::now(),
            frames: 0,
            dropped: 0,
            tap_dropped_seen: 0,
            scratch: vec![0.5, -0.5],
        };
        recording.submit(2, 0, true).unwrap();
        assert_eq!((recording.frames, recording.dropped), (3, 2));
        recording.writer.request_close();
        recording.scratch.extend([0.25, -0.25]);
        recording.submit(0, 0, true).unwrap();
        assert_eq!((recording.frames, recording.dropped), (4, 3));
        let status = recording.writer.finish();
        assert_eq!(status.error, None);
        assert_eq!(status.frames, 3, "the rejected frame was not retried");
        assert_eq!(status.final_signal.unwrap().sample_count, 6);
    }

    #[test]
    fn recording_submission_bounds_large_rejected_padding_without_materializing_it() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut recording = Recording {
            writer: TakeWriter::start(directory.path().join("rejected.wav"), 48_000).unwrap(),
            input: None,
            sample_rate: 48_000,
            started: Instant::now(),
            frames: 0,
            dropped: 0,
            tap_dropped_seen: 0,
            scratch: vec![0.5, -0.5],
        };
        recording.writer.request_close();
        let loss = 1 << 40;
        recording.submit(loss, 0, true).unwrap();
        assert!(recording.scratch.is_empty());
        assert_eq!(recording.scratch.capacity(), 1 << 16);
        // Preserve the old attempted/drop totals, not a unique-gap metric.
        assert_eq!(
            (recording.frames, recording.dropped),
            (loss + 1, 2 * loss + 1)
        );
        let status = recording.writer.finish();
        assert_eq!(status.frames, 0);
        assert_eq!(status.final_signal.unwrap().sample_count, 0);
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

        fn wait_for_capture_callback(&self) {
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
    fn dropping_the_engine_flushes_its_pending_recording_tail() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("drop-tail.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        held.wait_for_capture_callback();
        assert_eq!(
            held.engine
                .recording
                .as_ref()
                .unwrap()
                .writer
                .status()
                .frames,
            0
        );
        assert!(held.engine.request_recording_close());
        let Some(ClosingRecording::Tap { target, .. }) = held.engine.closing_recording.as_mut()
        else {
            panic!("closure discarded the pending tap");
        };
        *target = 0;
        // HeldRecording releases the writer gate before dropping the engine.
        // This covers destruction during pending close. With no wall padding,
        // only this callback's retained samples can make the file nonempty.
        drop(held);
        let bytes = std::fs::read(path).unwrap();
        let data_bytes = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert!(
            data_bytes > 0,
            "engine destruction lost the pending tap tail"
        );
        assert_eq!(data_bytes % 6, 0);
        assert_eq!(bytes.len(), 44 + data_bytes);
    }

    #[test]
    fn pending_tap_close_retains_ownership_until_the_final_handoff() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("pending-tap.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        held.wait_for_capture_callback();
        assert!(held.engine.request_recording_close());
        let Some(ClosingRecording::Tap { target, .. }) = held.engine.closing_recording.as_mut()
        else {
            panic!("closure discarded the pending tap");
        };
        *target = 0;
        for _ in 0..3 {
            // The audio tests hold an actual write guard. Here the predicate
            // isolates Studio's ownership while that retirement is pending.
            held.engine.advance_recording_close_with(|_| false);
            let Some(ClosingRecording::Tap {
                recording,
                target,
                has_output,
                flush,
            }) = held.engine.closing_recording.as_ref()
            else {
                panic!("pending retirement reached the writer");
            };
            assert_eq!(*target, 0);
            assert!(*has_output && *flush);
            assert_eq!(
                (
                    recording.frames,
                    recording.dropped,
                    recording.tap_dropped_seen
                ),
                (0, 0, 0)
            );
            assert_eq!(recording.writer.status().frames, 0);
            assert!(
                held.engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .owns_record_capture(&recording.input.as_ref().unwrap().capture)
            );
        }
        let refused = directory.path().join("refused.wav");
        assert!(held.engine.start_recording(refused.clone()).is_err());
        assert!(!refused.exists());

        held.engine.session.transport().stop();
        drop(held.engine.live.take());
        held.engine.advance_recording_close();
        let Some(ClosingRecording::Writer(writer)) = held.engine.closing_recording.as_ref() else {
            panic!("joined output did not retire its tap");
        };
        let frames = writer.status().frames;
        assert!(frames > 0, "the retained tap tail was lost");
        held.wait_until_held();
        for _ in 0..3 {
            assert_eq!(held.engine.try_finish_recording(), None);
            let Some(ClosingRecording::Writer(writer)) = held.engine.closing_recording.as_ref()
            else {
                panic!("writer ownership disappeared");
            };
            assert_eq!(writer.status().frames, frames);
        }
        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.path, path);
        assert_eq!(status.error, None);
        assert_eq!(status.frames, frames);
        assert_eq!(status.final_signal.unwrap().sample_count, frames * 2);
        assert_eq!(held.engine.try_finish_recording(), None);
    }

    #[test]
    fn recording_rebinds_only_after_the_previous_output_tap_is_drained() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("rebind.wav"), 48_000, false, false);
        held.wait_for_capture_callback();
        let mut live = held.engine.live.take().unwrap();
        drop(live.device);
        live.device = LiveScalarDevice::start_silent(48_000, held.engine.generation()).unwrap();
        held.engine
            .session
            .bind_audio_confirmations(live.device.confirmations())
            .expect("bind replacement output confirmations");
        held.engine.live = Some(live);
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .owns_record_capture(
                    &held
                        .engine
                        .recording
                        .as_ref()
                        .unwrap()
                        .input
                        .as_ref()
                        .unwrap()
                        .capture
                )
        );
        // A future test-owned start keeps the wall-padding target at zero.
        // Check it after the pump too, so elapsed time cannot satisfy the
        // retained-PCM assertion if the old tap was not drained.
        held.engine.recording.as_mut().unwrap().started = Instant::now() + Duration::from_secs(60);
        held.engine.pump_recording();
        let recording = held.engine.recording.as_ref().unwrap();
        assert_eq!(recording.target_frames(), 0);
        assert!(recording.frames > 0, "old output tail was not submitted");
        assert_eq!(recording.dropped, 0);
        assert_eq!(recording.tap_dropped_seen, 0);
        assert!(
            held.engine
                .live
                .as_ref()
                .unwrap()
                .device
                .owns_record_capture(&recording.input.as_ref().unwrap().capture)
        );
        assert!(held.engine.request_recording_close());
        held.wait_until_held();
        held.release_and_wait();
        assert_eq!(held.engine.try_finish_recording().unwrap().error, None);
    }

    #[test]
    fn changed_output_rate_flushes_only_the_previous_capture() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("changed-rate-tail.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        held.wait_for_capture_callback();
        let mut live = held.engine.live.take().unwrap();
        drop(live.device);
        live.device = LiveScalarDevice::start_silent(44_100, held.engine.generation()).unwrap();
        held.engine
            .session
            .bind_audio_confirmations(live.device.confirmations())
            .expect("bind replacement output confirmations");
        held.engine.live = Some(live);
        assert!(held.engine.request_recording_close());
        let Some(ClosingRecording::Tap {
            recording, target, ..
        }) = held.engine.closing_recording.as_mut()
        else {
            panic!("rate change discarded the pending tap");
        };
        assert_eq!(recording.sample_rate, 48_000);
        assert_eq!(recording.input.as_ref().unwrap().sample_rate, 48_000);
        assert_eq!(
            recording.input.as_ref().unwrap().capture.dropped_frames(),
            Ok(0)
        );
        assert_eq!(recording.writer.status().frames, 0);
        *target = 0;
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .owns_record_capture(&recording.input.as_ref().unwrap().capture)
        );
        assert!(held.engine.request_recording_close());
        held.engine.pump_recording();
        assert!(!held.engine.is_recording());
        let device = &held.engine.live.as_ref().unwrap().device;
        assert_eq!(device.sample_rate(), 44_100);
        assert!(!device.recording_enabled());
        assert_eq!(
            held.engine
                .pending_diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == "record")
                .count(),
            1
        );
        let Some(ClosingRecording::Writer(writer)) = held.engine.closing_recording.as_ref() else {
            panic!("old-rate tap did not reach its writer");
        };
        let frames = writer.status().frames;
        assert!(frames > 0, "old-rate tail was not submitted");
        held.wait_until_held();
        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.path, path);
        assert_eq!(status.sample_rate, 48_000);
        assert_eq!(status.error, None);
        assert_eq!(status.frames, frames);
        assert_eq!(status.final_signal.unwrap().sample_count, frames * 2);
        assert_eq!(held.engine.try_finish_recording(), None);
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .recording_enabled()
        );
    }

    #[test]
    fn closing_take_does_not_block_ticks_or_graceful_stop() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("closing.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        assert!(held.engine.request_recording_close());
        held.wait_until_held();
        assert!(!held.engine.is_recording());
        for _ in 0..2 {
            assert_eq!(held.engine.try_finish_recording(), None);
            assert!(!held.closing_finished());
            assert!(matches!(
                held.engine.tick_at(Duration::ZERO, accepted).unwrap(),
                StudioTick::Running { step: Some(_), .. }
            ));
        }
        let refused = directory.path().join("refused.wav");
        let generation = held.engine.generation();
        let stream = held.engine.live.as_ref().unwrap().device.stream_id();
        assert!(held.engine.start_recording(refused.clone()).is_err());
        assert!(!refused.exists());
        assert_eq!(held.engine.generation(), generation);
        assert_eq!(
            held.engine.live.as_ref().unwrap().device.stream_id(),
            stream
        );

        held.engine.request_stop();
        assert!(matches!(
            held.engine.tick_at(Duration::ZERO, accepted).unwrap(),
            StudioTick::Stopping
        ));
        assert!(matches!(
            held.engine
                .tick_at(
                    GRACEFUL_STOP_SILENCE_HOLD + Duration::from_millis(1),
                    accepted
                )
                .unwrap(),
            StudioTick::Stopped(_)
        ));
        assert!(!held.engine.is_playing());
        assert_eq!(held.engine.try_finish_recording(), None);
        assert!(!held.closing_finished());

        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.path, path);
        assert_eq!(status.error, None);
        assert_eq!(status.final_signal.unwrap().nonfinite_count, 0);
        assert_eq!(held.engine.try_finish_recording(), None);
        assert_eq!(held.engine.stop_recording(), None);
    }

    #[test]
    fn graceful_stop_does_not_mistake_a_silent_passage_for_a_finished_sample() {
        let silence = MasterLevels::default();
        assert!(graceful_stop_source_finished(silence, 0, 0, 0));
        assert!(
            !graceful_stop_source_finished(silence, 1, 0, 0),
            "an active sample voice may resume after a silent passage"
        );
        assert!(
            !graceful_stop_source_finished(silence, 0, 1, 0),
            "a queued source event is not finished"
        );
        assert!(
            !graceful_stop_source_finished(silence, 0, 0, 1),
            "a delay line may be silent before its next echo"
        );
        assert!(
            !graceful_stop_source_finished(
                MasterLevels {
                    peak: GRACEFUL_STOP_SILENCE_PEAK * 2.0,
                    ..MasterLevels::default()
                },
                0,
                0,
                0
            ),
            "the source is still ringing through its release or effect tail"
        );
    }

    /// `stop_recording` is the whole close (ask, wait, join), because the
    /// engine's Drop calls only it. A take still rolling at quit is asked
    /// to close, and its writer finishes.
    #[test]
    fn stop_recording_closes_a_take_nobody_asked_to_close() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("rolling.wav"), 48_000, false, false);
        assert!(held.engine.is_recording());
        assert!(held.engine.closing_recording.is_none());
        held.gate.release();
        let status = held
            .engine
            .stop_recording()
            .expect("the take is asked to close, then joined");
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
        assert!(!held.engine.is_recording());
        assert_eq!(held.engine.stop_recording(), None);
    }

    /// A driver that never calls again does not block the close. The tap
    /// gets half a second, then the close returns `None` and keeps the
    /// closure. The close completes when the driver is back.
    #[test]
    fn stop_recording_gives_a_wedged_tap_half_a_second_then_returns() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("wedged.wav"), 48_000, false, false);
        let started = Instant::now();
        assert_eq!(
            held.engine.stop_recording_with(|_| false),
            None,
            "a tap that never retires is not waited on forever"
        );
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(450) && waited < Duration::from_secs(3),
            "the wait is bounded at about half a second: {waited:?}"
        );
        assert!(
            matches!(
                held.engine.closing_recording,
                Some(ClosingRecording::Tap { .. })
            ),
            "the closure is kept for later turns"
        );
        // The driver comes back: the same close now completes and the take
        // is on disk with its sizes.
        held.gate.release();
        let status = held
            .engine
            .stop_recording()
            .expect("once the tap retires the take is joined");
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
    }

    #[test]
    fn audio_stop_keeps_an_open_take_until_explicit_close() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held = HeldRecording::new(directory.path().join("open.wav"), 48_000, false, false);
        assert!(held.engine.stop(Duration::from_millis(200)).is_some());
        assert!(!held.engine.is_playing());
        assert!(held.engine.is_recording());
        assert!(held.engine.closing_recording.is_none());
        assert!(held.engine.request_recording_close());
        held.wait_until_held();
        held.gate.release();
        let status = held.engine.stop_recording().unwrap();
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
        assert_eq!(held.engine.stop_recording(), None);
    }

    #[test]
    fn rate_change_closes_once_without_waiting_for_the_writer() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("old-rate.wav"), 44_100, false, false);
        let mut notices = 0;
        for _ in 0..2 {
            held.engine.tick_at(Duration::ZERO, |update| {
                    if matches!(&update, StudioUpdate::Diagnostic(diagnostic) if diagnostic.kind == "record") {
                        notices += 1;
                    }
                    Ok(())
                }).unwrap();
        }
        held.wait_until_held();
        assert_eq!(notices, 1);
        assert!(!held.engine.is_recording());
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .recording_enabled()
        );
        assert_eq!(held.engine.try_finish_recording(), None);
        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.sample_rate, 44_100);
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
    }

    #[test]
    fn opening_output_updates_sample_decode_rate_before_evaluation() {
        let engine = StudioEngine::new(StudioConfig {
            session: rustel_runtime::SessionConfig::default().with_sample_rate(44_100),
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("studio");
        let library = engine.sample_library().expect("sample library");
        assert_eq!(library.render_rate_for_test(), 44_100);

        // Every startup route opens the output before evaluating its source.
        let device = engine.open_output().expect("silent output");
        assert_eq!(device.sample_rate(), 48_000);
        assert_eq!(library.render_rate_for_test(), device.sample_rate());
        assert_eq!(engine.session.config().sample_rate, 44_100);
    }

    #[test]
    fn configured_output_and_cached_metadata_retain_dispatch() {
        for dispatch in [
            rustel_audio::DspDispatch::automatic(),
            rustel_audio::DspDispatch::portable(),
        ] {
            let mut options = super::super::StudioOptions::new("unused.strudel");
            options.session.dsp_dispatch = dispatch;
            let engine = StudioEngine::new(StudioConfig {
                session: options.clone().session,
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .expect("configured engine");
            let expected = rustel_runtime::capability_registry_for_dispatch(dispatch);
            assert_eq!(
                rustel_runtime::capability_registry_for_dispatch(
                    engine.session.config().dsp_dispatch
                ),
                expected
            );
            // Fresh starts use the same opener as ordinary scores, replay
            // blocks and auditions. No score or physical device is needed.
            for _ in 0..2 {
                let mut device = engine.open_output().expect("configured silent output");
                let registry = Arc::new(rustel_runtime::capability_registry_for_dispatch(
                    device.dispatch(),
                ));
                assert_eq!(registry.as_ref(), &expected);
                let before = StudioDeviceInfo::from_device(&device, Arc::clone(&registry));
                device.recycle_output().expect("recycle selected output");
                let after = StudioDeviceInfo::from_device(&device, Arc::clone(&registry));
                assert_eq!(
                    rustel_runtime::capability_registry_for_dispatch(device.dispatch()),
                    expected
                );
                assert!(Arc::ptr_eq(&before.registry, &after.registry));
                assert_eq!(after.registry.as_ref(), &expected);
                assert_eq!(
                    after.audio.output().host(),
                    &rustel_runtime::AudioHost::Silent
                );
            }
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

    /// Two chains that fail differently in one frame each reach the
    /// diagnostics once, through the queue a full channel holds.
    #[cfg(feature = "hydra")]
    #[test]
    fn chains_that_fail_in_one_frame_each_reach_the_diagnostics() {
        if rustel_hydra::native::NativeRenderer::new(8, 8).is_err() {
            eprintln!("skipped: no GPU and no software rasteriser");
            return;
        }
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine
            .session
            .evaluate("await initHydra()\nosc().foo().out(o0)\nosc().bar().out(o1)\n")
            .expect("the score records both chains");
        let mut hydra = rustel_runtime::hydra::HydraBridge::new();
        let frames = hydra.frames();
        let mut notices: Vec<StudioDiagnostic> =
            engine.drive_hydra(&mut hydra, None).into_iter().collect();
        // Both chains fail before their frame is read back, so the next turn
        // drains both failures at once.
        let until = Instant::now() + Duration::from_secs(20);
        while frames.take().is_none() {
            assert!(Instant::now() < until, "the sketch never drew a frame");
            std::thread::sleep(Duration::from_millis(10));
        }
        notices.extend(engine.drive_hydra(&mut hydra, None));

        emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |update| {
            Err((UiEventSendStatus::DroppedFull, update))
        });
        let mut said = notices.clone();
        emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |update| {
            if let StudioUpdate::Diagnostic(diagnostic) = update {
                said.push(diagnostic);
            }
            Ok(())
        });
        let failures: Vec<&str> = said
            .iter()
            .filter(|diagnostic| {
                diagnostic.kind == "hydra" && diagnostic.level == StudioDiagnosticLevel::Warning
            })
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert!(
            failures.len() == 2
                && failures.iter().any(|message| message.contains("foo"))
                && failures.iter().any(|message| message.contains("bar")),
            "each chain's failure must be said once: {said:?}"
        );
        assert!(
            notices
                .iter()
                .all(|notice| notice.level != StudioDiagnosticLevel::Warning),
            "a failure must not ride the notice a full channel skips: {notices:?}"
        );
    }

    /// The countdown warms the INCOMING score's sounds, including the
    /// names a `.bank()` builds, and does not spend its budget querying the
    /// score going out. The text scan alone missed `tr909_bd`, and the line
    /// fired into the loading hold the warm was there to avoid.
    #[test]
    fn arming_a_launch_warms_the_incoming_banked_sounds() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let library = Arc::new(rustel_runtime::samples::SampleLibrary::empty_without_loading());
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","tr909_bd":"bd.wav","hh":"hh.wav"}"#,
                None,
            )
            .expect("bank map");
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        engine.current_score = "s(\"hh\")".to_owned();
        let before = library.pending_loads();
        engine
            .arm_launch(r#"s("bd").bank("tr909")"#, false, 1.0, true)
            .expect("arm")
            .expect("armed");
        assert!(
            library.pending_loads() > before,
            "the banked name is on its way before the line"
        );
        assert_eq!(
            engine.current_score, "s(\"hh\")",
            "warming is not installing"
        );
        engine.supersede_launch();
    }

    #[test]
    fn the_next_line_leaves_head_room() {
        assert_eq!(next_boundary(2.3, 1.0, 0.1), 3.0);
        assert_eq!(
            next_boundary(2.95, 1.0, 0.1),
            4.0,
            "too close: the one after"
        );
        assert_eq!(next_boundary(2.3, 0.25, 0.05), 2.5);
        assert_eq!(next_boundary(5.0, 4.0, 0.1), 8.0);
        assert_eq!(next_boundary(7.9, 4.0, 0.5), 12.0);
        assert_eq!(next_boundary(1.0, 0.0, 0.0), 2.0, "a bad unit is a cycle");
    }

    use super::*;

    fn accepted(update: StudioUpdate) -> StudioUpdateSendResult {
        let _ = update;
        Ok(())
    }

    /// Every note of a run lands exactly one step after the one before it,
    /// whichever tick hands it over.
    #[test]
    fn every_note_of_a_run_lands_one_step_after_the_last() {
        let step = 0.15;
        let notes = vec![60.0, 62.0, 64.0, 65.0, 67.0];
        let count = notes.len();
        let clock = 8.0;
        let mut run = RunningAudition {
            request: AuditionRequest {
                sound: "triangle".into(),
                notes,
                gain: 1.0,
                step_secs: step,
                slot: 0,
                at: None,
            },
            next: 0,
            due: clock + AUDITION_LEAD_SECS,
        };
        // The tick loop, walking the device clock forward two milliseconds
        // at a time and handing over whatever has come due.
        let mut onsets = Vec::new();
        let mut now = clock;
        while onsets.len() < count && now < clock + 5.0 {
            while let Some(at) = run.next_onset(now) {
                onsets.push(at);
                run.sounded();
            }
            now += 0.002;
        }
        assert_eq!(onsets.len(), count, "every note was handed over");
        for (index, onset) in onsets.iter().enumerate() {
            let wanted = clock + AUDITION_LEAD_SECS + index as f64 * step;
            assert!(
                (onset - wanted).abs() < 1e-9,
                "note {index} landed at {onset}, not {wanted}: {onsets:?}"
            );
        }

        // A run that fell behind - its sound was still loading, or a tick
        // ran long - starts again from here rather than firing every note
        // it missed at once.
        let mut run = RunningAudition {
            request: AuditionRequest {
                sound: "triangle".into(),
                notes: vec![60.0, 62.0, 64.0],
                gain: 1.0,
                step_secs: step,
                slot: 0,
                at: None,
            },
            next: 0,
            due: clock + AUDITION_LEAD_SECS,
        };
        let late = clock + 2.0;
        let first = run.next_onset(late).expect("its moment is long past");
        assert!(
            first >= late + AUDITION_LEAD_SECS,
            "an overdue note is still placed where the device can take it: {first}"
        );
        run.sounded();
        assert_eq!(
            run.next_onset(late),
            None,
            "and the note after it waits its step out rather than piling on"
        );
        assert!((run.due - (first + step)).abs() < 1e-9);
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

    /// A delivered layout answers two questions apart: that the display has
    /// something to draw, and which generation it draws.
    #[test]
    fn a_delivered_layout_knows_which_generation_it_describes() {
        let source = "$: s(\"bd\")._spectrum()";
        let mut delivery = LayoutDelivery::default();
        assert!(!delivery.delivered());

        // Generation 7 has been evaluated; the device is still on 6.
        assert!(delivery.observe(source, 7).expect("layout"));
        assert!(delivery.try_deliver(&mut accepted));

        assert!(delivery.delivered());
        assert!(!delivery.ready_for(6));
        assert!(delivery.ready_for(7));
    }

    /// A widget is placed from the score that was evaluated, not from the one
    /// still coming out of the speakers.
    ///
    /// The two differ while a replacement waits to be prefilled. Gating on
    /// the audible generation meant a `_spectrum()` added to a playing score
    /// did not appear until the sound swapped, which reads as the studio
    /// being slow rather than as it being careful. Upstream places the widget
    /// during evaluation, before the pattern is even handed to its scheduler.
    ///
    /// What still waits for the cutover is the widget's tap: a voice carries
    /// its slot bit as numbered by the generation that evaluated it, and the
    /// tap reset takes the device's current generation as its capture floor,
    /// so arming early would draw the old score's voices in the new widget.
    #[test]
    fn after_step_delivers_the_evaluated_layout_before_the_device_cuts_over() {
        let mut session = Session::new().expect("session");
        // The device stays on the generation that was current BEFORE the
        // evaluation, as it does while the replacement's first window is
        // still being prefilled.
        let device =
            LiveScalarDevice::start_silent(48_000, session.generation()).expect("silent output");
        session.evaluate("$: s(\"bd\")._spectrum()").expect("score");
        assert_ne!(device.generation(), session.generation());

        let mut ui = StudioUiState::new(Duration::from_millis(50));
        let mut diagnostics = PendingDiagnostics::default();
        let mut updates = Vec::new();
        ui.after_step(
            &mut session,
            &device,
            Duration::ZERO,
            true,
            false,
            Vec::new(),
            &mut diagnostics,
            &mut |update| {
                updates.push(update);
                Ok(())
            },
        );

        let layout = updates
            .iter()
            .find_map(|update| match update {
                StudioUpdate::Layout(layout) => Some(layout),
                _ => None,
            })
            .expect("a layout was emitted before the device cut over");
        assert_eq!(layout.ui_layout.generation, session.generation());
        assert!(ui.layout.delivered());
        assert!(!ui.layout.ready_for(device.generation()));
        assert_eq!(
            ui.visual_audio_mask, AUDITION_VISUALS,
            "the widget's tap waits for the device to take its generation"
        );

        // The cutover: the device takes the evaluated generation, and only
        // now is the widget's tap armed.
        device.set_generation(session.generation(), 0, TakeoverCut::None);
        ui.after_step(
            &mut session,
            &device,
            Duration::from_millis(100),
            true,
            false,
            Vec::new(),
            &mut diagnostics,
            &mut accepted,
        );
        assert!(ui.layout.ready_for(device.generation()));
        assert_eq!(ui.visual_audio_mask, AUDITION_VISUALS | 1);
        device.stop();
    }

    /// The ids the layout hands the studio are the ids the running score's
    /// cells answer to, which is what lets a dragged chip reach the pattern
    /// without an evaluate.
    #[test]
    fn layout_slider_ids_address_the_running_scores_cells() {
        let source = "$: s(\"bd\").gain(slider(.25, 0, 1, .05))";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("score");
        let layout = visual_layout(source, session.generation()).expect("layout");
        let slider = &layout.ui_layout.sliders[0];
        assert_eq!(session.slider_value(&slider.id).expect("read"), Some(0.25));
        assert!(session.set_slider_value(&slider.id, 0.6).expect("write"));
        assert_eq!(session.slider_value(&slider.id).expect("read"), Some(0.6));
        assert!(
            !session
                .set_slider_value("not-a-slider", 0.6)
                .expect("write")
        );
    }

    #[test]
    fn layout_is_retried_and_gates_its_generation() {
        let mut delivery = LayoutDelivery::default();
        let source = "note(\"c4\").scope()";
        assert!(delivery.observe(source, 7).expect("layout"));
        assert!(!delivery.ready_for(7));

        let mut refused = |update| match update {
            StudioUpdate::Layout(_) => Err((UiEventSendStatus::DroppedFull, update)),
            _ => unreachable!(),
        };
        assert!(!delivery.try_deliver(&mut refused));
        assert!(!delivery.ready_for(7));
        assert!(delivery.pending.is_some());

        assert!(delivery.try_deliver(&mut accepted));
        assert!(delivery.ready_for(7));
        assert!(delivery.pending.is_none());
    }

    #[test]
    fn invalid_new_layout_still_clears_the_previous_generation() {
        let mut delivery = LayoutDelivery::default();
        assert!(
            delivery
                .observe("note(60)._pianoroll()", 1)
                .expect("initial layout")
        );
        assert!(delivery.try_deliver(&mut accepted));

        let invalid = std::iter::repeat_n(
            "p.scope()",
            rustel_runtime::ui_events::MAX_UI_LAYOUT_VISUALS + 1,
        )
        .collect::<Vec<_>>()
        .join(";\n");
        assert!(delivery.observe(&invalid, 2).is_err());
        let reset = delivery.pending.as_ref().expect("empty reset layout");
        assert_eq!(reset.ui_layout.generation, 2);
        assert_eq!(reset.ui_layout.source_revision, source_revision(&invalid));
        assert!(reset.ui_layout.visuals.is_empty());
        assert!(reset.ui_layout.sliders.is_empty());
        assert!(reset.ui_layout.mini_locations.is_empty());
        assert!(delivery.try_deliver(&mut accepted));
        assert!(delivery.ready_for(2));
    }

    /// The input is announced once for a choice: the handoff between its
    /// monitor and the live device is the same microphone, and not news;
    /// another choice is, and so is the first coming back.
    #[test]
    fn the_input_is_announced_once_per_choice() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        assert!(engine.announce_input("iPhone"), "the first open is news");
        assert!(
            !engine.announce_input("iPhone"),
            "the same input on the live device is not"
        );
        assert!(engine.announce_input("Scarlett"), "another choice is");
        assert!(engine.announce_input("iPhone"), "and the first, back again");
        engine.input_announced = None;
        assert!(
            engine.announce_input("iPhone"),
            "an input that died and came back is"
        );
    }

    #[test]
    fn device_recycle_requeues_retained_samples_before_new_publications() {
        let library = rustel_runtime::samples::SampleLibrary::empty();
        let old = DecodedSample::from_parts(48_000, 1, vec![0.25, 0.5]).expect("old sample");
        let new = DecodedSample::from_parts(48_000, 1, vec![0.75, 1.0]).expect("new sample");
        let retained_only_id = SampleId(8);
        let replaced_id = SampleId(9);
        library.requeue_ready(replaced_id, new.clone());
        let retained = HashMap::from([(retained_only_id, old.clone()), (replaced_id, old.clone())]);

        requeue_retained_samples(&library, &retained);

        let ready = library.take_ready();
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0], (retained_only_id, old));
        assert_eq!(ready[1], (replaced_id, new));
    }

    #[test]
    fn output_recovery_snapshot_selects_latest_valid_ready_body() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let old = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
        let newer = DecodedSample::from_parts(48_000, 1, vec![0.5]).unwrap();
        let replaced = SampleId(8);
        let retained_only = SampleId(9);
        let forgotten = SampleId(10);
        for id in [replaced, retained_only, forgotten] {
            engine.retain_sample_for_test(id, old.clone());
        }
        engine.retain_sample_for_test(SampleId(SAMPLE_BANK_CAPACITY as u32), old.clone());
        library.requeue_ready(replaced, old.clone());
        library.requeue_ready(replaced, newer.clone());
        library.remember_ready_for_test("forgotten-recovery-body", forgotten);
        library.requeue_ready(forgotten, old.clone());
        library.forget_decoded(&std::collections::HashSet::from([forgotten]));

        let samples = engine.collect_recycle_samples();

        assert_eq!(
            samples,
            vec![(replaced, newer.clone()), (retained_only, old)]
        );
        assert!(library.take_ready().is_empty());
        assert_eq!(
            engine.retained_samples[&replaced].identity(),
            newer.identity()
        );
        assert!(engine.sample_last_used.contains_key(&replaced));
    }

    #[test]
    fn delayed_first_preparation_keeps_the_downbeat_and_next_kick_in_time() {
        use rustel_audio::device::ManualLiveOutput;

        const RATE: u32 = 48_000;
        let mut session = Session::with_config(
            SessionConfig::default()
                .with_horizon(1.5)
                .with_sample_rate(RATE),
        )
        .expect("session");
        session.evaluate(r#"s("bd*4")"#).expect("four kicks");
        session.transport().start();
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(RATE, generation).expect("manual output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("confirmations");
        let preroll = DEFAULT_STUDIO_START_PREROLL;
        let provisional = output.device().schedule_lead_seconds() + preroll.as_secs_f64();
        session.restart_transport_at(provisional);
        let mut pending = Some(generation);
        let mut producer =
            LiveFileProducer::unwatched(DEFAULT_STUDIO_POLL_INTERVAL).expect("producer");

        // Model input opening / first Hydra work consuming the original lead.
        // Advance the actual output clock deterministically, without sleeping
        // or depending on a machine's input-opening latency.
        output.render(&mut vec![0.0; RATE as usize * 2]);
        let now = output.device().clock_seconds();
        assert!(now > provisional, "the old downbeat is already past");
        anchor_initial_start(&mut session, &mut pending, output.device(), preroll);
        assert_eq!(pending, None);
        let mut queued = Vec::new();
        producer
            .step_unwatched_with_clock(
                &mut session,
                || output.device().clock_seconds(),
                RATE,
                |event| {
                    assert!(output.device().push(event));
                    queued.push(event);
                    true
                },
            )
            .expect("initial prefill");
        assert!(queued.len() >= 2, "both opening kicks are scheduled");
        let first = queued[0].target_frame;
        let second = queued[1].target_frame;
        assert!(
            first > output.device().clock_frames(),
            "downbeat is not late"
        );
        assert_eq!(second - first, u64::from(RATE / 2));
        let first_time = first as f64 / f64::from(RATE);
        assert!(session.cycle_at_time(first_time).abs() < 1e-8);

        let start = output.device().clock_frames();
        let frames = (second + 512 - start) as usize;
        let mut pcm = vec![0.0; frames * 2];
        output.render(&mut pcm);
        let at = ((first - start) * 2) as usize;
        let next = ((second - start) * 2) as usize;
        let attack = &pcm[at..at + 1024];
        assert!(attack.iter().any(|sample| sample.abs() > 1e-4));
        assert_eq!(
            attack,
            &pcm[next..next + 1024],
            "the full first attack survives"
        );
        assert_eq!(output.device().report().late_events, 0);

        // A subsequent tick cannot move already-submitted onsets or restart
        // the bar, even though the output has now passed both opening hits.
        anchor_initial_start(&mut session, &mut pending, output.device(), preroll);
        assert!(session.cycle_at_time(first_time).abs() < 1e-8);
    }

    #[test]
    fn initial_phase_rebase_preserves_the_start_floor_for_slider_requery() {
        use rustel_audio::device::ManualLiveOutput;

        const RATE: u32 = 48_000;
        let mut session =
            Session::with_config(SessionConfig::default().with_horizon(1.0)).expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("score");
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(RATE, generation).expect("manual output");
        session.restart_transport_at(0.2);
        let mut pending = Some(generation);
        output.render(&mut vec![0.0; RATE as usize * 2]);
        let now = output.device().clock_seconds();
        let anchor = now
            + output.device().schedule_lead_seconds()
            + DEFAULT_STUDIO_START_PREROLL.as_secs_f64();
        anchor_initial_start(
            &mut session,
            &mut pending,
            output.device(),
            DEFAULT_STUDIO_START_PREROLL,
        );

        session.set_continuity_margin(0.0);
        session.requery_active_at(now).expect("slider requery");
        let events = session.schedule_audio_at(now, RATE).expect("schedule");
        let first = events.first().expect("downbeat").target_frame;
        assert_eq!(first, (anchor * f64::from(RATE)).round() as u64);
        assert!(events.iter().all(|event| event.target_frame >= first));
    }

    #[test]
    fn initial_anchor_preserves_midi_keys_received_after_the_explicit_start() {
        use rustel_audio::device::ManualLiveOutput;

        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.25).s('tri')"#)
            .expect("keyboard score");
        session.transport().start();
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(48_000, generation).expect("manual output");
        session.restart_transport_at(0.2);
        let mut pending = Some(generation);
        let port = session.midi_input_bus().find("keyboard").expect("keyboard");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);

        output.render(&mut vec![0.0; 48_000 * 2]);
        anchor_initial_start(
            &mut session,
            &mut pending,
            output.device(),
            DEFAULT_STUDIO_START_PREROLL,
        );
        let mut hits = Vec::new();
        port.keys.select(0.0, 1.0, 0, Some((0, 1)), &mut hits);
        assert_eq!(hits.len(), 1, "finishing startup discarded its NoteOn");
        assert_eq!(
            (hits[0].note, hits[0].velocity, hits[0].channel),
            (60, 100, 1)
        );
    }

    #[test]
    fn initial_start_does_not_rebase_a_replacement_or_cancelled_transport() {
        use rustel_audio::device::ManualLiveOutput;

        for cancelled in [false, true] {
            let mut session = Session::new().expect("session");
            session.evaluate(r#"s("bd*4")"#).expect("score");
            session.transport().start();
            let generation = session.generation();
            let output = ManualLiveOutput::new(48_000, generation).expect("manual output");
            let mut pending = Some(generation);
            if cancelled {
                session.transport().stop();
            } else {
                session.reload_at(r#"s("bd*8")"#, false, 2.0).expect("edit");
            }
            let cycle = session.cycle_at_time(2.0);
            anchor_initial_start(
                &mut session,
                &mut pending,
                output.device(),
                DEFAULT_STUDIO_START_PREROLL,
            );
            assert_eq!(pending, None);
            assert_eq!(session.cycle_at_time(2.0), cycle);
            assert_eq!(session.transport().is_stopped(), cancelled);
        }
    }

    #[test]
    fn recovery_preparation_seeds_large_ready_bank_before_first_prefill_copy() {
        use rustel_audio::confirmation::WindowOutcome;
        use rustel_audio::device::ManualLiveOutput;
        use rustel_core::{Value, pure};

        const RATE: u32 = 48_000;
        const BACKLOG: usize = 1536;
        const SOUND: &str = "recovery-backlog-sample";
        for frames in [128_usize, 1024] {
            let mut engine = StudioEngine::new(StudioConfig {
                session: SessionConfig::default()
                    .with_cps(1.0)
                    .with_horizon(1.0)
                    .with_sample_rate(RATE),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine
                .session
                .enable_default_samples()
                .expect("sample library");
            let library = Arc::clone(engine.session.sample_library().unwrap());
            let decoded = DecodedSample::from_parts(RATE, 1, vec![0.25; RATE as usize])
                .expect("one-second retained body");
            let identity = decoded.identity();
            engine.retained_samples = (1..=BACKLOG as u32)
                .map(|slot| (SampleId(slot), decoded.clone()))
                .collect();
            // The required body is last in the retained bank's transfer order.
            // All ids fit the bank and share one immutable decoded body.
            let id = *engine.retained_samples.keys().last().unwrap();
            let samples = engine.collect_recycle_samples();
            assert_eq!(samples.len(), BACKLOG);
            assert_eq!(engine.sample_last_used.len(), BACKLOG);
            assert!(library.take_ready().is_empty());
            library.remember_ready_for_test(SOUND, id);
            assert_eq!(
                library.readiness(SOUND, 0.0),
                rustel_runtime::samples::SoundReadiness::Ready
            );
            assert_eq!(library.decoded_identity(id), Some(identity));
            let session = &mut engine.session;
            session
                .set_pattern(
                    pure(Value::object(vec![
                        ("s".into(), Value::Str(SOUND.into())),
                        ("gain".into(), Value::F64(0.5)),
                    ]))
                    .fast(8.into()),
                )
                .expect("native sample controls");
            session.set_schedule_lead(0.35);
            session.set_continuity_margin(0.35);
            session.transport().start();
            // This is Studio's actual collection path and the same prepared
            // backend used by a seeded output restart, not ring publication.
            let mut output =
                ManualLiveOutput::new_with_samples(RATE, session.generation(), &samples)
                    .expect("seeded recovery bank");
            let confirmations = output.device().confirmations();
            session
                .bind_audio_confirmations(confirmations.clone())
                .expect("bind real receipts");
            let mut producer =
                LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            arm_output_recovery_after_recycle(session, &mut producer, 0.0, RATE)
                .expect("recovery requery");
            assert_eq!(output.device().report().asset_queues.sample_installs, 0);

            let mut published = None;
            let mut queued = Vec::new();
            let device = output.device();
            producer
                .step_unwatched_with_clock_and_cutover(
                    session,
                    || device.clock_seconds(),
                    RATE,
                    |generation, takeover, cut| {
                        published = Some((generation, takeover, cut));
                        device.set_generation(generation, takeover, cut);
                    },
                    |event| {
                        assert!(device.push(event));
                        queued.push(event);
                        true
                    },
                )
                .expect("Ready recovery prefill");
            let (generation, takeover, _cut) = published.expect("generation publication");
            assert_eq!(takeover, 16_800, "350 ms recovery lead");
            assert!((2..=8).contains(&queued.len()));
            assert!(queued.iter().all(|event| {
                event.sample.is_some_and(|sample| sample.sample == id)
                    && event.expected_sample_identity == Some(identity)
                    && event.confirmation.is_some()
                    && event.generation == generation
                    && event.target_frame >= takeover
            }));
            let first_target = queued[0].target_frame;
            let last_target = queued.last().unwrap().target_frame;
            assert_eq!(first_target, 18_000);
            assert!(queued[1].target_frame > first_target + 256 + frames as u64);
            let callbacks = (last_target + frames as u64).div_ceil(frames as u64);
            assert!(callbacks <= 512);

            let mut pcm = vec![0.0_f32; frames * 2];
            let mut first_peak = 0.0_f32;
            let mut later_peak = 0.0_f32;
            let mut first_copy_end = None;
            for _ in 0..callbacks {
                output.render(&mut pcm);
                let report = output.device().report();
                assert!(pcm.iter().all(|sample| sample.is_finite()));
                assert_eq!(report.asset_queues.sample_installs, 0);
                let peak = pcm
                    .iter()
                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                if first_copy_end.is_none() {
                    first_peak = first_peak.max(peak);
                    if report.submitted_frames >= first_target + 256 {
                        first_copy_end = Some(report.submitted_frames);
                    }
                } else {
                    later_peak = later_peak.max(peak);
                }
            }
            assert!(
                later_peak > 0.0,
                "later onsets must play the installed body"
            );
            let terminal = confirmations.pop_terminal().expect("real copy terminal");
            assert_eq!(terminal.generation, generation);
            assert_eq!(terminal.takeover_frame, takeover);
            assert!(terminal.copied_end_frame > first_target);
            assert!(confirmations.pop_terminal().is_none());
            let report = output.device().report();
            assert_eq!(report.callback_errors, 0);
            assert_eq!(report.callback_scope_misses, 0);
            assert_eq!(report.callback_allocations, 0);
            assert_eq!(report.callback_frees, 0);
            assert_eq!(report.ring_role_conflicts, 0);
            assert_eq!(report.late_events, 0);
            assert_eq!(report.asset_queues.leaked, 0);
            assert_eq!(
                terminal.outcome,
                WindowOutcome::Confirmed,
                "{frames} host frames"
            );
            assert!(
                first_peak > 0.0,
                "{frames} host frames: first ready onset lost"
            );
        }
    }

    pub(crate) fn silent_engine_for_output_selection() -> StudioEngine {
        silent_engine_with_pattern(rustel_core::silence())
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
    fn tick_until_fired(engine: &mut StudioEngine) -> Result<StudioInstall, RuntimeError> {
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
    fn arm_and_fire_launch(engine: &mut StudioEngine, source: &str) -> StudioInstall {
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

    /// A quantised rewind whose incoming score still loads a sample must
    /// not cut the outgoing score at the line. The cut would fade a room
    /// the replacement cannot yet sound, and the flip's choke ramp would
    /// then land on silence, which clicks. The engine withdraws the arm
    /// while the producer holds the flip for loading. When the sample
    /// decodes and the flip lands, the flip owns the cut and the line arm
    /// is not put back.
    #[test]
    fn a_launch_whose_replacement_still_loads_withdraws_the_line_cut_until_the_flip_lands() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("held"));
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        // Async fires on the line while the sample loads; wait would
        // take the first line after it.
        engine.master.set_load_mode(LoadMode::Async);

        assert!(
            engine
                .arm_launch("s(\"held\")", false, 0.25, true)
                .expect("arm")
                .is_some(),
            "a playing engine arms a quantised launch"
        );
        // Drive the countdown to the fire. The clock stays held from the
        // firing tick until the sample decodes, so the line cannot pass
        // while the test watches the hold.
        let install = match tick_until_fired(&mut engine) {
            Ok(install) => install,
            Err(error) => panic!("the launch failed: {error:?}"),
        };
        assert!(
            engine.landing.is_some(),
            "the fired rewind waits for its line"
        );
        assert_ne!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .armed_line_word(),
            0,
            "a rewind arms its line cut at fire time"
        );

        // The session installed the score at fire. While the sample loads,
        // the producer holds the device flip: the line cut is withdrawn and
        // the device generation does not move. Poll for the hold.
        let device_before = engine.live.as_ref().expect("playing").device.generation();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            engine.tick(accepted).expect("tick while loading");
            // Break when the producer reports the hold. The arm must be
            // withdrawn by then: an arm that survives the hold fires into
            // silence at the line.
            if engine
                .live
                .as_ref()
                .expect("playing")
                .producer
                .cut_takeover_deferred_for_loading()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the producer never held the flip for loading"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .line_cut_armed(),
            "the pre-armed line cut is withdrawn while the replacement loads"
        );
        assert_eq!(
            engine.live.as_ref().expect("playing").device.generation(),
            device_before,
            "the held flip must not install the loading score"
        );

        // The sample decodes: the next producer turn publishes the flip to
        // the device and the landing settles.
        hold_clock(&engine, false);
        library.finish_loading_sample_for_test();
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.live.as_ref().expect("playing").device.generation() != install.generation
            && Instant::now() < deadline
        {
            engine.tick(accepted).expect("tick after decode");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            engine.live.as_ref().expect("playing").device.generation(),
            install.generation,
            "the flip lands once the replacement can render"
        );
        // The landed flip owns the cut at the line (AtTakeover). A second
        // arm on top of it would cut the new generation's first window.
        // One more tick lets the engine notice the hold ended.
        engine.tick(accepted).expect("tick after the flip");
        assert_eq!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .armed_line_word(),
            0,
            "no line arm is put back once the replacement's flip has landed"
        );
        assert!(
            !engine.line_cut_withdrawn,
            "the withdrawal is over with the hold"
        );
        assert!(
            !engine
                .live
                .as_ref()
                .expect("playing")
                .producer
                .cut_takeover_deferred_for_loading(),
            "the landed flip ends the producer's loading hold"
        );
    }

    /// A repeat press after the launch fired and before its line is the
    /// same request. It is answered with the launch in flight, not armed
    /// again as a second from-zero install one line later.
    #[test]
    fn a_repeat_press_while_a_fired_launch_is_in_flight_answers_with_that_launch() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        let install = arm_and_fire_launch(&mut engine, source);

        // The line is still ahead: the landing window is open.
        let landing = engine
            .landing
            .as_ref()
            .expect("a fired launch waits for its line");
        let boundary_cycle = landing.boundary_cycle;
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
                < landing.boundary_time,
            "the line must still be ahead for the landing window to bite"
        );

        // The SAME source pressed again inside the window.
        let repeat = engine
            .arm_launch(source, false, 0.25, true)
            .expect("repeat arm")
            .expect("the in-flight launch answers the repeat");
        assert_eq!(
            repeat.boundary_cycle, boundary_cycle,
            "the repeat is answered with the launch already in flight, not the next line"
        );
        assert!(
            engine.pending_launch.is_none(),
            "the repeat must not arm a second launch"
        );
        // Its outcome is the landing's own install: answered once, no
        // second install.
        match engine.take_launch_outcome() {
            Some(Ok(again)) => assert_eq!(
                again.generation, install.generation,
                "the repeat answers with the fired generation"
            ),
            other => panic!(
                "the repeat press must be answered with the fired launch's success, got {other:?}"
            ),
        }

        // Past the line, nothing fires again: the generation the launch
        // installed is the one that plays on.
        hold_clock(&engine, false);
        let generation = engine.generation();
        let until = engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            + 1.0;
        while engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            < until
        {
            engine.tick(accepted).expect("tick past the line");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            engine.generation(),
            generation,
            "a repeat press never re-installs the score"
        );
    }

    /// An EDIT during the same window is a different gesture: it supersedes
    /// the in-flight launch and arms the next line afresh.
    #[test]
    fn an_edit_while_a_fired_launch_is_in_flight_supersedes_it() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        arm_and_fire_launch(&mut engine, "s(\"tri\")");
        assert!(
            engine.landing.is_some(),
            "a fired launch waits for its line"
        );

        let _supersede = engine
            .arm_launch("s(\"saw\")", false, 0.25, true)
            .expect("edit arm")
            .expect("an edit arms a launch");
        assert!(
            engine.pending_launch.is_some(),
            "an edit supersedes the in-flight launch with a pending one"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "an edit waits for its own fire; it is not answered with the old install"
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
    }

    pub(crate) fn clock_now(engine: &StudioEngine) -> f64 {
        device_of(engine).clock_seconds()
    }

    fn device_of(engine: &StudioEngine) -> &LiveScalarDevice {
        &engine.live.as_ref().expect("playing").device
    }

    /// Wait (wall time, clock running) until the silent device's clock has
    /// passed `seconds`.
    fn run_clock_past(engine: &StudioEngine, seconds: f64) {
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

    /// Only a rewind arms the device's line cut: a quantised launch that
    /// joins the running cycle hands over with the ordinary ring-out.
    #[test]
    fn a_launch_that_does_not_rewind_never_arms_the_line() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        assert!(
            engine
                .arm_launch("s(\"tri\")", false, 0.25, false)
                .expect("arm")
                .is_some()
        );
        tick_until_fired(&mut engine).expect("the launch fires");
        assert_eq!(device_of(&engine).armed_line_word(), 0);
    }

    /// Arm a rewind whose line is `line_from_now` seconds from the held
    /// clock, already inside its head-room so the next tick fires it.
    fn arm_past_headroom(engine: &mut StudioEngine, source: &str, line_from_now: f64) {
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

    /// A rewind whose evaluation fails after its line has passed: the
    /// consumer already cut the outgoing score at the line and retired its
    /// onsets. Withdrawing the arm lets the cut go, but only a re-query of
    /// the score still active puts sound back before its next window.
    #[test]
    fn a_rewind_that_fails_after_its_line_withdraws_the_cut_and_refills_the_room() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        arm_past_headroom(&mut engine, "s(\"tri\"", -0.05);
        let generation_before = engine.generation();

        engine.tick(accepted).expect("the firing tick");
        assert!(
            matches!(engine.take_launch_outcome(), Some(Err(_))),
            "the syntax error fails the launch"
        );
        assert!(
            !device_of(&engine).line_cut_armed(),
            "the failed launch withdraws its line cut"
        );
        assert!(engine.landing.is_none(), "a failed launch never lands");
        assert!(engine.recent_rewind.is_none());
        assert_eq!(
            engine.generation(),
            generation_before + 1,
            "the old score is asked again once, so the room refills"
        );
        // The same turn's producer step publishes the refill.
        assert_eq!(device_of(&engine).generation(), generation_before + 1);
        hold_clock(&engine, false);
    }

    /// The same failure before the line: nothing was cut, so the arm is
    /// withdrawn and the old score plays on untouched, with no re-query.
    #[test]
    fn a_rewind_that_fails_before_its_line_withdraws_the_cut_without_a_requery() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        arm_past_headroom(&mut engine, "s(\"tri\"", 0.05);
        let generation_before = engine.generation();

        engine.tick(accepted).expect("the firing tick");
        assert!(matches!(engine.take_launch_outcome(), Some(Err(_))));
        assert_eq!(
            device_of(&engine).armed_line_word(),
            rustel_audio::LINE_ARM_WITHDRAWN
        );
        assert_eq!(engine.generation(), generation_before, "nothing to refill");
        hold_clock(&engine, false);
    }

    /// A fired rewind that the producer refuses (its first window cannot
    /// sound) rolls back to the playing score. Its line cut is withdrawn,
    /// and the refused install does not answer a repeat press.
    #[test]
    fn a_fired_rewind_the_producer_refuses_withdraws_its_line_cut() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let playing = "note('c4').fast(8)";
        engine.evaluate(playing, false).expect("the playing score");
        let deadline = Instant::now() + Duration::from_secs(10);
        while device_of(&engine).generation() != engine.generation() {
            assert!(
                Instant::now() < deadline,
                "the playing score never published"
            );
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        let audible = engine.generation();
        engine
            .session
            .mark_audible_generation(audible)
            .expect("the playing score is the rollback target");

        assert!(
            engine
                .arm_launch("s(\"nosuchsound*4\")", false, 0.25, true)
                .expect("arm")
                .is_some()
        );
        let line = engine.pending_launch.as_ref().expect("armed").boundary_time;
        tick_until_fired(&mut engine).expect("the evaluation itself succeeds");
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.session.active_source() != Some(playing) {
            assert!(
                Instant::now() < deadline,
                "the producer never rolled the refused score back"
            );
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            device_of(&engine).clock_seconds() < line,
            "the clock is held before the line"
        );
        assert!(
            !device_of(&engine).line_cut_armed(),
            "the refused launch's line cut is withdrawn"
        );
        assert!(engine.landing.is_none(), "the refused launch never lands");
        assert!(
            engine.recent_rewind.is_none(),
            "a refused install is not remembered as heard"
        );
        hold_clock(&engine, false);
    }

    /// The commonest pad rewind restarts the score that is already
    /// playing. When the producer refuses that launch, its rollback puts
    /// back the very same text, so nothing in the session tells the refused
    /// install from the restored one: the producer names the generation it
    /// gave up on, and the fired line cut goes with it.
    #[test]
    fn a_refused_rewind_of_the_playing_score_withdraws_its_line_cut() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        // One text, two scores: the second evaluation of it - the launch -
        // names a sound nobody has, and every other evaluation plays.
        let playing = "globalThis.plays = (globalThis.plays ?? 0) + 1\n\
                       s(globalThis.plays === 2 ? \"nosuchsound*4\" : \"tri*8\")";
        engine.evaluate(playing, false).expect("the playing score");
        let deadline = Instant::now() + Duration::from_secs(10);
        while device_of(&engine).generation() != engine.generation() {
            assert!(
                Instant::now() < deadline,
                "the playing score never published"
            );
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        let audible = engine.generation();
        engine
            .session
            .mark_audible_generation(audible)
            .expect("the playing score is the rollback target");

        assert!(
            engine
                .arm_launch(playing, false, 0.25, true)
                .expect("arm")
                .is_some()
        );
        let line = engine.pending_launch.as_ref().expect("armed").boundary_time;
        let mut said = Vec::new();
        let mut collect = |update: StudioUpdate| {
            if let StudioUpdate::Diagnostic(diagnostic) = update {
                said.push(diagnostic);
            }
            Ok(())
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let launched = loop {
            hold_clock(&engine, true);
            engine.tick(&mut collect).expect("tick");
            if let Some(outcome) = engine.take_launch_outcome() {
                break outcome.expect("the evaluation itself succeeds").generation;
            }
            hold_clock(&engine, false);
            assert!(Instant::now() < deadline, "the launch never fired");
            std::thread::sleep(Duration::from_millis(2));
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.session.generation() == launched {
            assert!(
                Instant::now() < deadline,
                "the producer never rolled the refused score back"
            );
            engine.tick(&mut collect).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Read on the turn that saw the rollback: once the rollback's own
        // flip reaches the device it clears the arm too, and a later turn
        // could no longer tell a withdrawn cut from a superseded one.
        let armed_at_rollback = device_of(&engine).line_cut_armed();
        engine
            .tick(&mut collect)
            .expect("the turn after the rollback");
        assert_eq!(
            engine.session.active_source(),
            Some(playing),
            "the rollback put back the same text"
        );
        assert!(
            device_of(&engine).clock_seconds() < line,
            "the clock is held before the line"
        );
        assert!(
            !armed_at_rollback,
            "the refused launch's line cut is withdrawn"
        );
        assert!(
            !device_of(&engine).line_cut_armed(),
            "and it stays withdrawn"
        );
        assert!(engine.landing.is_none(), "the refused launch never lands");
        assert!(
            engine.recent_rewind.is_none(),
            "a refused install is not remembered as heard"
        );
        // Asked last, so a regression of the withdrawal above reports the
        // withdrawal rather than the missing warning it also causes.
        let refused = said
            .iter()
            .find(|diagnostic| {
                diagnostic.kind == "launch" && diagnostic.message.contains("was refused")
            })
            .expect("the player is told why the launch did nothing");
        assert_eq!(refused.level, StudioDiagnosticLevel::Warning);
        hold_clock(&engine, false);
    }

    /// An edit played now over a fired rewind whose flip is held for
    /// loading: nothing of the rewind has sounded, so its line cut must go
    /// at once. Left armed, it cut the edit at the line into dead air.
    #[test]
    fn an_edit_over_a_held_rewind_withdraws_its_line_cut_at_once() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("held"));
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        // Async, so the rewind fires while its sample loads.
        engine.master.set_load_mode(LoadMode::Async);
        assert!(
            engine
                .arm_launch("s(\"held\")", false, 0.25, true)
                .expect("arm")
                .is_some()
        );
        tick_until_fired(&mut engine).expect("the launch fires");
        assert!(device_of(&engine).line_cut_armed(), "armed at fire");

        engine
            .evaluate_guarded("s(\"tri\")", false, || false)
            .expect("the edit");
        assert!(
            !device_of(&engine).line_cut_armed(),
            "the edit withdraws the held rewind's line cut"
        );
        assert!(engine.landing.is_none(), "the edit supersedes the landing");
        engine.tick(accepted).expect("tick after the edit");
        assert!(
            !device_of(&engine).line_cut_armed(),
            "and it stays withdrawn"
        );
        hold_clock(&engine, false);
        library.finish_loading_sample_for_test();
    }

    /// A pad race: A fires, B is armed over it, A is pressed again. When B
    /// is cancelled, A is still in flight and answers the repeat.
    #[test]
    fn cancelling_a_pending_launch_keeps_the_fired_one_in_flight() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let install = arm_and_fire_launch(&mut engine, "s(\"tri\")");
        assert!(
            engine
                .arm_launch("s(\"saw\")", false, 0.25, true)
                .expect("arm B")
                .is_some()
        );
        engine.cancel_pending_launch();
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Err(RuntimeError::Cancelled))
        ));
        assert!(engine.landing.is_some(), "A is still in flight");

        engine
            .arm_launch("s(\"tri\")", false, 0.25, true)
            .expect("A again")
            .expect("answered");
        assert!(engine.pending_launch.is_none(), "nothing armed again");
        match engine.take_launch_outcome() {
            Some(Ok(again)) => assert_eq!(again.generation, install.generation),
            other => panic!("A's repeat must be answered with A, got {other:?}"),
        }
        hold_clock(&engine, false);
    }

    /// Drive a fired rewind landing past its line, so it settles into the
    /// grace memory the way the worker's ticks leave it.
    fn settle_rewind_into_grace(engine: &mut StudioEngine, source: &str) -> StudioInstall {
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

    /// The second press of a fast double press usually arrives after the
    /// line. Inside the grace it is answered with the fired install:
    /// nothing is armed or installed again, and the countdown reads zero.
    #[test]
    fn a_repeat_press_just_after_the_line_is_answered_by_the_grace_memory() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        let install = settle_rewind_into_grace(&mut engine, source);
        let (landed_line_cycle, landed_at) = {
            let recent = engine.recent_rewind.as_ref().expect("checked");
            (recent.boundary_cycle, recent.landed_at)
        };
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
                >= landed_at,
            "the line has passed: this is the window the shipped guard does not cover"
        );

        assert!(
            engine.snapshot().launch.is_none(),
            "past the line there is no countdown to show, grace or not"
        );

        let repeat = engine
            .arm_launch(source, false, 0.25, true)
            .expect("repeat arm")
            .expect("the grace answers the repeat");
        assert_eq!(
            repeat.boundary_cycle, landed_line_cycle,
            "the answer names the line already landed, not a new one"
        );
        assert_eq!(
            repeat.seconds_left, 0.0,
            "the countdown reads zero: nothing is waited for"
        );
        assert!(
            engine.pending_launch.is_none(),
            "the repeat must not arm a second from-zero install"
        );
        assert!(
            engine.snapshot().launch.is_none(),
            "an answered repeat shows no countdown"
        );
        match engine.take_launch_outcome() {
            Some(Ok(again)) => {
                assert_eq!(
                    again.generation, install.generation,
                    "the repeat answers with the generation already sounding"
                );
                assert!(again.answered_repeat, "the answer says nothing installed");
            }
            other => {
                panic!("the repeat press must be answered with the fired install, got {other:?}")
            }
        }

        // Nothing fired again: the generation the launch installed plays on.
        let generation = engine.generation();
        let until = engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            + 0.5;
        while engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            < until
        {
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(engine.generation(), generation, "no second install");
    }

    /// A repeat press is answered on the time its guard read. A second
    /// clock read could cross the line, or the end of the grace, and turn
    /// the press into "evaluate now".
    #[test]
    fn a_repeat_press_is_answered_on_the_time_its_guard_read() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        arm_and_fire_launch(&mut engine, source);
        // The landing edge: the clock (held) is before the line, so move
        // the line a hair behind it and decide the press just before it.
        let now = clock_now(&engine);
        engine.landing.as_mut().expect("in flight").boundary_time = now - 0.001;
        let answer = engine
            .arm_launch_at(now - 0.002, source, false, 0.25, true)
            .expect("repeat")
            .expect("answered before the line, as the guard saw it");
        assert!(answer.seconds_left > 0.0);
        assert!(engine.pending_launch.is_none());
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Ok(StudioInstall {
                answered_repeat: true,
                ..
            }))
        ));

        // The grace edge: settle the landing, then age the memory so the
        // held clock is past the grace while the press was just inside it.
        engine.settle_landing();
        let recent = engine.recent_rewind.as_mut().expect("settled");
        let grace = recent.grace_secs;
        recent.landed_at = now - grace - 0.001;
        let landed_at = recent.landed_at;
        let answer = engine
            .arm_launch_at(landed_at + grace - 1e-6, source, false, 0.25, true)
            .expect("repeat")
            .expect("answered inside the grace, as the guard saw it");
        assert_eq!(answer.seconds_left, 0.0);
        assert!(engine.pending_launch.is_none());
        let _ = engine.take_launch_outcome();
        hold_clock(&engine, false);
    }

    /// An outside clock that retimes the transport during a countdown
    /// moves the time of the line's cycle. The fire, the takeover, cycle
    /// zero and the device cut must land on the moved bar line, not where
    /// it stood when the pad was pressed.
    #[test]
    fn a_launch_lands_on_its_cycle_line_after_the_transport_is_retimed() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.05);
        hold_clock(&engine, true);
        let info = engine
            .arm_launch("s(\"tri\")", false, 1.0, true)
            .expect("arm")
            .expect("armed");
        let now = clock_now(&engine);
        let cps = engine.session.cps();
        engine
            .session
            .retime(now, cps * 1.02, engine.session.cycle_at_time(now));
        let retimed = engine.launch_info().expect("still armed");
        assert!(
            (retimed.seconds_left - info.seconds_left / 1.02).abs() < 1e-6,
            "the countdown follows the new tempo: {} vs {}",
            retimed.seconds_left,
            info.seconds_left
        );
        hold_clock(&engine, false);

        // Where the line's cycle falls under the new mapping. (The rewind
        // puts cycle zero there once it fires, so read it before.)
        let moved_line = now + retimed.seconds_left;
        tick_until_fired(&mut engine).expect("the launch fires");
        let landing = engine.landing.as_ref().expect("in flight");
        assert!(
            (landing.boundary_time - moved_line).abs() * 48_000.0 < 1.0,
            "the landing waits for the moved line: {} vs {moved_line}",
            landing.boundary_time
        );
        // The same tick's producer step usually publishes the flip, which
        // carries the cut itself and clears the arm; one still armed must
        // name the moved line.
        let word = device_of(&engine).armed_line_word();
        if word & 0b01 != 0 {
            assert_eq!(
                word >> 2,
                (landing.boundary_time * 48_000.0).round() as u64,
                "the device cut is armed at the moved line"
            );
        }
        hold_clock(&engine, false);
    }

    /// The grace is short on purpose: past it, the same score pressed again
    /// is a genuine restart and arms a fresh launch, as a rewind should.
    #[test]
    fn a_repeat_press_past_the_grace_restarts_for_real() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        settle_rewind_into_grace(&mut engine, source);
        let recent = engine
            .recent_rewind
            .as_mut()
            .expect("the landing settled into the grace memory");
        recent.landed_at -= recent.grace_secs + 0.1;

        let restart = engine
            .arm_launch(source, false, 0.25, true)
            .expect("restart arm")
            .expect("an expired memory is no answer");
        assert!(restart.seconds_left > 0.0, "a fresh line is waited for");
        assert!(
            engine.pending_launch.is_some(),
            "the expired press arms a fresh launch"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "it waits for its own fire, not answered with the old install"
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
    }

    /// The grace is half the launch's unit at its tempo, with no cap. At
    /// 0.5 cps that is 0.25 s for a beat, 1 s for a cycle and 4 s for a
    /// four-cycle phrase.
    #[test]
    fn a_repeat_press_belongs_to_the_nearest_line_for_every_quantise_size() {
        assert_eq!(rewind_grace_secs(0.25, 0.5), 0.25);
        assert_eq!(rewind_grace_secs(1.0, 0.5), 1.0);
        assert_eq!(rewind_grace_secs(4.0, 0.5), 4.0);
        assert_eq!(rewind_grace_secs(8.0, 0.5), 8.0);
        assert_eq!(rewind_grace_secs(0.25, 2.0), 0.0625);
        for (unit, cps) in [
            (0.0, 0.5),
            (0.25, 0.0),
            (f64::NAN, 0.5),
            (0.25, f64::INFINITY),
        ] {
            assert_eq!(
                rewind_grace_secs(unit, cps),
                0.0,
                "no line to be near, no grace: {unit} at {cps}"
            );
        }
    }

    /// With beat quantise at `setCpm(120/4)`, a rewind pressed again and
    /// again restarts the score on each beat line. A press just after the
    /// line is still the launch that landed. A press half a beat later
    /// arms the next beat.
    #[test]
    fn a_rewind_pressed_again_on_the_next_beat_restarts_on_that_beat() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        assert_eq!(engine.session.cps(), 0.5, "120 BPM with four beats a cycle");
        let source = "s(\"tri\")";
        settle_rewind_into_grace(&mut engine, source);
        hold_clock(&engine, true);
        let now = clock_now(&engine);
        let recent = engine.recent_rewind.as_mut().expect("settled");
        assert_eq!(
            recent.grace_secs, 0.25,
            "a beat launch's grace is half a beat"
        );

        // A double landing just after the line: answered, nothing armed.
        recent.landed_at = now - 0.05;
        engine
            .arm_launch_at(now, source, false, 0.25, true)
            .expect("repeat")
            .expect("answered");
        assert!(engine.pending_launch.is_none(), "a double is not a restart");
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Ok(StudioInstall {
                answered_repeat: true,
                ..
            }))
        ));

        // The press on the next beat (0.3 s after the line: past half a
        // beat, before the next line's head-room): a fresh rewind on it.
        engine.recent_rewind.as_mut().expect("still held").landed_at = now - 0.3;
        let next = engine
            .arm_launch_at(now, source, false, 0.25, true)
            .expect("restart arm")
            .expect("armed for the next beat");
        assert!(
            engine.pending_launch.is_some(),
            "the next beat's press arms a restart instead of being answered"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "it waits for its own line"
        );
        // The next beat line past the launch head-room: at most one beat
        // plus the head-room away, never a second beat further.
        assert!(
            next.seconds_left > 0.0 && next.seconds_left <= 0.5 + engine.launch_headroom(),
            "the restart lands on the next beat line: {}",
            next.seconds_left
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
        hold_clock(&engine, false);
    }

    /// A different score is an edit even inside the grace: it arms a fresh
    /// launch and is not answered with the install it would replace.
    #[test]
    fn an_edit_inside_the_grace_restarts_for_real() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        settle_rewind_into_grace(&mut engine, "s(\"tri\")");

        let edit = engine
            .arm_launch("s(\"saw\")", false, 0.25, true)
            .expect("edit arm")
            .expect("a different score is not answered by the grace");
        assert!(edit.seconds_left > 0.0, "a fresh line is waited for");
        assert!(
            engine.pending_launch.is_some(),
            "the edit arms a fresh launch"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "it waits for its own fire, not answered with the old install"
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
    }

    /// The sliver the first fix left open: press 2 arrives after the line
    /// has passed but BEFORE any turn's tick has settled the landing, so
    /// the landing still dangles. Settling inside arm_launch turns it into
    /// the answerable memory; nothing is armed, nothing re-installs.
    #[test]
    fn a_press_between_the_line_and_the_next_tick_is_still_answered() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        // Arm first. The fire happens inside the head-room, before the
        // line, and the same tick's settle cannot touch a landing whose
        // line is still ahead: that is the natural dangling state.
        let armed = engine
            .arm_launch(source, false, 0.25, true)
            .expect("arm")
            .expect("a playing engine arms a quantised launch");
        let boundary_cycle = armed.boundary_cycle;

        // Tick until the fire. The landing is left waiting for its line.
        let install = match tick_until_fired(&mut engine) {
            Ok(install) => install,
            Err(error) => panic!("the launch failed: {error:?}"),
        };
        assert!(engine.landing.is_some(), "the fired landing waits");
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
                < engine.landing.as_ref().expect("checked").boundary_time,
            "the fire lands inside the head-room, before the line"
        );
        hold_clock(&engine, false);

        // Now cross the line without a tick: the worker reaches the next
        // tick only after it handles a command that arrives here.
        let until = engine
            .landing
            .as_ref()
            .expect("the landing is still there")
            .boundary_time
            + 0.02;
        while engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            < until
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        // The dangling state this test is about: fired, line passed, no
        // turn's settle has run since.
        assert!(
            engine.landing.is_some(),
            "the landing dangles past its line"
        );
        assert!(engine.recent_rewind.is_none(), "not settled yet");

        // Press 2 lands in the sliver.
        let repeat = engine
            .arm_launch(source, false, 0.25, true)
            .expect("repeat arm")
            .expect("the settled memory answers the repeat");
        assert_eq!(
            repeat.boundary_cycle, boundary_cycle,
            "answered with the line already landed"
        );
        assert!(engine.pending_launch.is_none(), "nothing was armed again");
        match engine.take_launch_outcome() {
            Some(Ok(again)) => assert_eq!(
                again.generation, install.generation,
                "the answer is the generation already sounding"
            ),
            other => panic!("the sliver press must be answered, got {other:?}"),
        }
    }

    #[test]
    fn no_open_input_clears_obsolete_channel_validation() {
        for close in [false, true] {
            let mut engine = silent_engine_for_output_selection();
            engine.session.set_direct_diagnostic_logging(false);
            engine.session.set_audio_input_channels(Some(2));
            engine
                .session
                .evaluate("s('in').n(3).fast(8)")
                .expect("input score");
            if close {
                engine.close_input();
            } else {
                engine.ensure_input();
            }
            assert_eq!(engine.snapshot().input_channels, 0);
            let events = engine.session.schedule_audio_at(0.0, 48_000).unwrap();
            assert!(!events.is_empty());
            assert!(events.iter().all(|event| matches!(
                event.synth,
                Some(rustel_audio::SynthSource::Input { channel: 3 })
            )));
        }
    }

    fn silent_engine_with_pattern(pattern: rustel_core::Pattern) -> StudioEngine {
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
    pub(super) fn play_silently(engine: &mut StudioEngine) {
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

    fn wait_piano_audio(engine: &mut StudioEngine, voices: u64) {
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

    /// With nothing open there is no audio side to speak of. Once an
    /// output opens its DSP is there from the start, and its rings grow by
    /// a slot an event rather than all at once.
    #[test]
    fn an_open_output_reports_what_its_audio_holds() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("studio");
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        assert_eq!(engine.snapshot().audio_memory, None);

        engine
            .session
            .set_pattern(rustel_core::silence())
            .expect("native pattern");
        play_silently(&mut engine);
        let opened = engine.snapshot().audio_memory.expect("an output is open");
        assert!(opened.backend > 0, "{opened:?}");
        assert!(opened.other > 0, "the master's analysis tap: {opened:?}");
        let analysis = engine.audio_device().expect("open").memory().analysis;
        let backlog = engine
            .live
            .as_ref()
            .map_or(0, |live| live.producer.backlog_bytes());
        assert_eq!(
            opened.other,
            analysis + backlog,
            "the taps and the backlog, not the engine's own trace table"
        );
        assert_eq!(
            (
                opened.event_ring,
                opened.input,
                opened.record,
                opened.reverbs
            ),
            (0, 0, 0, 0)
        );

        let slot = std::mem::size_of::<rustel_audio::QueuedAudioEvent>();
        engine.piano_note_on(0, 60, 100).unwrap();
        let one = engine.snapshot().audio_memory.expect("still open");
        assert_eq!(one.event_ring, slot, "one press, one slot");
        engine.piano_note_on(4, 64, 100).unwrap();
        let two = engine.snapshot().audio_memory.expect("still open");
        assert_eq!(two.event_ring, 2 * slot);
        assert_eq!(two.backend, opened.backend, "measured once, when prepared");
    }

    #[test]
    fn piano_keys_overlap_repeat_and_release_independently() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        let source = engine.active_source().map(str::to_owned);
        engine.piano_note_on(0, 60, 100).unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        engine.piano_note_on(0, 72, 127).unwrap(); // held key/octave change cannot retrigger
        wait_piano_audio(&mut engine, 2);
        assert_eq!(engine.piano.iter().flatten().count(), 2);
        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 1);
        assert!(engine.piano[0].is_none());
        assert!(engine.piano[4].is_some());
        assert_eq!(engine.active_source(), source.as_deref());
        assert!(!engine.session.transport().is_stopped());
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
        assert!(engine.piano.iter().all(Option::is_none));
        engine.retire_finished_audition_transport();
        assert!(
            !engine.session.transport().is_stopped(),
            "score keeps its transport"
        );
    }

    #[test]
    fn piano_is_audible_over_a_sampled_score_without_replacing_its_voice() {
        use rustel_core::Value;

        let mut engine = silent_engine_with_pattern(rustel_core::pure(Value::object([
            ("s".into(), Value::Str("piano".into())),
            ("note".into(), Value::F64(36.0)),
            ("gain".into(), Value::F64(0.1)),
        ])));
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("piano"));
        library.finish_loading_sample_frames_for_test(480_000);
        engine.session.set_sample_library_for_test(library);
        engine.tick(|_| Ok(())).expect("schedule the score");
        wait_piano_audio(&mut engine, 1);
        let device = engine.audio_device().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        let score_peak = device.take_levels().peak;
        assert!(score_peak > 0.001, "the sampled score is audible");
        assert!(engine.piano_event(0, 36, 100).unwrap().sample.is_none());

        engine.piano_note_on(0, 36, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        let device = engine.audio_device().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        let combined_peak = device.take_levels().peak;
        assert!(
            combined_peak > score_peak * 1.5,
            "keyboard oscillator is audible: {combined_peak} over {score_peak}"
        );

        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 1);
        let device = engine.audio_device().unwrap();
        device.take_levels();
        settle_callbacks(device, device.callbacks(), 16);
        let remaining_peak = device.take_levels().peak;
        assert!(
            (remaining_peak - score_peak).abs() < 0.0001,
            "key-up preserves the score: {remaining_peak} vs {score_peak}"
        );
        assert!(engine.is_playing());
    }

    #[test]
    fn piano_idle_output_is_audible_without_starting_or_advancing_a_score() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        engine.stop(Duration::from_millis(500));
        let before = engine.snapshot();
        let source = engine.active_source().map(str::to_owned);
        engine.piano_output = Some(engine.open_output().unwrap());
        engine.piano_note_on(0, 60, 100).unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        let device = engine.piano_output.as_ref().expect("output only");
        settle_callbacks(device, device.callbacks(), 16);
        assert!(device.take_levels().peak > 0.0001, "direct PCM is audible");
        engine.idle_turn(|_| Ok(()));
        let during = engine.snapshot();
        assert!(!during.playing);
        assert!(!engine.is_playing());
        assert!(engine.live.is_none(), "no score producer created");
        assert!(engine.session.transport().is_stopped());
        assert_eq!(during.cycle, before.cycle);
        assert_eq!(during.cps, before.cps);
        assert_eq!(during.session_generation, before.session_generation);
        assert_eq!(engine.active_source(), source.as_deref());
        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 1);
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
        engine.idle_turn(|_| Ok(()));
        assert!(engine.piano_output.is_none());
        assert_eq!(engine.snapshot().cycle, before.cycle);
    }

    #[test]
    fn piano_outlives_browser_preview_without_keeping_its_transport_running() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        engine.live.as_mut().unwrap().audition_owned = true;
        engine.piano_note_on(0, 60, 100).unwrap();
        wait_piano_audio(&mut engine, 1);
        engine.retire_finished_audition_transport();
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert!(engine.piano_output.is_some());
        wait_piano_audio(&mut engine, 1);
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
    }

    #[test]
    fn piano_during_stop_preserves_tail_recording_and_hard_stop() {
        let directory = tempfile::tempdir().unwrap();
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        engine
            .start_recording(directory.path().join("piano-tail.wav"))
            .unwrap();
        let mut score = engine.piano_event(0, 48, 100).unwrap();
        score.controls.piano = false;
        score.duration_secs = 30.0;
        score.cut = None;
        let device = engine.audio_device().unwrap();
        score.target_frame = device.clock_frames();
        assert!(device.push(score));
        wait_piano_audio(&mut engine, 1);
        engine.pump_recording();
        let stream = engine.device_report().unwrap().stream_id;
        engine.request_stop();
        let frozen = engine.snapshot().cycle;
        // Exercise Stop -> keypress in one command batch, before any drain tick.
        engine.piano_note_on(4, 64, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        assert!(
            engine.is_playing(),
            "a keypress cannot retire the score tail"
        );
        assert!(engine.piano_output.is_none());
        assert!(matches!(
            engine.tick(|_| Ok(())).unwrap(),
            StudioTick::Stopping
        ));
        assert!(engine.is_stopping());
        assert_eq!(engine.device_report().unwrap().stream_id, stream);
        assert_eq!(engine.snapshot().cycle, frozen);
        let before = engine.recording.as_ref().unwrap().frames;
        let device = engine.audio_device().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        engine.tick(|_| Ok(())).unwrap();
        assert!(
            engine.recording.as_ref().unwrap().frames > before,
            "the original recording tap continues through score and keyboard audio"
        );
        engine.stop_piano();
        wait_piano_audio(&mut engine, 1);
        engine.tick(|_| Ok(())).unwrap();
        assert!(
            engine.live.is_some(),
            "helper exit must not cut the old tail"
        );
        assert_eq!(engine.snapshot().cycle, frozen);
        assert!(engine.master_bus().take_levels().peak > 0.0001);
        let stop = engine
            .stop(Duration::from_millis(500))
            .expect("shared output still needs a stop acknowledgement");
        assert!(stop.acknowledged);
        assert!(stop.report.stop_acknowledged);
        assert_eq!(stop.report.stream_id, stream);
        assert!(
            engine.audio_device().is_none(),
            "hard stop closes every sound"
        );
        let take = engine.stop_recording().expect("joined recording");
        assert!(take.frames > 0);
        assert!(take.error.is_none());
        assert!(take.final_signal.unwrap().sample_count > 0);
    }

    #[test]
    fn piano_cut_sounding_cancels_queued_keys_without_forgetting_their_releases() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .hold_silent_clock_for_test(true)
        );
        engine.piano_note_on(0, 60, 100).unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        engine.cut_sounding();
        for key in [0, 4] {
            assert!(
                engine.piano[key].unwrap().releasing,
                "global cut must retain queued key release ownership"
            );
        }
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .hold_silent_clock_for_test(false)
        );
        wait_piano_audio(&mut engine, 0);
        assert!(engine.piano.iter().all(Option::is_none));
        // A future press is a fresh voice, and still has an effective key-up.
        engine.piano_note_on(0, 72, 100).unwrap();
        wait_piano_audio(&mut engine, 1);
        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 0);
    }

    #[test]
    fn piano_queued_press_survives_a_score_generation_change() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        let device = &engine.live.as_ref().unwrap().device;
        assert!(device.hold_silent_clock_for_test(true));
        engine.piano_note_on(0, 60, 100).unwrap();
        let device = &engine.live.as_ref().unwrap().device;
        // The onset remains in the ring when an unrelated score update wins.
        device.set_generation(
            device.generation() + 1,
            device.clock_frames(),
            TakeoverCut::None,
        );
        assert!(device.hold_silent_clock_for_test(false));
        wait_piano_audio(&mut engine, 1);
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
    }

    #[test]
    fn piano_fast_repress_lands_after_queued_release_even_across_generation_changes() {
        for replace_generation in [false, true] {
            let mut engine = silent_engine_with_pattern(rustel_core::silence());
            engine.session.set_sample_library_for_test(Arc::new(
                rustel_runtime::samples::SampleLibrary::empty_without_loading(),
            ));
            engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
            engine.piano_note_on(0, 60, 100).unwrap();
            wait_piano_audio(&mut engine, 1);
            assert!(
                engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .hold_silent_clock_for_test(true)
            );
            engine.piano_note_off(0);
            if replace_generation {
                hush(&engine.live.as_ref().unwrap().device);
            }
            engine.piano_note_on(0, 72, 100).unwrap();
            assert!(
                engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .hold_silent_clock_for_test(false)
            );
            wait_piano_audio(&mut engine, 1);
            // Let the old release and its complete fade pass. The re-pressed
            // voice still sounds and has not inherited a release request.
            let device = &engine.live.as_ref().unwrap().device;
            settle_callbacks(device, device.callbacks(), 16);
            assert_eq!(device.report().realtime_pressure.active_voices, 1);
            assert!(!engine.piano[0].unwrap().releasing);
            engine.stop_piano();
            wait_piano_audio(&mut engine, 0);
        }
    }

    #[test]
    fn piano_sounds_ignore_loading_and_ready_sample_banks() {
        use super::super::PianoSound;

        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        for sound in [PianoSound::Sine, PianoSound::Triangle, PianoSound::Square] {
            let library = Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test(sound.key()),
            );
            engine
                .session
                .set_sample_library_for_test(Arc::clone(&library));
            engine.set_piano_settings(sound, 130);
            engine.prepare_piano().unwrap();
            let oscillator = engine.piano_event(0, 60, 100).unwrap();
            assert!(oscillator.sample.is_none());
            assert!(oscillator.wavetable.is_none());
            engine.piano_note_on(0, 60, 100).unwrap();
            wait_piano_audio(&mut engine, 1);
            engine.piano_note_off(0);
            wait_piano_audio(&mut engine, 0);

            library.finish_loading_sample_for_test();
            engine.piano_note_on(0, 60, 100).unwrap();
            wait_piano_audio(&mut engine, 1);
            assert!(engine.piano_event(0, 60, 100).unwrap().sample.is_none());
            assert!(
                engine.retained_samples.is_empty(),
                "keys never install sample PCM"
            );
            engine.stop_piano();
            wait_piano_audio(&mut engine, 0);
        }
    }

    #[test]
    fn piano_first_key_sounds_without_a_sample_library_while_the_score_stays_idle() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            default_samples: false,
            ..StudioConfig::default()
        })
        .expect("studio");
        engine.session.transport().stop();
        assert!(engine.session.sample_library().is_none());
        let before = engine.snapshot();
        // Unit tests do not install the binary's allocation tripwire. Supply
        // the silent output as in the other idle keyboard fixture.
        engine.piano_output = Some(engine.open_output().unwrap());
        engine.prepare_piano().unwrap();
        engine.piano_note_on(0, 60, 100).unwrap();
        wait_piano_audio(&mut engine, 1);
        let device = engine.piano_output.as_ref().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        assert!(device.take_levels().peak > 0.0001, "first key is audible");
        engine.idle_turn(|_| Ok(()));
        let during = engine.snapshot();
        assert!(!during.playing);
        assert_eq!(during.cycle, before.cycle);
        assert_eq!(during.session_generation, before.session_generation);
        assert!(engine.session.transport().is_stopped());
        assert!(
            engine.session.sample_library().is_none(),
            "no sample loader was started"
        );
        assert!(
            engine.retained_samples.is_empty(),
            "no sample PCM is retained"
        );
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
        assert!(
            engine
                .stop(Duration::from_millis(500))
                .unwrap()
                .acknowledged
        );
    }

    #[test]
    fn a_score_tail_finishes_naturally_while_a_keyboard_note_stays_held() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        let mut score = engine.piano_event(0, 48, 100).unwrap();
        score.controls.piano = false;
        score.duration_secs = 0.6;
        score.cut = None;
        let device = engine.audio_device().unwrap();
        score.target_frame = device.clock_frames();
        assert!(device.push(score));
        wait_piano_audio(&mut engine, 1);
        let stream = engine.device_report().unwrap().stream_id;
        engine.request_stop();
        let frozen = engine.snapshot().cycle;
        engine.tick(|_| Ok(())).unwrap();
        let generation = engine.audio_device().unwrap().generation();
        engine.prepare_piano().unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        assert!(
            engine.is_stopping(),
            "F12 and keypress do not retire the tail"
        );
        assert_eq!(engine.audio_device().unwrap().generation(), generation);

        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.is_playing() {
            engine.tick(|_| Ok(())).unwrap();
            assert!(
                Instant::now() < deadline,
                "held keyboard note prolonged score stop"
            );
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(!engine.snapshot().stopping);
        assert_eq!(engine.snapshot().cycle, frozen);
        assert_eq!(engine.device_report().unwrap().stream_id, stream);
        wait_piano_audio(&mut engine, 1);
        let device = engine.audio_device().unwrap();
        device.take_levels();
        settle_callbacks(device, device.callbacks(), 8);
        assert!(
            device.take_levels().peak > 0.001,
            "held keyboard remains audible"
        );
        assert!(!engine.piano[4].unwrap().releasing);
        engine.piano_note_off(4);
        wait_piano_audio(&mut engine, 0);
        assert!(
            engine
                .stop(Duration::from_millis(500))
                .unwrap()
                .acknowledged
        );
    }

    #[test]
    fn piano_selected_synth_is_immediate_held_and_separate_from_preview_groups() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(super::super::PianoSound::Triangle, 130);
        let event = engine.piano_event(0, 69, 127).unwrap();
        assert_eq!(event.freq_hz, 440.0);
        assert_eq!(event.duration_secs, f32::INFINITY);
        assert_eq!(event.controls.preview_epoch, 0);
        assert_eq!(event.ui_visuals, 0);
        assert!(event.sample.is_none());
        engine.set_piano_settings(super::super::PianoSound::Triangle, 100);
        let original = engine.piano_event(0, 69, 127).unwrap();
        assert!((event.gain - original.gain * 1.3).abs() < 0.00001);
        engine.set_piano_settings(super::super::PianoSound::Square, 0);
        assert_eq!(engine.piano_event(0, 69, 127).unwrap().gain, 0.0);
        for slot in 0..=MAX_AUDITION_RUN_NOTES {
            for key in 0..PIANO_KEYS {
                assert_ne!(audition_cut_group(slot), piano_cut_group(key));
            }
        }
    }

    #[test]
    fn a_finished_audition_stops_only_the_transport_it_opened() {
        let mut audition = silent_engine_with_pattern(rustel_core::silence());
        audition.live.as_mut().unwrap().audition_owned = true;
        audition.audition_horizon_frame = 0;
        audition.retire_finished_audition_transport();
        assert!(
            audition.session.transport().is_stopped(),
            "the silent score opened for the preview retires with it"
        );

        let mut set = silent_engine_with_pattern(rustel_core::silence());
        set.audition_horizon_frame = 0;
        set.retire_finished_audition_transport();
        assert!(
            !set.session.transport().is_stopped(),
            "a real set keeps its clock after a preview"
        );
    }

    #[test]
    fn recording_takes_ownership_of_a_preview_transport() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.live.as_mut().unwrap().audition_owned = true;
        engine.audition_horizon_frame = 0;

        engine
            .start_recording(directory.path().join("preview-take.wav"))
            .expect("start take on preview transport");
        assert!(!engine.live.as_ref().unwrap().audition_owned);
        engine.retire_finished_audition_transport();
        assert!(
            !engine.session.transport().is_stopped(),
            "the expired preview must not stop the active take"
        );
        engine.stop_recording().expect("finished take");
    }

    /// A `midikeys` press re-queries the pattern from close to now. It does
    /// not wait for the scheduler frontier. See `hear_midi_keys_now`.
    #[test]
    fn a_key_press_asks_the_pattern_again_from_close_to_now() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine
            .session
            .evaluate(
                "const kb = await midikeys('keyboard')
$: kb(0.25).s(\"tri\")
",
            )
            .expect("a score that plays a keyboard");
        // The port the score asked for, and a turn to settle on it.
        engine.tick(|_| Ok(())).expect("tick");
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");

        let quiet = engine.generation();
        engine.tick(|_| Ok(())).expect("tick");
        assert_eq!(
            engine.generation(),
            quiet,
            "a turn with no key pressed asks the pattern nothing"
        );

        // Proof the assertion above can fail: without the press, the
        // generation is still `quiet` after any number of turns.
        for _ in 0..3 {
            engine.tick(|_| Ok(())).expect("tick");
        }
        assert_eq!(engine.generation(), quiet);

        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        engine.tick(|_| Ok(())).expect("tick");
        assert!(
            engine.generation() > quiet,
            "the press asks the pattern again rather than waiting for the frontier"
        );

        // And once asked, it is not asked again for the same press.
        let asked = engine.generation();
        engine.tick(|_| Ok(())).expect("tick");
        assert_eq!(engine.generation(), asked, "one press, one re-query");
    }

    /// A racing horizon fill must not steal a fresh press's placement.
    ///
    /// The first trigger query that selects a press claims it. A producer
    /// step deep in its horizon can win that race and pin the press well
    /// past the takeover frame, as happens when a heavy pattern shares the
    /// stack. The press must sound at the takeover frame, the earliest frame
    /// the device can voice it, and not at the pin of the fill.
    ///
    /// The test stages the race: it publishes the press, runs one query that
    /// begins well past the takeover of the re-query (the fill), then runs
    /// the engine turn that answers the press.
    #[test]
    fn a_horizon_fill_cannot_steal_a_fresh_key_press() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        // A fixed takeover offset keeps the race deterministic whatever the
        // host's own pipeline latency would make the margin (WSLg drifts).
        engine.session.set_continuity_margin(0.1);
        engine
            .session
            .evaluate("const kb = await midikeys('keyboard')\n$: kb(0.25).s(\"tri\")\n")
            .expect("a score that plays a keyboard");
        engine.tick(|_| Ok(())).expect("tick");
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");

        let now = engine.live.as_ref().expect("live").device.clock_seconds();
        // The press lands, and before the engine can answer it a producer
        // step whose query begins past the re-query's takeover selects it.
        // Staging the fill after the press is what a producer step deep in
        // a heavy stack looks like from the ring.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        let fill_events = engine
            .session
            .schedule_audio_through(now + 0.25, now + 0.75, 48_000)
            .expect("the racing horizon fill");
        // The pattern is silent without a press, so the single fill event is
        // the press. The fill pins it at its own begin, now + 0.25 s.
        // `target_frame` counts frames at 48 kHz.
        assert_eq!(
            fill_events.len(),
            1,
            "the fill claims the press at its own begin"
        );
        let fill_pin = fill_events[0].target_frame as f64 / 48_000.0;
        // Well past the 0.1 takeover: the fill's claim is the steal itself.
        // (Window quantization shifts the pin a few ms; nobody hears that.)
        assert!(
            fill_pin >= now + 0.2,
            "the fill pinned the press past the takeover (pin {fill_pin})"
        );

        // The turn that answers the press re-queries from close to now. Its
        // takeover frame (now + 0.1) is inside its own query span, and the
        // fill's pin is not. If the stale pin of the outgoing generation
        // survived, the re-query would sound nothing.
        engine.tick(|_| Ok(())).expect("tick");

        // The press must now be placed at the takeover frame - the earliest
        // moment the device could voice it - not parked at the fill's begin.
        // `select` with no placement offer only reads; it cannot move a pin.
        let takeover_cycle = engine.session.cycle_at_time(now + 0.1);
        let stale_cycle = engine.session.cycle_at_time(now + 0.25);
        let mut hits = Vec::new();
        port.keys.select(
            0.0,
            stale_cycle + 1.0,
            rustel_core::midi_in::now_nanos(),
            None,
            &mut hits,
        );
        assert_eq!(hits.len(), 1, "one press, one placement");
        let placed = hits[0].num as f64 / hits[0].den as f64;
        assert!(
            placed < stale_cycle - 0.01,
            "the press must not stay where the racing fill pinned it \
             ({placed} >= {stale_cycle})"
        );
        assert!(
            placed <= takeover_cycle + 0.06,
            "the press must be re-claimed at the re-query's takeover \
             (~{takeover_cycle}), not parked ahead at {placed}"
        );
    }

    /// A pad bound to scene launch is a transport button, not a key. Its
    /// press arms no requery, so the session generation does not move. A
    /// key with no scene binding still requeries.
    #[test]
    fn a_launch_pad_press_never_arms_the_keys_requery() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine
            .session
            .evaluate("const kb = await midikeys('keyboard')\n$: kb(0.25).s(\"tri\")\n")
            .expect("a score that plays a keyboard");
        engine.set_launch_pads(vec![(60, 1)]);
        engine.tick(|_| Ok(())).expect("baseline tick");
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");

        // The launch pad's press: the ring stays empty and the watcher -
        // which counts musical presses only - sees no change, so the
        // session generation cannot move.
        let before = engine.session.generation();
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        engine
            .tick(|_| Ok(()))
            .expect("tick over the transport press");
        assert_eq!(
            engine.session.generation(),
            before,
            "a transport press must arm no requery"
        );
        let mut hits = Vec::new();
        port.keys.select(
            0.0,
            4.0,
            rustel_core::midi_in::now_nanos(),
            Some((1, 8)),
            &mut hits,
        );
        assert!(hits.is_empty(), "a transport press is never musical");

        // The control: an unbound note on the same port changes the press
        // count and arms the at-once requery.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 61, 100);
        engine
            .tick(|_| Ok(()))
            .expect("tick over the musical press");
        assert!(
            engine.session.generation() > before,
            "a musical press must still requery at once"
        );
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

    fn wait_for_output_notes(engine: &mut StudioEngine, accepted_before: u64) {
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

    #[test]
    fn output_preparation_failure_restores_score_through_studio_ticks() {
        use rustel_core::{Value, pure};
        let pattern = pure(Value::object(vec![
            ("s".into(), Value::Str("sine".into())),
            ("note".into(), Value::F64(60.0)),
            ("gain".into(), Value::F64(0.25)),
        ]))
        .fast(16.into());
        let mut engine = silent_engine_with_pattern(pattern);
        wait_for_output_notes(&mut engine, 0);
        let before = engine.live.as_ref().unwrap().device.report();
        let generation = engine.generation();
        let preference = engine.preferred_output.clone();
        let mut attempts = 0;
        let error = engine
            .set_output_device_with("replacement", |device, selector, samples| {
                attempts += 1;
                if attempts == 1 {
                    assert_eq!(selector, Some("replacement"));
                    device.fail_output_replacement_for_test()
                } else {
                    assert_eq!(attempts, 2, "only one recovery attempt is allowed");
                    assert_eq!(selector, Some(rustel_audio::SILENT_OUTPUT_NAME));
                    device.recycle_output_to_with_samples(selector, samples, || false)
                }
            })
            .expect_err("requested output was not installed");
        assert!(error.to_string().contains("output preparation failed"));
        // Drive the ordinary health check, refill and callback, not a manual
        // producer push. A healthy empty replacement is not recovered music.
        wait_for_output_notes(&mut engine, before.accepted_events);
        assert_eq!(attempts, 2);
        assert_eq!(engine.generation(), generation + 1);
        assert_eq!(engine.preferred_output, preference);
        assert_eq!(
            engine.output_name().as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        let after = engine.live.as_ref().unwrap().device.report();
        assert!(after.callbacks > before.callbacks);
        assert!(after.playhead_nanos > before.playhead_nanos);
    }

    /// A stream that reports an error is reopened, and the set plays on.
    ///
    /// Taking a Bluetooth headset's microphone drops it out of A2DP and
    /// the output stream with it, which is a device going away under the
    /// music rather than a machine that cannot keep up: the studio said
    /// "failed while playing live", stopped, and left `no output` behind.
    /// It reopens instead, the way it does for a clock that stops
    /// advancing, and says so in the log.
    #[test]
    fn an_output_stream_that_fails_is_opened_again_rather_than_ending_the_set() {
        use rustel_core::{Value, pure};
        let pattern = pure(Value::object(vec![
            ("s".into(), Value::Str("sine".into())),
            ("note".into(), Value::F64(60.0)),
            ("gain".into(), Value::F64(0.25)),
        ]))
        .fast(16.into());
        let mut engine = silent_engine_with_pattern(pattern);
        wait_for_output_notes(&mut engine, 0);
        let sounded = engine
            .live
            .as_ref()
            .expect("live output")
            .device
            .report()
            .accepted_events;

        engine
            .live
            .as_ref()
            .expect("live output")
            .device
            .fail_output_stream_for_test();
        let mut said: Vec<StudioDiagnostic> = Vec::new();
        let tick = engine
            .tick(|update| {
                if let StudioUpdate::Diagnostic(diagnostic) = &update {
                    said.push(diagnostic.clone());
                }
                Ok(())
            })
            .expect("the set plays on");
        assert!(
            matches!(tick, StudioTick::Running { .. }),
            "a failed stream is reopened, not the end of the set: {tick:?}"
        );
        assert!(engine.is_playing());
        assert!(
            !engine
                .live
                .as_ref()
                .expect("live output")
                .device
                .output_failed(),
            "and the stream that replaced it is a healthy one"
        );
        assert!(
            said.iter().any(|diagnostic| {
                diagnostic.kind == "audio-recycled"
                    && diagnostic.level == StudioDiagnosticLevel::Info
                    && diagnostic.message.contains("opened again")
            }),
            "the log says what happened: {said:?}"
        );
        // And the music is really on the new stream: events accepted
        // past what the old one had taken, which is what `no output` cost
        // and what a reopen is for.
        wait_for_output_notes(&mut engine, sounded);

        // Failing again inside the cooldown is a device that is not coming
        // back: the studio says so rather than reopening it every turn.
        engine
            .live
            .as_ref()
            .expect("live output")
            .device
            .fail_output_stream_for_test();
        let error = engine
            .tick(|_| Ok(()))
            .expect_err("a device that keeps failing ends the set");
        assert!(
            error.to_string().contains("failed while playing live"),
            "{error}"
        );
        assert!(engine.session.transport().is_stopped());
    }

    #[test]
    fn output_recovery_failure_stops_without_repeated_attempts() {
        let mut engine = silent_engine_for_output_selection();
        engine.config.stop_timeout = Duration::from_millis(10);
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let id = SampleId(512);
        let decoded = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
        library.requeue_ready(id, decoded.clone());
        let preference = engine.preferred_output.clone();
        let mut attempts = 0;
        let error = engine
            .set_output_device_with("replacement", |device, _, samples| {
                attempts += 1;
                assert_eq!(samples, &[(id, decoded.clone())]);
                device.fail_output_replacement_for_test()
            })
            .expect_err("both outputs failed");
        assert!(error.to_string().contains("output preparation failed"));
        assert!(
            error
                .to_string()
                .contains("could not reopen previous output")
        );
        assert_eq!(attempts, 2);
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert_eq!(engine.preferred_output, preference);
        assert_eq!(engine.retained_samples.get(&id), Some(&decoded));
        assert_eq!(library.take_ready(), vec![(id, decoded)]);
        for _ in 0..3 {
            assert!(matches!(engine.tick(|_| Ok(())).unwrap(), StudioTick::Idle));
        }
    }

    #[test]
    fn stop_during_output_change_prevents_recovery_prefill() {
        for stop_on_attempt in [1, 2] {
            let mut engine = silent_engine_for_output_selection();
            engine.config.stop_timeout = Duration::from_millis(10);
            let library = Arc::clone(engine.session.sample_library().unwrap());
            let id = SampleId(512);
            let decoded = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
            library.requeue_ready(id, decoded.clone());
            let stop = engine.stop_handle();
            let generation = engine.generation();
            let mut attempts = 0;
            let error = engine
                .set_output_device_with("replacement", |device, selector, samples| {
                    attempts += 1;
                    let result = if attempts == 1 {
                        device.fail_output_replacement_for_test()
                    } else {
                        device.recycle_output_to_with_samples(selector, samples, || false)
                    };
                    if attempts == stop_on_attempt {
                        stop.request_stop();
                    }
                    result
                })
                .expect_err("Stop cancels recovery");
            assert!(error.to_string().contains("output preparation failed"));
            assert!(error.to_string().contains("cancelled"));
            assert_eq!(attempts, stop_on_attempt);
            assert_eq!(
                engine.generation(),
                generation,
                "no recovery requery after Stop"
            );
            assert!(!engine.is_playing());
            assert!(stop.is_stopped());
            assert_eq!(engine.retained_samples.get(&id), Some(&decoded));
            assert_eq!(library.take_ready(), vec![(id, decoded)]);
        }
    }

    #[test]
    fn output_selection_during_stop_does_not_reopen_the_draining_device() {
        let mut engine = silent_engine_for_output_selection();
        let stream = engine.live.as_ref().unwrap().device.stream_id();
        let generation = engine.generation();
        engine.request_stop();
        engine
            .set_output_device_with("next-output", |_, _, _| panic!("reopened after Stop"))
            .expect("record next preference");
        assert!(engine.is_playing(), "existing owner still drains");
        assert!(engine.session.transport().is_stopped());
        assert_eq!(engine.live.as_ref().unwrap().device.stream_id(), stream);
        assert_eq!(engine.generation(), generation);
        assert_eq!(engine.preferred_output.as_deref(), Some("next-output"));
    }

    #[test]
    fn output_recovery_seeds_retained_samples_and_accepts_valid_silence() {
        let mut engine = silent_engine_for_output_selection();
        engine
            .session
            .enable_default_samples()
            .expect("sample library");
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let id = SampleId(512);
        let retiring = SampleId(513);
        let decoded = DecodedSample::from_parts(44_100, 1, vec![0.25; 64]).unwrap();
        engine.retain_sample_for_test(id, decoded.clone());
        engine.retain_sample_for_test(retiring, decoded.clone());
        library.forget_id_for_test(retiring);
        engine.retire_sample(retiring, u64::MAX);
        engine.release_retired_samples();
        assert_eq!(engine.retiring_count(), 1, "old horizon still owns the id");
        assert_eq!(library.free_id_count(), 0);
        let generation = engine.generation();
        engine.live.as_mut().unwrap().progress_clock = u64::MAX;
        let mut attempts = 0;
        engine
            .set_output_device_with("replacement", |device, selector, samples| {
                attempts += 1;
                assert_eq!(samples, &[(id, decoded.clone())]);
                if attempts == 1 {
                    device.fail_output_replacement_for_test()
                } else {
                    device.recycle_output_to_with_samples(selector, samples, || false)
                }
            })
            .expect_err("requested output refused");
        assert_eq!(attempts, 2);
        assert!(
            library.take_ready().is_empty(),
            "successful fallback already seeded the bank"
        );
        assert_eq!(engine.retained_samples.get(&id), Some(&decoded));
        engine.retry_pending_uninstalls();
        assert_eq!(engine.retiring_count(), 0);
        assert_eq!(library.free_id_count(), 1, "recovery released the old id");
        assert!(!engine.retained_samples.contains_key(&retiring));
        let live = engine.live.as_ref().unwrap();
        assert_eq!(live.device.report().asset_queues.sample_installs, 0);
        assert_eq!(library.render_rate_for_test(), live.device.sample_rate());
        assert_ne!(live.progress_clock, u64::MAX);
        assert_eq!(live.last_recycle_at, Some(live.progress_at));
        assert_eq!(
            live.device.generation(),
            generation,
            "refill not yet published"
        );
        assert!(matches!(
            engine.tick(|_| Ok(())).unwrap(),
            StudioTick::Running { step: Some(_), .. }
        ));
        assert_eq!(engine.generation(), generation + 1);
        assert_eq!(
            engine.live.as_ref().unwrap().device.generation(),
            generation + 1
        );
        assert!(!engine.session.transport().is_stopped());
    }

    /// Wait until the silent device has completed at least `count`
    /// callbacks past `from`. The silent callback runs on its own clock, a
    /// couple of milliseconds a block.
    fn settle_callbacks(device: &LiveScalarDevice, from: u64, count: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while device.callbacks() < from + count {
            assert!(
                Instant::now() < deadline,
                "the silent device stopped running callbacks"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// An evicted id goes back to the library only once its baton has
    /// landed and two more callbacks have completed - one whole block ran
    /// with the slot empty, so every voice on it retired.
    #[test]
    fn a_retiring_id_waits_for_two_callbacks_after_its_baton() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let id = SampleId(600);
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .install_sample(id, pcm())
                .is_ok(),
            "bank install"
        );
        engine.retain_sample_for_test(id, pcm());
        library.forget_id_for_test(id);

        engine.retire_sample(id, 0);
        assert_eq!(engine.retiring_count(), 1);
        assert_eq!(library.free_id_count(), 0, "not yet: the baton has to land");
        let pushed = engine
            .retiring
            .iter()
            .find_map(|retiring| match retiring.baton {
                Baton::Pushed { callbacks } => Some(callbacks),
                _ => None,
            })
            .expect("the baton was pushed");

        let device = &engine.live.as_ref().unwrap().device;
        settle_callbacks(device, pushed, BATON_SETTLE_CALLBACKS);
        engine.release_retired_samples();
        assert_eq!(engine.retiring_count(), 0, "settled and quiet: released");
        assert_eq!(library.free_id_count(), 1, "the id is the library's again");
    }

    /// A baton the ring refused is owed, and an owed id is never released:
    /// the bank still holds its PCM. The retry lands it, and only then does
    /// the settle start counting.
    #[test]
    fn an_owed_baton_is_never_released_until_it_lands() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let id = SampleId(601);
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .install_sample(id, pcm())
                .is_ok(),
            "bank install"
        );
        library.forget_id_for_test(id);

        // The ring drains between blocks, so "full" is a moment: refill and
        // re-retire until the refusal actually lands.
        let mut owed = false;
        for _ in 0..8 {
            engine.retain_sample_for_test(id, pcm());
            fill_install_ring_for_test(&engine.live.as_ref().unwrap().device);
            engine.retire_sample(id, 0);
            if engine.owes_uninstall(id) {
                owed = true;
                break;
            }
            engine.retiring.retain(|retiring| retiring.id != id);
        }
        assert!(owed, "the install ring never refused the baton");

        // However long we wait, an owed baton keeps the id out of the free
        // list.
        std::thread::sleep(Duration::from_millis(20));
        engine.release_retired_samples();
        assert!(engine.owes_uninstall(id));
        assert_eq!(library.free_id_count(), 0);

        // The retry lands it once the ring has room, and the settle follows.
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.owes_uninstall(id) {
            assert!(Instant::now() < deadline, "the retry never landed");
            engine.retry_pending_uninstalls();
            std::thread::sleep(Duration::from_millis(2));
        }
        let pushed = engine
            .retiring
            .iter()
            .find_map(|retiring| match retiring.baton {
                Baton::Pushed { callbacks } => Some(callbacks),
                _ => None,
            })
            .expect("pushed after the retry");
        settle_callbacks(
            &engine.live.as_ref().unwrap().device,
            pushed,
            BATON_SETTLE_CALLBACKS,
        );
        engine.release_retired_samples();
        assert_eq!(library.free_id_count(), 1);
    }

    /// A stop takes the device, its bank, its ring and its voices with it:
    /// everything retiring is free at once, and everything still retained
    /// is not - it is re-published on the next evaluate.
    #[test]
    fn stop_releases_every_retiring_id_and_keeps_retained_ones() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        let retiring = SampleId(602);
        let kept = SampleId(603);
        for id in [retiring, kept] {
            assert!(
                engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .install_sample(id, pcm())
                    .is_ok(),
                "bank install"
            );
            engine.retain_sample_for_test(id, pcm());
        }
        library.forget_id_for_test(retiring);
        engine.retire_sample(retiring, u64::MAX);
        engine.release_retired_samples();
        assert_eq!(
            library.free_id_count(),
            0,
            "held by the horizon while playing"
        );

        assert!(engine.stop(Duration::from_millis(500)).is_some());
        assert_eq!(engine.retiring_count(), 0);
        assert_eq!(library.free_id_count(), 1, "the retiring id is free");
        assert_eq!(
            engine.retained_sample_count(),
            1,
            "the retained one is kept"
        );
    }

    /// With no device there is nothing that can read an id: an eviction
    /// while stopped releases at once.
    #[test]
    fn an_idle_engine_releases_a_retiring_id_at_once() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("engine");
        assert!(
            engine.live.is_none(),
            "nothing plays until the first launch"
        );
        let library = engine.sample_library().expect("library");
        let id = SampleId(604);
        engine.retain_sample_for_test(
            id,
            DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm"),
        );
        library.forget_id_for_test(id);
        engine.retire_sample(id, 0);
        assert_eq!(engine.retiring_count(), 0);
        assert_eq!(library.free_id_count(), 1);
    }

    /// A preview run reaches seconds ahead, and the callback holds an event
    /// that far out in front of everything pushed after it: nothing retires
    /// until the frontier is past it.
    #[test]
    fn a_retiring_id_waits_for_the_audition_horizon() {
        let mut engine = silent_engine_for_output_selection();
        let library = engine.sample_library().expect("library");
        let id = SampleId(605);
        let pcm = || DecodedSample::from_parts(48_000, 1, vec![0.0; 128]).expect("pcm");
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .install_sample(id, pcm())
                .is_ok(),
            "bank install"
        );
        engine.retain_sample_for_test(id, pcm());
        library.forget_id_for_test(id);
        // A preview far ahead of the device's clock.
        let far = engine.live.as_ref().unwrap().device.clock_frames() + 48_000 * 60;
        engine.audition_horizon_frame = far;

        engine.retire_sample(id, 0);
        let pushed = engine
            .retiring
            .iter()
            .find_map(|retiring| match retiring.baton {
                Baton::Pushed { callbacks } => Some(callbacks),
                _ => None,
            })
            .expect("pushed");
        settle_callbacks(
            &engine.live.as_ref().unwrap().device,
            pushed,
            BATON_SETTLE_CALLBACKS,
        );
        engine.release_retired_samples();
        assert_eq!(
            library.free_id_count(),
            0,
            "settled, but a preview still names the slot a minute ahead"
        );
        engine.audition_horizon_frame = 0;
        engine.release_retired_samples();
        assert_eq!(library.free_id_count(), 1);
    }

    /// A pad coming or going reaches the studio as a diagnostic: the log
    /// and the status line say so, the way they do for an audio input.
    #[test]
    fn a_pad_arriving_or_going_is_a_diagnostic() {
        let mut engine = silent_engine_for_output_selection();
        let _ = rustel_core::gamepad::take_notices();
        engine.pending_diagnostics.clear();
        rustel_core::gamepad::notice(rustel_core::gamepad::Notice::Connected {
            pad: 1,
            name: "Test Pad".to_owned(),
        });
        rustel_core::gamepad::notice(rustel_core::gamepad::Notice::Disconnected {
            pad: 1,
            name: "Test Pad".to_owned(),
        });
        engine.collect_gamepad_notices();
        let lines: Vec<(String, StudioDiagnosticLevel)> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad")
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.level))
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    "gamepad(1) connected: Test Pad".to_owned(),
                    StudioDiagnosticLevel::Info
                ),
                (
                    "gamepad(1) disconnected: Test Pad".to_owned(),
                    StudioDiagnosticLevel::Info
                ),
            ]
        );
        assert!(
            rustel_core::gamepad::take_notices().is_empty(),
            "taken, not left for the next turn"
        );
    }

    /// Pad slot 0's state, reset to disconnected and at rest: the gamepad
    /// activity tests below share it with the rest of the binary and must
    /// hand it back clean, panic or not.
    #[cfg(feature = "gamepad")]
    fn reset_test_pad_0() -> &'static rustel_core::gamepad::Pad {
        let pad = rustel_core::gamepad::pad(0).expect("pad 0");
        pad.set_connected(false);
        pad
    }

    /// A button crossing half is discrete news, the way a MIDI key landing
    /// is: it reaches the log at a level a reader sees, in the CLI
    /// monitor's own wording, and lands in the mixer's feed beside MIDI's.
    #[cfg(feature = "gamepad")]
    #[test]
    fn a_pads_button_reaches_the_log_the_way_a_midi_note_does() {
        let _guard = GAMEPAD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pad = reset_test_pad_0();
        rustel_core::gamepad::set_name(0, Some("Test Pad Buttons".to_owned()));
        pad.set_connected(true);
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();

        pad.set_button(0, 1.0);
        engine.collect_gamepad_notices();
        let pressed: Vec<(String, StudioDiagnosticLevel)> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad-activity")
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.level))
            .collect();
        assert_eq!(
            pressed,
            vec![(
                "gamepad(0) Test Pad Buttons: a pressed".to_owned(),
                StudioDiagnosticLevel::Note
            )]
        );
        assert!(
            rustel_core::gamepad::recent_activity()
                .contains(&"gamepad(0) Test Pad Buttons: a pressed".to_owned()),
            "the mixer's own feed sees the press too"
        );

        engine.pending_diagnostics.clear();
        pad.set_button(0, 0.0);
        engine.collect_gamepad_notices();
        let released: Vec<(String, StudioDiagnosticLevel)> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad-activity")
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.level))
            .collect();
        assert_eq!(
            released,
            vec![(
                "gamepad(0) Test Pad Buttons: a released".to_owned(),
                StudioDiagnosticLevel::Note
            )]
        );
        pad.set_connected(false);
    }

    /// `GAMEPAD_AXIS_LOG_THRESHOLD` coalesces a full stick sweep to a few
    /// lines in `studio.log`. Each line is at `Trace`, which the log panel
    /// does not show by default.
    #[cfg(feature = "gamepad")]
    #[test]
    fn a_sticks_continuous_move_does_not_flood_the_log() {
        let _guard = GAMEPAD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pad = reset_test_pad_0();
        rustel_core::gamepad::set_name(0, Some("Test Pad Stick".to_owned()));
        pad.set_connected(true);
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();

        // A slow, steady sweep across the whole throw: the shape a musician
        // leaning on a stick actually sends, sampled once a turn.
        const SAMPLES: usize = 200;
        let mut seen = Vec::new();
        for step in 0..=SAMPLES {
            let value = -1.0 + 2.0 * (step as f32) / (SAMPLES as f32);
            pad.set_axis(0, value);
            engine.collect_gamepad_notices();
            emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |update| {
                if let StudioUpdate::Diagnostic(diagnostic) = update {
                    seen.push(diagnostic);
                }
                Ok(())
            });
        }
        let axis_lines: Vec<&StudioDiagnostic> = seen
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad-activity")
            .collect();
        assert!(
            !axis_lines.is_empty(),
            "the sweep crossed the threshold at least once"
        );
        assert!(
            axis_lines.len() < SAMPLES / 4,
            "a full sweep coalesced to a handful of lines, not one a sample: {}",
            axis_lines.len()
        );
        assert!(
            axis_lines
                .iter()
                .all(|diagnostic| diagnostic.level == StudioDiagnosticLevel::Trace),
            "a stick's motion never reaches a level the log panel shows by default: {axis_lines:?}"
        );
        pad.set_connected(false);
    }

    /// A pad that is connected but nobody is touching must cost the log
    /// nothing: the studio watches every pad from launch, and a controller
    /// left switched on all evening must not write to `studio.log` for
    /// doing nothing.
    #[cfg(feature = "gamepad")]
    #[test]
    fn an_idle_connected_pad_writes_no_activity() {
        let _guard = GAMEPAD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pad = reset_test_pad_0();
        rustel_core::gamepad::set_name(0, Some("Test Pad Idle".to_owned()));
        pad.set_connected(true);
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();

        for _ in 0..20 {
            engine.collect_gamepad_notices();
        }
        assert!(
            engine
                .pending_diagnostics
                .iter()
                .all(|diagnostic| diagnostic.kind != "gamepad-activity"),
            "a pad at rest wrote nothing: {:?}",
            engine.pending_diagnostics
        );
        pad.set_connected(false);
    }

    /// Push filler installs until the device's install ring refuses one.
    fn fill_install_ring_for_test(device: &LiveScalarDevice) {
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

    /// The output-latency row recycles the output by its current name so
    /// the new size is heard now. That is not choosing the device: a set
    /// following the host default must still follow it afterwards, or the
    /// next headphone plug-in goes unheard.
    #[test]
    fn changing_the_output_buffer_keeps_following_the_host_default() {
        let mut engine = silent_engine_for_output_selection();
        engine.preferred_output = None;
        engine
            .set_output_buffer_frames(Some(256))
            .expect("a playing set recycles its output");
        assert_eq!(engine.config.output_buffer_frames, Some(256));
        assert_eq!(
            engine.preferred_output, None,
            "a buffer change is not a device choice"
        );
        assert!(engine.live.is_some(), "the set is still playing");
    }

    /// A live latency change opens a new stream, and what the host granted
    /// is logged the way the first open logs it.
    #[test]
    fn an_output_latency_change_logs_the_facts_of_the_stream_it_opened() {
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();
        engine
            .set_output_buffer_frames(Some(512))
            .expect("a playing set recycles its output");
        let facts = engine
            .pending_diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == "audio")
            .expect("the recycled stream is described");
        assert_eq!(facts.level, StudioDiagnosticLevel::Note);
        assert!(facts.message.contains("512"), "{}", facts.message);
    }

    #[test]
    fn output_selection_failure_preserves_preference_and_live_state() {
        let mut engine = silent_engine_for_output_selection();
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let id = SampleId(512);
        let replaced = SampleId(513);
        let decoded = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
        let newer = DecodedSample::from_parts(48_000, 1, vec![0.5]).unwrap();
        library.requeue_ready(id, decoded.clone());
        library.requeue_ready(replaced, decoded.clone());
        engine
            .session
            .evaluate_mini("~")
            .expect("replayable mini source");
        engine
            .session
            .schedule_at(0.0)
            .expect("fill silent horizon");
        let generation = engine.generation();
        let horizon = engine.session.scheduled_to_cycle();
        let source = engine.session.active_source().map(str::to_owned);
        assert_eq!(source.as_deref(), Some("~"));
        let preference = engine.preferred_output.clone();
        let live = engine.live.as_mut().unwrap();
        live.device.set_generation(generation, 0, TakeoverCut::None);
        live.progress_clock = 123;
        let stream = live.device.stream_id();
        let output = live.device.audio_facts();
        let mut calls = 0;

        let error = engine
            .set_output_device_with("refused-output", |device, selector, samples| {
                calls += 1;
                assert_eq!(selector, Some("refused-output"));
                assert_eq!(device.stream_id(), stream);
                assert_eq!(
                    samples,
                    &[(id, decoded.clone()), (replaced, decoded.clone())]
                );
                library.requeue_ready(replaced, newer.clone());
                Err(DevicePlaybackError::Unavailable("discovery refused".into()))
            })
            .expect_err("refused recycle");

        assert!(matches!(error, RuntimeError::Audio(message) if message == "discovery refused"));
        assert_eq!(calls, 1);
        assert!(engine.is_playing());
        assert!(!engine.session.transport().is_stopped());
        assert_eq!(engine.generation(), generation);
        assert_eq!(engine.session.scheduled_to_cycle(), horizon);
        assert_eq!(engine.session.active_source(), source.as_deref());
        let live = engine.live.as_ref().unwrap();
        assert_eq!(live.device.stream_id(), stream);
        assert_eq!(live.device.audio_facts(), output);
        assert_eq!(live.device.generation(), generation);
        assert_eq!(live.progress_clock, 123);
        assert!(live.draining.is_none());
        assert_eq!(engine.preferred_output, preference);
        assert_eq!(library.take_ready(), vec![(id, decoded), (replaced, newer)]);
    }

    #[test]
    fn stopped_output_selection_records_preference_without_recycling() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        engine.session.transport().stop();
        let generation = engine.generation();
        engine
            .set_output_device_with("next-output", |_, _, _| {
                panic!("stopped selection recycled")
            })
            .expect("record preference");
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert_eq!(engine.generation(), generation);
        assert_eq!(engine.preferred_output.as_deref(), Some("next-output"));
        assert_eq!(engine.output_name().as_deref(), Some("next-output"));
    }

    #[test]
    fn successful_output_selection_commits_preference_and_requeries_once() {
        let mut engine = silent_engine_for_output_selection();
        engine.preferred_output = None;
        let generation = engine.generation();
        let mut calls = 0;
        engine
            .set_output_device_with(
                rustel_audio::SILENT_OUTPUT_NAME,
                |device, selector, samples| {
                    calls += 1;
                    assert_eq!(selector, Some(rustel_audio::SILENT_OUTPUT_NAME));
                    device.recycle_output_to_with_samples(selector, samples, || false)
                },
            )
            .expect("recycle silent output");
        assert_eq!(calls, 1);
        assert!(engine.is_playing());
        assert!(!engine.session.transport().is_stopped());
        assert_eq!(engine.generation(), generation + 1);
        assert_eq!(
            engine.preferred_output.as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        assert_eq!(
            engine.output_name().as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        // Recovery has re-armed scheduling, not published a consumer result.
        assert_eq!(
            engine.live.as_ref().unwrap().device.generation(),
            generation
        );
    }

    #[test]
    fn recycled_output_updates_decode_rate_without_rewriting_ready_samples() {
        let mut session =
            Session::with_config(rustel_runtime::SessionConfig::default().with_sample_rate(44_100))
                .expect("session");
        session.enable_default_samples().expect("sample library");
        session
            .set_pattern(rustel_core::silence())
            .expect("silent pattern");
        let library = Arc::clone(session.sample_library().unwrap());
        let decoded = DecodedSample::from_parts(44_100, 1, vec![0.25, 0.5]).unwrap();
        library.requeue_ready(SampleId(8), decoded.clone());
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).unwrap();

        // Both manual switching and stalled-output recovery use this seam.
        // Rebinding the same rate must also leave already decoded PCM alone.
        for rate in [48_000, 96_000, 96_000, 44_100] {
            let generation = session.generation();
            arm_output_recovery_after_recycle(&mut session, &mut producer, 0.25, rate)
                .expect("arm recovery");
            assert_eq!(library.render_rate_for_test(), rate);
            assert_eq!(session.config().sample_rate, 44_100);
            assert_eq!(session.generation(), generation + 1);
        }
        // An invalid requery clock must not restore the discarded output's
        // rate: the replacement stream has already opened successfully.
        let generation = session.generation();
        assert!(
            arm_output_recovery_after_recycle(&mut session, &mut producer, f64::NAN, 88_200)
                .is_err()
        );
        assert_eq!(library.render_rate_for_test(), 88_200);
        assert_eq!(session.generation(), generation);
        assert_eq!(library.take_ready(), vec![(SampleId(8), decoded)]);
    }

    #[test]
    fn device_recycle_requeries_and_refills_the_drained_horizon() {
        let source = "note('c4').fast(16)";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("score");
        session.set_schedule_lead(0.1);
        session.set_continuity_margin(0.1);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let before = session.generation();
        let recovery_now = 0.25;
        arm_output_recovery_after_recycle(&mut session, &mut producer, recovery_now, 44_100)
            .expect("arm recovery");
        let after = session.generation();
        assert_eq!(after, before + 1);

        let sample_rate = 44_100;
        let expected_takeover = ((recovery_now + 0.1) * f64::from(sample_rate)).round() as u64;
        let mut publication = None;
        let mut recovered = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || recovery_now,
                sample_rate,
                |generation, frame, _cut| publication = Some((generation, frame)),
                |event| {
                    recovered.push(event);
                    true
                },
            )
            .expect("recovery prefill");

        assert_eq!(publication, Some((after, expected_takeover)));
        assert!(!recovered.is_empty(), "drained horizon was not refilled");
        assert!(
            recovered.iter().all(|event| {
                event.generation == after && event.target_frame >= expected_takeover
            })
        );
    }

    /// The whole point of a studio prebake: a score calls what the setup
    /// defined, on the same heap, having never been able to before.
    ///
    /// Driven through `StudioEngine` rather than `Session` because the flag
    /// it hands the evaluator is the thing at issue: the transport's
    /// stopped flag is TRUE on an idle studio, and passing that would
    /// cancel every setup a studio ever runs at rest.
    #[test]
    fn a_setup_makes_its_helper_callable_by_the_next_score() {
        let never = std::sync::atomic::AtomicBool::new(false);
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".to_owned()),
            ..StudioConfig::default()
        })
        .expect("engine");

        // After a stop the transport's stopped flag is true. Passing that
        // flag to the evaluator would cancel every setup that runs on a
        // stopped studio.
        engine.session.transport().stop();
        assert!(engine.session.transport().is_stopped());
        engine
            .evaluate_prebake_guarded("globalThis.riff = () => note(\"c4 e4\")", &never)
            .expect("setup on a stopped studio");
        assert!(
            engine.session.transport().is_stopped(),
            "setup started the transport"
        );

        let generation = engine.session.generation();
        engine.session.transport().start();
        engine.session.evaluate("riff()").expect("score using it");
        assert_ne!(engine.session.generation(), generation);
        let haps = engine
            .session
            .query(
                rustel_fraction::Fraction::ZERO,
                rustel_fraction::Fraction::ONE,
            )
            .expect("query");
        assert!(!haps.is_empty());
        assert!(
            haps.iter()
                .all(|hap| hap.value.show().contains("c4") || hap.value.show().contains("e4")),
            "the score did not get the setup's notes: {haps:?}"
        );

        // A setup that throws leaves the score that is loaded alone.
        let error = engine
            .evaluate_prebake_guarded("throw new Error('bad setup')", &never)
            .expect_err("a throwing setup");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(engine.session.active_source(), Some("riff()"));

        // And shutdown really does reach a runaway one.
        let closing = std::sync::atomic::AtomicBool::new(true);
        let error = engine
            .evaluate_prebake_guarded("while (true) {}", &closing)
            .expect_err("a setup nobody is waiting for");
        assert_eq!(error.kind(), "cancelled");
    }

    #[test]
    fn rejected_live_evaluation_keeps_the_audible_source_and_generation() {
        let old = "note(\"c4\")";
        let mut session = Session::new().expect("session");
        session.evaluate(old).expect("old score");
        let generation = session.generation();
        let transport = session.transport();

        let error = session
            .reload_at_cancellable("note(", false, 0.1, transport.stopped_flag())
            .expect_err("invalid replacement");
        assert!(!matches!(error, RuntimeError::Cancelled));
        assert_eq!(session.generation(), generation);
        assert_eq!(session.active_source(), Some(old));
    }

    /// `.log()` writes one line for each note that sounds. Each line
    /// arrives at `Info`, the default floor of the log panel, and no
    /// surface paints it as a failure.
    #[test]
    fn log_lines_arrive_as_info_one_per_sounding_note() {
        let mut engine = silent_engine_for_output_selection();
        let mut logs = Vec::new();
        engine
            .evaluate("$: note(\"c\").s(\"sine\").fast(4).log()", false)
            .expect("logged score");
        // The notes sound on the device's own clock, so these lines arrive
        // as wall time passes rather than as turns are taken. A fixed
        // number of turns counts whatever happened to have sounded by the
        // last one, which on a busy machine is sometimes nothing at all.
        // Wait for the two lines the assertions below are about instead,
        // and give up only at a deadline no healthy run comes near.
        let deadline = Instant::now() + Duration::from_secs(10);
        let sounded = |logs: &[(String, StudioDiagnosticLevel, String)]| {
            logs.iter().filter(|(kind, _, _)| kind == "log").count()
        };
        while sounded(&logs) < 2 {
            engine
                .tick_at(Duration::from_millis(12), |update| {
                    if let StudioUpdate::Diagnostic(diagnostic) = update {
                        logs.push((
                            diagnostic.kind.clone(),
                            diagnostic.level,
                            diagnostic.message,
                        ));
                    }
                    Ok(())
                })
                .expect("tick");
            assert!(
                Instant::now() < deadline,
                "the notes never sounded: {logs:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let hap_lines = logs
            .iter()
            .filter(|(kind, _, _)| kind == "log")
            .collect::<Vec<_>>();
        assert!(hap_lines.len() >= 2, "the notes sounded: {:?}", logs);
        assert!(
            hap_lines
                .iter()
                .all(|(_, level, _)| *level == StudioDiagnosticLevel::Info)
        );
        assert!(
            hap_lines
                .iter()
                .all(|(_, _, message)| message.starts_with("[hap] "))
        );
    }

    #[test]
    fn a_syntax_error_does_not_count_as_engine_pressure_refusal() {
        let mut engine = silent_engine_for_output_selection();
        engine
            .evaluate("note(\"c4\")", false)
            .expect("audible score");
        engine.tick_at(Duration::ZERO, accepted).expect("prefill");
        let _ = engine.snapshot();

        let error = engine.evaluate("note(", false).expect_err("invalid syntax");
        assert_eq!(error.kind(), "evaluation");

        engine
            .tick_at(Duration::from_millis(2), accepted)
            .expect("continuation");
        let pressure = engine.snapshot().pressure.expect("still playing");
        assert_eq!(
            pressure.producer_refusals, 0,
            "a typo is not the producer refusing work"
        );
        assert_ne!(pressure.cause, rustel_runtime::EnginePressureCause::Refusal);
    }

    #[test]
    fn replacement_cutover_precedes_the_first_new_generation_push() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("note(\"c4\").fast(4)")
            .expect("initial score");
        session.set_schedule_lead(0.05);
        session.set_continuity_margin(0.05);
        let mut producer =
            LiveFileProducer::unwatched(Duration::from_millis(2)).expect("unwatched producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let before = session.generation();
        let transport = session.transport();
        let after = session
            .reload_at_cancellable("note(\"e4\").fast(4)", false, 0.1, transport.stopped_flag())
            .expect("replacement");
        producer.arm_replacement(before, after);

        let actions = std::cell::RefCell::new(Vec::<(&'static str, u64)>::new());
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.1,
                48_000,
                |generation, _, _| actions.borrow_mut().push(("cutover", generation)),
                |event| {
                    actions.borrow_mut().push(("push", event.generation));
                    true
                },
            )
            .expect("replacement prefill");
        let actions = actions.into_inner();
        let cutover = actions
            .iter()
            .position(|action| *action == ("cutover", after))
            .expect("device cutover");
        let first_new_push = actions
            .iter()
            .position(|action| *action == ("push", after))
            .expect("new generation push");
        assert!(cutover < first_new_push, "actions: {actions:?}");
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

    /// A line cut the render frontier has reached retires what the shield holds
    /// from its line; one still ahead retires nothing.
    #[cfg(feature = "osc")]
    #[test]
    fn a_fired_line_cut_retires_what_the_shield_holds_from_it() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        let now = device_of(&engine).clock_seconds();
        let rate = device_of(&engine).sample_rate();
        engine
            .shield_hold
            .hold_osc_for_test(&[now - 0.2, now - 0.05, now + 1.0]);
        assert!(!engine.retire_past_fired_line(frame_at(now + 5.0, rate)));
        assert_eq!(
            engine.shield_hold.held_targets_for_test(),
            [now - 0.2, now - 0.05, now + 1.0]
        );
        assert!(engine.retire_past_fired_line(frame_at(now - 0.1, rate)));
        assert_eq!(engine.shield_hold.held_targets_for_test(), [now - 0.2]);
    }

    /// An output recycle discards the audio horizon, and what the shield held
    /// for it goes too.
    #[cfg(feature = "osc")]
    #[test]
    fn an_output_recycle_forgets_what_the_shield_held() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let now = device_of(&engine).clock_seconds();
        engine
            .shield_hold
            .hold_osc_for_test(&[now + 1.0, now + 2.0]);
        engine
            .set_output_device_with(
                rustel_audio::SILENT_OUTPUT_NAME,
                |device, selector, samples| {
                    device.recycle_output_to_with_samples(selector, samples, || false)
                },
            )
            .expect("the output recycles");
        assert!(engine.shield_hold.held_targets_for_test().is_empty());
    }

    /// Withdrawing a line that a published flip already cleared retires nothing:
    /// the audio from that line is the launched score's. An arm still set when
    /// it is withdrawn has fired, and takes what the shield holds past it.
    #[cfg(feature = "osc")]
    #[test]
    fn withdrawing_a_line_retires_only_an_arm_still_set() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        let now = device_of(&engine).clock_seconds();
        let line = frame_at(now - 0.1, device_of(&engine).sample_rate());
        engine.shield_hold.hold_osc_for_test(&[now + 1.0]);
        let device = device_of(&engine);
        device.arm_line_cut(line, true);
        device.set_generation(device.generation() + 1, line, TakeoverCut::AtTakeover);
        engine.withdraw_line_cut(false);
        assert_eq!(engine.shield_hold.held_targets_for_test(), [now + 1.0]);

        device_of(&engine).arm_line_cut(line, true);
        engine.withdraw_line_cut(false);
        assert!(engine.shield_hold.held_targets_for_test().is_empty());
    }

    /// A flip retires the outgoing score's external output from its takeover,
    /// or from the line of an earlier cut that has fired.
    #[test]
    fn a_flip_retires_from_a_fired_line_before_its_takeover() {
        let engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        let device = device_of(&engine);
        let rate = device.sample_rate();
        let now = device.clock_seconds();
        let takeover = frame_at(now + 1.0, rate);
        assert_eq!(retire_frame(device, takeover), takeover);
        device.arm_line_cut(frame_at(now + 5.0, rate), true);
        assert_eq!(retire_frame(device, takeover), takeover);
        device.arm_line_cut(frame_at(now - 0.1, rate), true);
        assert_eq!(retire_frame(device, takeover), frame_at(now - 0.1, rate));
    }

    /// Before a drain releases what the shield holds, a fired line takes what
    /// lies past it, and a line still ahead keeps what lies past it waiting.
    #[cfg(feature = "osc")]
    #[test]
    fn the_shield_releases_nothing_past_an_armed_line() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.5);
        hold_clock(&engine, true);
        let now = device_of(&engine).clock_seconds();
        let rate = device_of(&engine).sample_rate();
        engine
            .shield_hold
            .hold_osc_for_test(&[now + 0.1, now + 0.3]);
        device_of(&engine).arm_line_cut(frame_at(now + 0.2, rate), true);
        engine.release_shield_hold(now);
        assert_eq!(engine.shield_hold.held_targets_for_test(), [now + 0.3]);

        engine.shield_hold.hold_osc_for_test(&[now + 5.0]);
        device_of(&engine).arm_line_cut(frame_at(now - 0.1, rate), true);
        engine.release_shield_hold(now);
        assert!(engine.shield_hold.held_targets_for_test().is_empty());
        hold_clock(&engine, false);
    }

    /// The consumer fires a line cut in the first block that ends past its
    /// line, so the render frontier has passed a line only once it is beyond it.
    #[test]
    fn a_line_is_passed_once_the_frontier_is_beyond_it() {
        let engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.1);
        hold_clock(&engine, true);
        let device = device_of(&engine);
        let frontier = device.render_frontier_frames();
        assert!(!line_passed(device, frontier));
        assert!(line_passed(device, frontier - 1));
        hold_clock(&engine, false);
    }
}

#[cfg(test)]
mod load_mode_tests {
    //! Tests for the load mode: a start or an edit meeting sounds that are
    //! still loading, the loading cue that follows them, and the one line a
    //! load's late sounds are said in.

    use super::super::settings::LoadMode;
    use super::*;
    use rustel_runtime::samples::{SampleFailure, SampleLibrary};

    /// A stopped studio in `mode` whose sounds come from `library`.
    fn studio(mode: LoadMode, library: &Arc<SampleLibrary>) -> StudioEngine {
        studio_with(mode, library, SessionConfig::default())
    }

    /// [`studio`] over its own session settings.
    fn studio_with(
        mode: LoadMode,
        library: &Arc<SampleLibrary>,
        session: SessionConfig,
    ) -> StudioEngine {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            session,
            ..StudioConfig::default()
        })
        .expect("studio");
        engine
            .session
            .set_sample_library_for_test(Arc::clone(library));
        engine.master.set_load_mode(mode);
        engine
    }

    /// Start `source` from stopped on the silent output, as a play does once
    /// the output is open.
    fn start(engine: &mut StudioEngine, source: &str) -> StudioInstall {
        engine.session.transport().start();
        let device =
            LiveScalarDevice::start_silent(48_000, engine.generation()).expect("silent output");
        engine
            .session
            .bind_audio_confirmations(device.confirmations())
            .expect("bind the silent output");
        engine
            .start_on(device, source, false, false)
            .expect("the start")
    }

    /// One producer turn, answering what it sent.
    fn turn(engine: &mut StudioEngine) -> Vec<StudioUpdate> {
        let mut sent = Vec::new();
        engine
            .tick(|update| {
                sent.push(update);
                Ok(())
            })
            .expect("a turn");
        sent
    }

    /// Turn for `span` of wall time, answering what the turns sent.
    fn turns_for(engine: &mut StudioEngine, span: Duration) -> Vec<StudioUpdate> {
        let until = Instant::now() + span;
        let mut sent = Vec::new();
        while Instant::now() < until {
            sent.extend(turn(engine));
            std::thread::sleep(Duration::from_millis(2));
        }
        sent
    }

    /// Turn until `done`, answering what the turns sent.
    fn turn_until(
        engine: &mut StudioEngine,
        what: &str,
        done: impl Fn(&mut StudioEngine) -> bool,
    ) -> Vec<StudioUpdate> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut sent = Vec::new();
        while !done(engine) {
            assert!(Instant::now() < deadline, "{what} never happened");
            sent.extend(turn(engine));
            std::thread::sleep(Duration::from_millis(2));
        }
        sent
    }

    fn device_now(engine: &StudioEngine) -> f64 {
        engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
    }

    fn diagnostics(sent: &[StudioUpdate]) -> Vec<&StudioDiagnostic> {
        sent.iter()
            .filter_map(|update| match update {
                StudioUpdate::Diagnostic(diagnostic) => Some(diagnostic),
                _ => None,
            })
            .collect()
    }

    fn late_lines(sent: &[StudioUpdate]) -> Vec<&StudioDiagnostic> {
        diagnostics(sent)
            .into_iter()
            .filter(|diagnostic| diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC)
            .collect()
    }

    /// A library whose sound `kick` stays loading until the test finishes it.
    fn loading_kick() -> Arc<SampleLibrary> {
        Arc::new(SampleLibrary::with_loading_sample_for_test("kick"))
    }

    /// Start from stopped in wait: nothing sounds while the kick loads, and
    /// cycle zero, the kick's own onset, falls after the kick came in. An
    /// onset that sounds late still carries cycle zero, so the test pins when
    /// cycle zero is, not only that the first onset is on it.
    #[test]
    fn a_wait_start_holds_cycle_zero_until_its_first_sound_has_loaded() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"kick\")");
        turns_for(&mut engine, Duration::from_millis(300));
        assert!(engine.start_is_held());
        assert!(
            engine.played_by_text.is_empty(),
            "nothing sounds while held"
        );
        let cue = engine.snapshot().loading.expect("the cue follows the kick");
        assert!(cue.waiting);
        assert_eq!((cue.settled, cue.total), (0, 1));
        assert_eq!(cue.loading.as_deref(), Some("kick"));

        let kick = library.finish_loading_sample_for_test();
        let arrived = device_now(&engine);
        turn_until(&mut engine, "the kick", |engine| {
            engine.played_by_text.contains(&kick)
        });
        assert!(
            engine.session.cycle_at_time(arrived) < 0.0,
            "cycle zero comes after the kick came in"
        );
        assert!(engine.snapshot().loading.is_none(), "the cue is gone");
    }

    /// Async: the start plays at once, the loading kick is skipped, and
    /// its lateness is said once, in one line, when the load is over.
    #[test]
    fn an_async_start_plays_at_once_and_says_the_late_sound_once() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Async, &library);
        start(&mut engine, "s(\"kick\").fast(4)");
        assert!(!engine.start_is_held());
        let mut sent = turns_for(&mut engine, Duration::from_millis(700));
        let cue = engine.snapshot().loading.expect("the cue follows the kick");
        assert!(!cue.waiting, "async holds nothing");
        assert!(late_lines(&sent).is_empty(), "nothing is said per sound");

        let kick = library.finish_loading_sample_for_test();
        sent.extend(turn_until(&mut engine, "the kick", |engine| {
            engine.played_by_text.contains(&kick)
        }));
        sent.extend(turns_for(&mut engine, Duration::from_millis(50)));
        let late = late_lines(&sent);
        assert_eq!(late.len(), 1, "{late:?}");
        assert!(late[0].message.contains("kick:0"), "{}", late[0].message);
        let Some(DiagnosticAlert::Raise(key)) = &late[0].alert else {
            panic!("the late line carries its alert: {:?}", late[0]);
        };
        assert!(
            diagnostics(&sent)
                .iter()
                .any(|diagnostic| diagnostic.alert == Some(DiagnosticAlert::Resolve(key.clone()))),
            "and resolves it, its sounds being in"
        );
    }

    /// A rewind whose first window waits whole for its sound skips nothing:
    /// the session holds it and it lands from its own cycle zero once the
    /// sound is in, so no late line names that sound.
    #[test]
    fn a_rewind_held_for_its_sound_says_nothing_was_skipped() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Async, &library);
        start(&mut engine, "s(\"sine\")");
        turns_for(&mut engine, Duration::from_millis(100));
        engine.start_next_from_zero();
        let rewind = engine.evaluate("s(\"kick\")", false).expect("the rewind");
        let mut sent = turns_for(&mut engine, Duration::from_millis(300));
        let audible =
            |engine: &StudioEngine| engine.live.as_ref().expect("playing").device.generation();
        assert_ne!(audible(&engine), rewind.generation, "the rewind is held");

        let kick = library.finish_loading_sample_for_test();
        sent.extend(turn_until(&mut engine, "the kick", |engine| {
            engine.played_by_text.contains(&kick)
        }));
        assert_eq!(audible(&engine), rewind.generation, "and lands whole");
        sent.extend(turns_for(&mut engine, Duration::from_millis(50)));
        assert!(late_lines(&sent).is_empty(), "{:?}", late_lines(&sent));
    }

    /// A sound whose load failed has settled: the start goes on.
    #[test]
    fn a_failed_sound_settles_a_held_start() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"kick\")");
        turns_for(&mut engine, Duration::from_millis(100));
        assert!(engine.start_is_held());
        library.fail_loading_sample_for_test();
        turn_until(&mut engine, "the start going on", |engine| {
            engine
                .live
                .as_ref()
                .is_some_and(|live| live.initial_start_generation.is_none())
        });
        assert!(engine.snapshot().loading.is_none());
    }

    /// A map left reading loading with no job behind it holds nothing for
    /// good: the start waits while the loader has manifests in hand, and goes
    /// on, its cue gone, once the loader is idle.
    #[test]
    fn a_map_nobody_is_fetching_lets_a_held_start_go_on() {
        let library = Arc::new(SampleLibrary::empty());
        let mut engine = studio(LoadMode::Wait, &library);
        let stranded = "github:me/stranded";
        library.note_samples_source_state_for_tests(
            stranded,
            rustel_runtime::samples::SourceState::Loading,
        );
        let hold = library.hold_manifest_worker_for_test();
        start(
            &mut engine,
            &format!("samples('{stranded}')\n$: s(\"sine\")"),
        );
        turns_for(&mut engine, Duration::from_millis(50));
        assert!(engine.start_is_held(), "held while a manifest is in hand");
        assert!(engine.snapshot().loading.is_some_and(|cue| cue.waiting));

        drop(hold);
        turn_until(&mut engine, "the start going on", |engine| {
            engine
                .live
                .as_ref()
                .is_some_and(|live| live.initial_start_generation.is_none())
        });
        assert_eq!(
            library.samples_source_state(stranded),
            Some(rustel_runtime::samples::SourceState::Loading),
            "the map still reads loading"
        );
        assert!(engine.snapshot().loading.is_none());
    }

    /// Two maps on a host that never answers, unique to this run: a trusted
    /// map a past run fetched is kept on disk. The host stays silent while the
    /// listener lives.
    fn silent_maps() -> (std::net::TcpListener, String, String) {
        // Connections wait in its backlog and are never answered.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("a silent host");
        let host = silent.local_addr().expect("its address");
        let run = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let first = format!("http://{host}/{run}-first.json");
        let later = format!("http://{host}/{run}-later.json");
        (silent, first, later)
    }

    /// A batch whose first map spends its budget on a host that never answers
    /// holds nothing for good: the map it never reached reads failed too, and
    /// the held start goes on, its cue gone, once the loader is idle.
    #[test]
    fn a_batch_spent_on_its_first_map_lets_a_held_start_go_on() {
        let library = Arc::new(SampleLibrary::empty());
        let mut engine = studio(LoadMode::Wait, &library);
        let (_silent, first, later) = silent_maps();
        let hold = library.hold_manifest_worker_for_test();
        library.register_samples_batch_for_test(&[&first, &later], Duration::from_millis(300));
        start(
            &mut engine,
            &format!("samples('{first}')\nsamples('{later}')\n$: s(\"sine\")"),
        );
        turns_for(&mut engine, Duration::from_millis(50));
        assert!(engine.start_is_held(), "held while the batch is in hand");
        assert!(engine.snapshot().loading.is_some_and(|cue| cue.waiting));

        drop(hold);
        turn_until(&mut engine, "the start going on", |engine| {
            engine
                .live
                .as_ref()
                .is_some_and(|live| live.initial_start_generation.is_none())
        });
        for spec in [&first, &later] {
            let state = library.samples_source_state(spec);
            assert!(
                matches!(state, Some(rustel_runtime::samples::SourceState::Failed(_))),
                "{spec}: {state:?}"
            );
        }
        assert!(engine.snapshot().loading.is_none());
    }

    /// Stop cancels a held start, a held edit and the cue.
    #[test]
    fn stop_cancels_a_held_start_a_held_edit_and_the_cue() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"kick\")");
        turns_for(&mut engine, Duration::from_millis(50));
        engine.request_stop();
        assert!(
            engine.snapshot().loading.is_none(),
            "the cue goes with Stop"
        );
        turn_until(&mut engine, "the stop", |engine| !engine.is_playing());
        library.finish_loading_sample_for_test();
        assert!(
            engine.played_by_text.is_empty(),
            "the held start never sounds"
        );

        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        assert!(engine.hold_update("s(\"kick\")", false, false));
        engine.request_stop();
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Err(RuntimeError::Cancelled))
        ));
        assert!(engine.load.held_edit.is_none());
    }

    /// An edit naming a sound not ready keeps the old score until it is, then
    /// lands whole and is answered.
    #[test]
    fn a_held_edit_keeps_the_old_score_until_its_sounds_load() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        turns_for(&mut engine, Duration::from_millis(100));
        assert!(engine.hold_update("s(\"kick\")", false, false));
        turns_for(&mut engine, Duration::from_millis(200));
        assert_eq!(engine.active_source(), Some("s(\"sine\")"));
        assert!(engine.take_launch_outcome().is_none());
        assert!(engine.snapshot().loading.expect("the cue").waiting);

        library.finish_loading_sample_for_test();
        turn_until(&mut engine, "the edit landing", |engine| {
            engine.active_source() == Some("s(\"kick\")")
        });
        assert!(matches!(engine.take_launch_outcome(), Some(Ok(_))));
    }

    /// Async lands an edit at once, however its sounds stand.
    #[test]
    fn an_async_edit_lands_at_once() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Async, &library);
        start(&mut engine, "s(\"sine\")");
        assert!(!engine.hold_update("s(\"kick\")", false, false));
    }

    /// An edit that needs no new sound is never delayed: a value changed, a
    /// visual added, or a value changed in a score whose own later sound is
    /// still loading.
    #[test]
    fn an_edit_needing_no_new_sound_is_never_delayed() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        for edit in [
            "s(\"sine\").gain(0.5)",
            "s(\"sine\").gain(slider(0.4, 0, 1))",
            "s(\"sine\")\nosc(10).out()",
        ] {
            assert!(!engine.hold_update(edit, false, false), "{edit}");
        }

        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"<sine sine kick>\")");
        assert!(!engine.start_is_held(), "the kick is past the first window");
        assert!(!engine.hold_update("s(\"<sine sine kick>\").gain(0.5)", false, false));
    }

    /// A newer edit replaces a held one.
    #[test]
    fn a_newer_edit_replaces_a_held_one() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        assert!(engine.hold_update("s(\"kick\")", false, false));
        engine.cancel_pending_launch();
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Err(RuntimeError::Cancelled))
        ));
        assert!(engine.hold_update("s(\"kick\").gain(0.5)", false, false));
        library.finish_loading_sample_for_test();
        turn_until(&mut engine, "the newer edit", |engine| {
            engine.active_source() == Some("s(\"kick\").gain(0.5)")
        });
    }

    /// A quantised launch whose sound is still loading lets its line pass and
    /// lands on the first line after the sound is in.
    #[test]
    fn a_quantised_launch_lands_on_the_first_line_after_its_sounds() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        turns_for(&mut engine, Duration::from_millis(100));
        let armed = engine
            .arm_launch("s(\"kick\")", false, 0.25, false)
            .expect("arm")
            .expect("a line");
        let first_line = armed.boundary_cycle;
        turn_until(&mut engine, "the first line passing", |engine| {
            engine.session.cycle_at_time(device_now(engine)) > first_line + 0.25
        });
        assert!(engine.take_launch_outcome().is_none(), "it has not fired");
        library.finish_loading_sample_for_test();
        let arrived = engine.session.cycle_at_time(device_now(&engine));
        turn_until(&mut engine, "the launch firing", |engine| {
            engine.launch_outcome.is_some()
        });
        let landing = engine
            .landing
            .as_ref()
            .expect("the fired launch")
            .boundary_cycle;
        assert!(landing > arrived, "{landing} after {arrived}");
        assert!(landing - arrived <= 0.25 + 0.1, "the first line after");
    }

    /// A Play pressed while a preview's silent output is open is held, and the
    /// preview ending does not close the output it is about to take over.
    #[test]
    fn a_play_held_over_a_preview_keeps_its_output_and_lands() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "silence");
        engine.live.as_mut().expect("playing").audition_owned = true;
        engine.audition_horizon_frame = 0;
        assert!(engine.hold_update("s(\"kick\")", false, false));
        turns_for(&mut engine, Duration::from_millis(300));
        assert!(
            engine.is_playing() && !engine.is_stopping(),
            "the output stays"
        );
        library.finish_loading_sample_for_test();
        turn_until(&mut engine, "the play landing", |engine| {
            engine.active_source() == Some("s(\"kick\")")
        });
        assert!(!engine.live.as_ref().expect("playing").audition_owned);
        turns_for(&mut engine, Duration::from_millis(100));
        assert!(engine.is_playing() && !engine.is_stopping());
    }

    /// Previews sound through a held start's output as through a playing
    /// one: a preview whose sample comes in while the start waits is pushed
    /// then, not when the start opens, and a run hands each note over at its
    /// own moment rather than all at the downbeat.
    #[test]
    fn previews_play_while_a_start_is_held() {
        let library = Arc::new(SampleLibrary::with_loading_note_bank_for_test(
            "keys",
            &[36.0, 60.0],
        ));
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "note(\"c4\").s(\"keys\")");
        turns_for(&mut engine, Duration::from_millis(50));
        assert!(engine.start_is_held(), "the start waits for its key");

        engine
            .audition_notes(&[36.0], "keys", 0.5)
            .expect("a preview");
        let pending = |engine: &StudioEngine| {
            engine
                .live
                .as_ref()
                .is_some_and(|live| live.audition.is_some())
        };
        assert!(pending(&engine), "the previewed key is loading");
        library.finish_note_key_for_test(36.0);
        turns_for(&mut engine, Duration::from_millis(50));
        assert!(
            !pending(&engine),
            "the preview went out once its key was in"
        );
        assert!(engine.start_is_held(), "while the start still waits");

        engine
            .audition_run(&[60.0, 62.0], "sine", 0.5, 0.2)
            .expect("a run");
        turn_until(&mut engine, "the run's second note", |engine| {
            engine.live.as_ref().is_some_and(|live| live.run.is_none())
        });
        assert!(engine.start_is_held(), "handed over while the start waits");
    }

    /// The cue asks for what it follows as bets, never ahead of what plays,
    /// and follows the banked spelling a `.bank()` plays rather than the
    /// default bank's name the score never plays.
    #[test]
    fn the_cue_asks_only_bets_and_only_for_what_the_bank_plays() {
        let library = Arc::new(SampleLibrary::with_unasked_banks_for_test(&[
            "bd", "tr909_bd",
        ]));
        let mut engine = studio(LoadMode::Async, &library);
        let source = "s(\"bd\").bank(\"tr909\")";
        assert_eq!(
            engine.followed_in_text(source, false),
            [Followed::Named {
                name: "tr909_bd".into(),
                n: 0.0
            }]
        );
        engine.follow_edit(source, false);
        super::tests::play_silently(&mut engine);
        let cue = engine.loading_cue().expect("the banked kick is loading");
        assert_eq!(cue.total, 1);
        assert_eq!(
            library.queued_loads_for_test(),
            (0, 1),
            "a bet, and only one"
        );
        assert!(!library.asked_for_test("unasked-bd.wav"));
    }

    /// In wait, what a held edit or an armed launch waits on is asked at play
    /// priority, as an evaluated update's window is, so a bet placed since
    /// does not go ahead of it. Async leaves an armed launch's sounds
    /// to bets until it fires.
    #[test]
    fn a_waiting_update_asks_for_its_sounds_ahead_of_every_bet() {
        let library = Arc::new(SampleLibrary::with_unasked_banks_for_test(&[
            "kick", "snare",
        ]));
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        assert!(engine.hold_update("s(\"kick\")", false, false));
        assert_eq!(
            library.queued_loads_for_test(),
            (1, 0),
            "the kick plays next"
        );
        engine.cancel_pending_launch();
        engine
            .arm_launch("s(\"snare\")", false, 1.0, false)
            .expect("arm")
            .expect("a line");
        assert_eq!(library.queued_loads_for_test(), (2, 0), "so does the snare");

        let library = Arc::new(SampleLibrary::with_unasked_banks_for_test(&["snare"]));
        let mut engine = studio(LoadMode::Async, &library);
        start(&mut engine, "s(\"sine\")");
        engine
            .arm_launch("s(\"snare\")", false, 1.0, false)
            .expect("arm")
            .expect("a line");
        turn(&mut engine);
        assert_eq!(
            library.queued_loads_for_test(),
            (0, 1),
            "a bet until it fires"
        );
    }

    /// The late-sounds line names every sound that came late during the
    /// load, whichever turn it came late on.
    #[test]
    fn the_late_line_names_every_sound_late_on_any_turn_of_the_load() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Async, &library);
        start(&mut engine, "s(\"kick\")");
        engine.note_late("sample \"snare:0\" is still loading");
        engine.settle_loads();
        engine.note_late("sample \"hat:2\" is still loading");
        engine.settle_loads();
        assert!(
            !engine
                .pending_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC),
            "not while the kick loads"
        );
        library.finish_loading_sample_for_test();
        engine.settle_loads();
        let late: Vec<_> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC)
            .collect();
        assert_eq!(late.len(), 1, "{late:?}");
        assert!(late[0].message.contains("hat:2") && late[0].message.contains("snare:0"));
    }

    /// An edit waits only for what its live code plays: not for a name in a
    /// comment, in a muted lane, or under a bank that does not hold it.
    #[test]
    fn an_edit_waits_only_for_live_names() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        for edit in [
            "// s(\"kick\")\n$: s(\"sine\")",
            "_$: s(\"kick\")\n$: s(\"sine\")",
            "$: s(\"sine\")\n$_: s(\"kick\")",
            "$: s(\"sine\")\ndrums_ : s(\"kick\")",
            "$: s(\"sine\")\n_$: stack(\n  s(\"sine\"),\n  // kick\n\n  s(\"kick\"),\n)",
            "$: s(\"kick\").bank(\"other\")",
        ] {
            assert!(!engine.hold_update(edit, false, false), "{edit}");
        }
        assert!(engine.hold_update("$: s(\"kick\")", false, false));
    }

    /// A bank keyed by note stands for every key: the edit lands once the
    /// last key's file is in, and the cue counts each key.
    #[test]
    fn a_note_keyed_bank_waits_for_every_key() {
        let library = Arc::new(SampleLibrary::with_loading_note_bank_for_test(
            "keys",
            &[36.0, 60.0],
        ));
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"sine\")");
        let edit = "note(\"c2 c4\").s(\"keys\")";
        assert!(engine.hold_update(edit, false, false));
        let cue = engine.snapshot().loading.expect("the keys load");
        assert_eq!((cue.settled, cue.total), (0, 2));

        library.finish_note_key_for_test(36.0);
        turns_for(&mut engine, Duration::from_millis(100));
        assert!(engine.load.held_edit.is_some(), "one key is not the bank");
        let cue = engine.snapshot().loading.expect("one key still loads");
        assert_eq!((cue.settled, cue.total), (1, 2));

        library.finish_note_key_for_test(60.0);
        turn_until(&mut engine, "the edit landing", |engine| {
            engine.active_source() == Some(edit)
        });
    }

    /// An edit whose sounds come from its own import asks for the import
    /// first, so its names read as loading and it waits for them.
    #[test]
    fn an_edit_asks_for_its_own_import_and_waits_for_it() {
        let library = Arc::new(SampleLibrary::empty());
        let mut session = SessionConfig::default();
        session
            .score_sample_access
            .permit_origin("http://127.0.0.1:9")
            .expect("the test origin");
        let mut engine = studio_with(LoadMode::Wait, &library, session);
        start(&mut engine, "s(\"sine\")");
        let hold = library.hold_manifest_worker_for_test();
        let spec = "http://127.0.0.1:9/kit.json";
        let edit = format!("samples('{spec}')\n$: s(\"kit_bd\")");
        assert_eq!(library.samples_source_state(spec), None);
        assert!(engine.hold_update(&edit, false, false));
        assert_eq!(
            library.samples_source_state(spec),
            Some(rustel_runtime::samples::SourceState::Loading),
            "the import was asked for"
        );
        turns_for(&mut engine, Duration::from_millis(100));
        assert!(
            engine.load.held_edit.is_some(),
            "the edit waits for the map"
        );
        drop(hold);
        turn_until(&mut engine, "the edit landing", |engine| {
            engine.launch_outcome.is_some()
        });
    }

    /// An update with a mistake sent while a start waits leaves the held
    /// start exactly as it was; a good one replaces it from its own cycle
    /// zero and waits for its own first window.
    #[test]
    fn an_update_during_a_held_start_replaces_it_only_if_it_evaluates() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"kick\")");
        turns_for(&mut engine, Duration::from_millis(50));
        let generation = engine.generation();
        assert!(engine.evaluate("s(\"kick\"", false).is_err());
        assert!(engine.start_is_held(), "the held start stays");
        assert_eq!(engine.generation(), generation);
        assert_eq!(engine.active_source(), Some("s(\"kick\")"));

        let install = engine
            .evaluate("s(\"kick\").gain(0.8)", false)
            .expect("a good update");
        assert!(engine.start_is_held(), "it waits for its own first window");
        assert_eq!(
            engine
                .live
                .as_ref()
                .and_then(|live| live.initial_start_generation),
            Some(install.generation)
        );
        let kick = library.finish_loading_sample_for_test();
        let arrived = device_now(&engine);
        turn_until(&mut engine, "the kick", |engine| {
            engine.played_by_text.contains(&kick)
        });
        assert!(engine.session.cycle_at_time(arrived) < 0.0);
    }

    /// A slider moved while a start waits is part of the running score, its
    /// layout having reached the display on the waiting turns, and moving it
    /// does not requery: the first query reads it, and cycle zero stays the
    /// start's.
    #[test]
    fn a_held_start_takes_slider_moves_without_moving_its_cycle_zero() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(&mut engine, "s(\"kick\").gain(slider(0.5, 0, 1))");
        let sent = turns_for(&mut engine, Duration::from_millis(100));
        assert!(engine.start_is_held());
        let layout = sent
            .iter()
            .find_map(|update| match update {
                StudioUpdate::Layout(layout) => Some(layout),
                _ => None,
            })
            .expect("the layout reaches the display while the start waits");
        let slider = layout
            .ui_layout
            .sliders
            .first()
            .expect("the slider")
            .id
            .clone();
        let generation = engine.generation();
        engine
            .set_slider(&slider, 0.9)
            .expect("the waiting score's slider moves");
        turns_for(&mut engine, Duration::from_millis(200));
        assert_eq!(engine.generation(), generation, "no requery");

        let kick = library.finish_loading_sample_for_test();
        let arrived = device_now(&engine);
        turn_until(&mut engine, "the kick", |engine| {
            engine.played_by_text.contains(&kick)
        });
        assert!(engine.session.cycle_at_time(arrived) < 0.0);
    }

    /// A key pressed while a start waits does not requery either: the first
    /// query reads the keys as they stand, and cycle zero stays the start's.
    #[test]
    fn a_held_start_takes_key_presses_without_moving_its_cycle_zero() {
        let library = loading_kick();
        let mut engine = studio(LoadMode::Wait, &library);
        start(
            &mut engine,
            "const kb = await midikeys('keyboard')\n$: kb(0.25).s(\"tri\")\n$: s(\"kick\")\n",
        );
        turns_for(&mut engine, Duration::from_millis(50));
        assert!(engine.start_is_held());
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");
        let generation = engine.generation();
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        turns_for(&mut engine, Duration::from_millis(200));
        assert_eq!(engine.generation(), generation, "no requery");

        let kick = library.finish_loading_sample_for_test();
        let arrived = device_now(&engine);
        turn_until(&mut engine, "the kick", |engine| {
            engine.played_by_text.contains(&kick)
        });
        assert!(engine.session.cycle_at_time(arrived) < 0.0);
    }

    /// The alerts `engine` has queued, raised or resolved.
    fn queued_alerts(engine: &StudioEngine) -> Vec<DiagnosticAlert> {
        engine
            .pending_diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.alert.clone())
            .collect()
    }

    /// An import's failure is said under the key of the map it names, and
    /// resolved once a retry brings the map in. A failure naming no map is
    /// no import's, whatever its words.
    #[test]
    fn a_failed_import_is_said_under_its_key_and_resolved_when_it_comes_in() {
        let library = Arc::new(SampleLibrary::empty());
        let mut engine = studio(LoadMode::Wait, &library);
        let spec = "github:me/kit";
        let gone = rustel_runtime::samples::SourceState::Failed("the map is gone".into());
        library.note_samples_source_state_for_tests(spec, gone);
        let said = engine.import_failure(SampleFailure {
            message: "the map is gone".into(),
            maps: vec![serde_json::to_string(spec).expect("a string")],
        });
        let key = import_alert(spec);
        assert_eq!(said.alert, Some(DiagnosticAlert::Raise(key.clone())));
        queue_diagnostic(&mut engine.pending_diagnostics, said);
        emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |_| Ok(()));
        let unrelated = engine.import_failure(SampleFailure::from("the map is gone".to_owned()));
        assert_eq!(unrelated.alert, None, "not every failure is an import's");

        engine.settle_loads();
        assert!(engine.pending_diagnostics.is_empty(), "still failed");
        library
            .note_samples_source_state_for_tests(spec, rustel_runtime::samples::SourceState::Ready);
        engine.settle_loads();
        assert_eq!(queued_alerts(&engine), [DiagnosticAlert::Resolve(key)]);
    }

    /// A batch spent on its first map says so once, about every map it left
    /// failed: the map it never reached coming in resolves nothing while the
    /// first still fails, and the line resolves once both are in.
    #[test]
    fn a_spent_batch_resolves_only_once_every_map_it_left_failed_is_in() {
        let library = Arc::new(SampleLibrary::empty());
        let mut engine = studio(LoadMode::Wait, &library);
        let (_silent, first, later) = silent_maps();
        library.register_samples_batch_for_test(&[&first, &later], Duration::from_millis(300));
        library.wait_until_idle(Duration::from_secs(10));

        engine.collect_session_diagnostics();
        let alerts = queued_alerts(&engine);
        let [DiagnosticAlert::Raise(key)] = alerts.as_slice() else {
            panic!("the spent batch is said under one alert: {alerts:?}");
        };
        let key = key.clone();
        engine.pending_diagnostics.clear();

        library.note_samples_source_state_for_tests(
            &later,
            rustel_runtime::samples::SourceState::Ready,
        );
        engine.settle_loads();
        assert!(
            queued_alerts(&engine).is_empty(),
            "the first map still fails"
        );
        library.note_samples_source_state_for_tests(
            &first,
            rustel_runtime::samples::SourceState::Ready,
        );
        engine.settle_loads();
        assert_eq!(queued_alerts(&engine), [DiagnosticAlert::Resolve(key)]);
    }

    /// A failed import the session's own scheduling meets before the engine
    /// collects it keeps its alert.
    #[test]
    fn a_failed_import_met_by_a_scheduling_step_keeps_its_alert() {
        let library = Arc::new(SampleLibrary::empty());
        let mut engine = studio(LoadMode::Wait, &library);
        let spec = "github:me/kit";
        engine.session.evaluate("s(\"sine\")").expect("a score");
        library.register_samples_batch_for_test(&[spec], Duration::ZERO);
        library.wait_until_idle(Duration::from_secs(10));

        engine
            .session
            .schedule_audio_through(0.0, 0.5, 48_000)
            .expect("a scheduling step");
        engine.collect_session_diagnostics();
        assert_eq!(
            queued_alerts(&engine),
            [DiagnosticAlert::Raise(import_alert(spec))]
        );
    }
}

#[cfg(test)]
mod sample_recording_tests {
    use super::*;
    use rustel_audio::input::{INPUT_RING_FRAMES, InputRing};

    fn recording(ring: &Arc<InputRing>, path: std::path::PathBuf) -> SampleRecording {
        SampleRecording {
            writer: TakeWriter::start(path, 48_000).expect("writer"),
            cursor: ring.written(),
            ring: Arc::clone(ring),
            own_input: None,
            dropped: 0,
        }
    }

    /// A mono microphone lands on both sides of the stereo file, frame for
    /// frame, starting where the recording started and not before.
    #[test]
    fn a_mono_input_is_recorded_on_both_sides_from_the_press() {
        let directory = tempfile::tempdir().expect("temp dir");
        let ring = Arc::new(InputRing::new());
        ring.set_channels(1);
        ring.set_sample_rate(48_000);
        // Before the press: not in the sample.
        ring.write(&[0.9; 256], 1);
        let mut sample = recording(&ring, directory.path().join("sample.wav"));
        ring.write(&[0.5; 1_000], 1);
        sample.drain();
        ring.write(&[0.25; 500], 1);
        sample.drain();
        let status = sample.writer.finish();
        assert_eq!(status.error, None);
        assert_eq!(status.frames, 1_500);
        let signal = status.final_signal.expect("summary");
        assert_eq!(signal.sample_count, 3_000, "two channels a frame");
        assert!(
            (signal.finite_peak - 0.5).abs() < 1e-6,
            "the 0.9 before the press is not in it"
        );
        assert_eq!(sample.dropped, 0);
    }

    /// Recorded with the transport stopped, as a sample often is: the
    /// stopped turn drains the input like the playing one, so the file holds
    /// the whole take and not the last second the ring still had.
    #[test]
    fn a_sample_recorded_while_stopped_keeps_its_beginning() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".into()),
            ..StudioConfig::default()
        })
        .expect("engine");
        let ring = Arc::new(InputRing::new());
        ring.set_channels(1);
        ring.set_sample_rate(48_000);
        engine.sample_recording = Some(recording(&ring, directory.path().join("sample.wav")));
        // Five seconds of input, a turn every tenth of one: four times what
        // the ring holds, so only draining on every stopped turn keeps it.
        for tenth in 0..50 {
            let level = if tenth == 0 { 0.75 } else { 0.25 };
            ring.write(&vec![level; 4_800], 1);
            engine.idle_turn(|_| Ok(()));
        }
        let status = engine
            .stop_sample_recording()
            .expect("a sample was recording");
        assert_eq!(status.error, None);
        assert_eq!(status.frames, 50 * 4_800, "every frame, from the first");
        let signal = status.final_signal.expect("summary");
        assert!(
            (signal.finite_peak - 0.75).abs() < 1e-6,
            "the loud first tenth is in the file"
        );
    }

    /// A pump that finds more than one chunk waiting takes all of it.
    #[test]
    fn one_drain_takes_everything_the_ring_holds() {
        let directory = tempfile::tempdir().expect("temp dir");
        let ring = Arc::new(InputRing::new());
        ring.set_channels(1);
        ring.set_sample_rate(48_000);
        let mut sample = recording(&ring, directory.path().join("sample.wav"));
        ring.write(&vec![0.5; 40_000], 1);
        sample.drain();
        assert_eq!(sample.cursor, ring.written());
        assert_eq!(sample.dropped, 0);
        assert_eq!(sample.writer.finish().frames, 40_000);
    }

    /// A reader that fell a whole ring behind counts what the writer came
    /// round over, rather than recording frames that are no longer those.
    #[test]
    fn frames_the_ring_wrote_over_are_counted_not_recorded() {
        let directory = tempfile::tempdir().expect("temp dir");
        let ring = Arc::new(InputRing::new());
        ring.set_channels(2);
        ring.set_sample_rate(48_000);
        let mut sample = recording(&ring, directory.path().join("sample.wav"));
        let burst = vec![0.1f32; (INPUT_RING_FRAMES + 4_096) * 2];
        ring.write(&burst, 2);
        while sample.cursor < ring.written() {
            sample.drain();
        }
        assert!(
            sample.dropped >= 4_096 + SAMPLE_RING_GUARD_FRAMES,
            "{}",
            sample.dropped
        );
        let status = sample.writer.finish();
        assert_eq!(
            status.frames + sample.dropped,
            (INPUT_RING_FRAMES + 4_096) as u64,
            "every frame is either on disk or counted lost"
        );
    }
}
