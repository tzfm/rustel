//! Headless Session API and shared helpers for the `rustel` CLI.
//!
//! This crate connects the transpiler, QuickJS host, pattern query engine,
//! scheduler, and audio renderer. It supports deterministic offline rendering
//! and optional CPAL playback with file-watched live reloads.

/// Replacing a file's contents in one rename.
mod atomic_file;
mod capabilities;
mod capability_report;
/// Shared configuration, session, and sample-cache directory rules.
pub mod config_dir;
#[cfg(feature = "device-audio")]
mod engine_pressure;
/// Handing freed heap back to the operating system.
pub mod free_memory;
/// Polling gamepad devices and publishing their state for scores.
#[cfg(feature = "gamepad")]
pub mod gamepad;
mod hap_json;
/// Keeping macOS's view of the MIDI ports current; a no-op elsewhere.
#[cfg(feature = "midi")]
pub mod midi_hotplug;
/// Progress lines on stderr are text for a person unless the run asked
/// for JSON, in which case they are JSON objects - one or the other.
pub fn set_progress_json(on: bool) {
    render::set_progress_json(on);
}
#[cfg(feature = "hydra")]
pub mod hydra;
#[cfg(feature = "hydra")]
mod hydra_input;
pub mod lint;
#[cfg(feature = "device-audio")]
mod live;
#[cfg(feature = "midi")]
pub mod midi_bridge;
/// MIDI clock sync: 24 ppq ticks out to the hardware, and a follower that
/// keeps the scheduler on a clock coming in. Shared by the live command and
/// the studio, because both drive the same scheduler.
#[cfg(feature = "midi")]
pub mod midi_clock;
#[cfg(feature = "midi")]
pub mod midi_input;
#[cfg(feature = "osc")]
pub mod osc_bridge;
mod process_stats;
mod producer;
/// Product names shared by configuration, diagnostics, and CLI help.
pub mod product;
mod render;
pub mod sample_fetch;
pub mod sample_server;
pub mod samples;
/// The shared "is this a valid score" answer behind `check` and
/// `query`: the static lint plus a fallback-free dry evaluation.
pub mod score_check;
#[cfg(feature = "serial")]
pub mod serial_bridge;
mod session;
/// Recording and replaying a live-coding set (`--save-session` / `replay`).
#[cfg(feature = "session-log")]
pub mod session_log;
pub mod settings;
/// Reading sound names out of score TEXT, so warming covers what a query
/// cannot see: lanes commented out for later, and muted `_$:` lanes.
pub mod sounds;
/// Showing untrusted text on a terminal as visible characters only.
pub mod terminal_text;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
/// Producer-side audio reduction for terminal scopes and spectra.
pub mod ui_analysis;
/// Bounded, versioned live-editor event protocol.
pub mod ui_events;
pub mod updates;
mod watch;

pub use capabilities::{
    AccelerationPreference, AccelerationPreferenceParseError, AudioHost, AudioInputFacts,
    AudioOutputFacts, AudioSampleFormat, AudioStreamFacts, CapabilityId, CapabilityReason,
    CapabilityRegistry, CapabilityStatus, CpuArchitecture, CpuFeatureId, CpuFeatureSet,
    CpuFeatureStatus, DspDispatch, capability_registry, capability_registry_for,
    capability_registry_for_dispatch, capability_registry_with_audio, detected_cpu_features,
};
pub use capability_report::{
    CAPABILITY_REPORT_SCHEMA_VERSION, CapabilityAudioHostV1, CapabilityAudioInputV1,
    CapabilityAudioV1, CapabilityBuildV1, CapabilityCpuFeatureV1, CapabilityCpuV1,
    CapabilityReportContext, CapabilityReportV1, CapabilityRuntimeV1, CapabilitySafetyFacts,
    CapabilitySafetyV1, CapabilityStatusV1,
};
#[cfg(feature = "device-audio")]
pub use engine_pressure::{
    ENGINE_PRESSURE_SCHEMA_VERSION, EnginePressureAssetQueuesV1, EnginePressureCause,
    EnginePressureDeviceV1, EnginePressureDspV1, EnginePressureLevel, EnginePressureMonitor,
    EnginePressurePoolValuesV1, EnginePressurePoolsV1, EnginePressureProcessV1,
    EnginePressureQueueV1, EnginePressureQueuesV1, EnginePressureReportContext,
    EnginePressureReportV1, EnginePressureSchedulerV1, EnginePressureSnapshot,
    EnginePressureStatusV1, EnginePressureVoicesV1,
};
pub use hap_json::{
    BenchMetrics, HapJson, OnsetEventJson, PlayReport, QueryReport, RenderReport, SpanJson,
    ValueJson, haps_to_json,
};

/// The stack every thread gets if it will build a JavaScript runtime and query
/// through it.
///
/// A query recurses once per graph node, and `MAX_PATTERN_DEPTH` admits 512 of
/// them: roughly 24 MiB of debug-profile frames, more than the platform
/// default on any of the three targets. QuickJS measures its own budget
/// against this same stack. `MAX_JS_STACK_BYTES` must fit inside what the
/// thread owns, or a call QuickJS means to refuse with `RangeError` overflows
/// the stack first. The assertion below checks that fit. A thread that uses
/// the default stack size can turn a diagnosed refusal into a segmentation
/// fault.
pub const QUERY_WORKER_STACK_BYTES: usize = 64 * 1024 * 1024;

const _: () = assert!(
    QUERY_WORKER_STACK_BYTES > rustel_jsruntime::MAX_JS_STACK_BYTES,
    "a query worker's stack must be larger than the budget QuickJS measures against it"
);

/// Bound on the writes an output bridge holds for one port whose open is
/// still in flight. MIDI and serial both hold a score's onsets while a slow
/// device opens, and share the one ceiling so the two cannot drift apart.
#[cfg(any(feature = "midi", feature = "serial"))]
pub(crate) const MAX_PENDING_OPEN_WRITES_PER_PORT: usize = 1024;
#[cfg(feature = "hydra")]
pub use hydra::{HydraBridge, HydraUpdate, audio_frame as hydra_audio_frame};
#[cfg(feature = "device-audio")]
pub use live::{LiveFileProducer, LiveProducerStep};
#[cfg(feature = "osc")]
pub use osc_bridge::ScoreOscAccess;
pub use process_stats::{ProcessMonitor, ProcessStats};
pub use producer::{
    PRODUCER_REPORTING_WINDOW_TURNS, ProducerLoadSnapshot, ProducerPhase, ProducerPhaseSnapshot,
    ProducerTurnOutcome,
};
pub use render::{MP3_EXPORT, RenderFormat, write_onset_dump, write_scalar_wav, write_silent_wav};
pub use rustel_audio::TakeoverCut;
pub use samples::{
    SCORE_SAMPLE_CACHE_MAX_BYTES, SCORE_SAMPLE_CACHE_MAX_ENTRIES,
    SCORE_SAMPLE_CACHE_SESSION_MAX_BYTES, SCORE_SAMPLE_CACHE_SESSION_MAX_ENTRIES,
    ScoreSampleAccess, clear_score_sample_cache, sample_cache_dir, score_sample_cache_dir,
};
pub use session::{
    EvaluateSource, PREVIEW_ONSET_ID_FLAG, RuntimeError, SAMPLE_AWAITED_DIAGNOSTIC,
    SAMPLE_LOADING_DIAGNOSTIC, Session, SessionConfig, SessionDiagnostic, VOICE_NOTICE_DIAGNOSTIC,
    WindowSound,
};

#[cfg(any(test, feature = "test-support"))]
pub use session::SessionPanicPoint;
pub use session::{catch_score_panic, panic_is_contained};
pub use watch::{
    FileWatch, MAX_WATCH_SOURCE_BYTES, ReloadEvent, ReloadStatus, WatchLanguage, WatchPoll,
    WatchTarget,
};
