//! Opt-in CPAL output for finite and continuously scheduled scalar audio.
//!
//! Both paths keep scheduling, locks, syscalls and dynamic
//! allocation off the CPAL callback. The live path crosses a fixed-capacity
//! POD ring and filters replaced generations at the block boundary. Device
//! recovery after device loss is not implemented by this module. Live reports
//! count xruns when the host reports them.

mod callback;
mod control;
mod inserts;
mod stream;

use callback::flag_stream_failure;
use callback::live_error_callback;
use callback::write_live_output;
use callback::write_output;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
pub use stream::audio_device_inventory;
pub use stream::audio_devices;
#[cfg(feature = "test-support")]
use stream::buffer_preference_error;
pub use stream::input_retry_delay;
use stream::rebase_frame_rate;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, FromSample, I24, Sample, SampleFormat, SizedSample, StreamInstant,
    SupportedBufferSize, U24,
};

use crate::{
    AudioBackend, AudioBufferPreference, AudioBufferRange, AudioHost, AudioInputFacts,
    AudioOutputFacts, AudioSampleFormat, AudioStreamFacts, DspDispatch,
    LIVE_ANALYSIS_SIDES_SAMPLES, LIVE_ANALYSIS_WINDOW_SAMPLES, LiveOutputOptions,
    LiveScalarBackend, MAX_LIVE_VOICES, OnsetEvent, Ring, ScalarBackend, TakeoverCut,
    pressure::{RealtimePressureMeter, RealtimePressurePublisher},
    realtime::{RealtimeLoadMeter, RealtimeLoadPublisher},
    tripwire,
};

#[cfg(test)]
use crate::AudioEvent;
#[cfg(test)]
use crate::realtime::REALTIME_LOAD_PUBLISH_INTERVAL;

#[cfg(test)]
#[global_allocator]
static DEVICE_TEST_ALLOCATOR: tripwire::TripwireAlloc = tripwire::TripwireAlloc;

const LIVE_RING_CAPACITY: usize = 8192;
// Four windows leave enough distance that a producer-side snapshot can be
// descheduled briefly without the callback overwriting every slot it reads.
// A slot carries its lap and sample in one AtomicU64, so readers never observe
// a generation tag from one callback with sample bits from another.
const LIVE_ANALYSIS_RING_CAPACITY: usize = LIVE_ANALYSIS_WINDOW_SAMPLES * 4;
// 128 frames (2.7 ms at 48 kHz) is what an interactive instrument asks for:
// a key press voiced in the next callback is heard before a 10 ms host period
// even rolls over. A heavier set that underruns here asks for more through the
// output-latency knob or RUSTEL_LIVE_BUFFER_FRAMES; forwarded audio paths
// (WSLg's RDP sink) already do exactly that by default.
const TARGET_LIVE_BUFFER_FRAMES: u32 = 128;
const MAX_LIVE_PLAYBACK_LATENCY_NANOS: u64 = 250_000_000;
const MAX_LIVE_LATENCY_ENV: &str = "RUSTEL_MAX_LIVE_LATENCY_MS";
const LIVE_BUFFER_FRAMES_ENV: &str = "RUSTEL_LIVE_BUFFER_FRAMES";
const UNSET_STREAM_INSTANT_NANOS: u64 = u64::MAX;
static NEXT_LIVE_STREAM_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub enum DevicePlaybackError {
    Unavailable(String),
    ResourceLimit(String),
    Cancelled,
}

impl std::fmt::Display for DevicePlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => formatter.write_str(message),
            Self::ResourceLimit(message) => formatter.write_str(message),
            Self::Cancelled => formatter.write_str("device playback cancelled"),
        }
    }
}

impl std::error::Error for DevicePlaybackError {}

/// One opened default device. Opening and discovery happen before rendering;
/// the data callback receives only a prepared scalar backend and plain PCM
/// storage.
pub struct ScalarDevice {
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
    host_id: Box<str>,
    name: String,
}

struct DiscoveredOutput {
    device: cpal::Device,
    host_id: Box<str>,
    name: String,
}

/// The name of the output that makes no sound: the live callback run by a
/// thread on the wall clock, its audio discarded. The clock, the meters,
/// the visualizers, takes and the graceful stop all work as they do on a
/// device - on a machine with no audio hardware, over SSH, in a recording.
pub const SILENT_OUTPUT_NAME: &str = "silent";

/// How long to wait before trying a refused audio input again, and how far
/// that delay doubles. A device that is busy or still waking comes back on
/// its own within a second; an unplugged one must be neither re-enumerated
/// every tick nor forgotten for minutes.
const INPUT_RETRY_BASE: Duration = Duration::from_millis(500);
const INPUT_RETRY_MAX_SHIFT: u64 = 5;

/// Coalesces one producer turn's shared-orbit reverb requests. The final
/// scheduled parameters for each orbit are prepared once per turn.
pub struct LiveReverbBatch {
    publication: Option<crate::KabelPublication>,
    requested: [Option<crate::reverb::ReverbParams>; crate::scalar::MAX_ORBITS],
    /// The last insert each slot asked for in this turn.
    inserts: Vec<(crate::insert::InsertSlot, crate::insert::InsertKey)>,
    refused: u64,
}

#[derive(Clone, Copy)]
struct KabelReservation {
    delays: usize,
    bytes: usize,
    pointer: usize,
    until_secs: f64,
}

#[derive(Clone, Copy)]
struct KabelProgramReservation {
    id: rustel_kabel::ProgramId,
    pointer: usize,
}

/// One continuously running output fed by the scheduler through a
/// fixed-capacity SPSC ring. The query thread owns pushes and generation
/// changes; the audio callback owns DSP and pops.
pub struct LiveScalarDevice {
    stream: Option<cpal::Stream>,
    callback_gate: Option<Arc<CallbackGate>>,
    /// The silent output's stop flag and thread; joined before any
    /// replacement stream starts so only one thread touches `LiveShared`.
    silent: Option<SilentHandle>,
    /// The audio input, when one is open beside the output.
    input: Option<AudioInput>,
    /// Producer-side record of each orbit's installed reverb parameters,
    /// and the bytes that reverb holds: the callback owns the reverb, so
    /// its size is read here, where it was generated.
    /// A mutex only because the producer closures hold `&self`; the audio
    /// thread never touches it.
    reverb_cache: std::sync::Mutex<Vec<Option<(crate::reverb::ReverbParams, usize)>>>,
    /// Recently shipped `.FX()` stage reverbs. The pointer identity lets the
    /// producer forget a fingerprint when the callback returns that box.
    fx_reverb_cache: std::sync::Mutex<Vec<(crate::reverb::ReverbParams, usize, usize)>>,
    kabel_cache: std::sync::Mutex<Vec<KabelReservation>>,
    program_cache: std::sync::Mutex<Vec<KabelProgramReservation>>,
    /// Shipped logical slots and box addresses, one entry for each copy the
    /// callback did not return. Returned copies invalidate only their own
    /// entry, including a copy refused by the callback pool.
    insert_cache:
        std::sync::Mutex<Vec<(crate::insert::InsertSlot, crate::insert::InsertKey, usize)>>,
    insert_waiting:
        std::sync::Mutex<Vec<(crate::insert::InsertSlot, crate::insert::InsertKey, Instant)>>,
    /// Builds an insert outside the callback. With no provider, a note that
    /// asks for an insert plays dry.
    insert_provider: std::sync::Mutex<Option<Arc<crate::insert::InsertProvider>>>,
    /// Names the chains of the playing score before each turn of inserts.
    insert_names: std::sync::Mutex<Option<Arc<crate::insert::InsertNames>>>,
    shared: LiveShared,
    sample_rate: u32,
    channels: u16,
    host_id: Option<Box<str>>,
    output_sample_format: Option<AudioSampleFormat>,
    name: String,
    stream_id: u64,
    options: LiveOutputOptions,
    requested_buffer_frames: u32,
    reported_buffer_frames: Option<u32>,
    /// The host's fixed callback period, when it advertised one (WASAPI's
    /// shared-mode period): see [`AudioOutputFacts::device_period_frames`].
    device_period_frames: Option<u32>,
    tripwire_baseline: tripwire::Violations,
    /// Positive controls run by this device, excluded without erasing real
    /// callback violations from the original reporting window.
    tripwire_canaries: tripwire::Violations,
    /// What the callback's backend wrote when it was prepared, read before
    /// the callback took it: see [`LiveScalarBackend::prepared_bytes`].
    backend_bytes: usize,
}

/// What an open output holds in memory, as the producer can see it without
/// reaching into the callback: see [`LiveScalarDevice::memory`]. The decoded
/// sounds the bank plays are the producer's own and not counted here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveDeviceMemory {
    /// The score's ring and the keyboard's, as far as pushes have reached:
    /// they start uninitialised and a slot is resident from the first push
    /// that lands on it, so a score fills its ring once it has wrapped.
    pub event_rings: usize,
    /// The input ring, as far as an input has written it, whether or not
    /// one is open now.
    pub input: usize,
    /// The record tap, as far as a take has written it.
    pub record: usize,
    /// The orbit and `.FX()` reverbs shipped to the callback, as they were
    /// measured when generated. A stage reverb the callback's budget
    /// refuses is freed and not counted.
    pub reverbs: usize,
    /// What the backend wrote when it was prepared.
    pub backend: usize,
    /// The master's analysis tap and each visual tap opened so far.
    pub analysis: usize,
}

/// Lock-free counters sampled off the callback thread.
///
/// These counters describe asset delivery, separate from host-reported xruns
/// in [`LiveDeviceReport`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssetQueuePressureSnapshot {
    pub capacity: u64,
    pub sample_installs: u64,
    pub sample_returns: u64,
    pub orbit_reverb_installs: u64,
    pub orbit_reverb_returns: u64,
    pub fx_reverb_installs: u64,
    pub fx_reverb_returns: u64,
    pub fx_reverb_refusals: u64,
    pub kabel_installs: u64,
    pub kabel_returns: u64,
    pub kabel_refusals: u64,
    pub insert_refusals: u64,
    pub leaked: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveDeviceReport {
    pub stream_id: u64,
    /// Frames submitted into host buffers, which may run far ahead of sound.
    pub submitted_frames: u64,
    /// Current audible playhead relative to the first output buffer.
    pub playhead_nanos: u64,
    /// Predicted presentation position of the latest callback buffer.
    pub buffer_playback_nanos: u64,
    pub playback_latency_nanos: u64,
    pub max_playback_latency_nanos: u64,
    pub generation: u64,
    pub callbacks: u64,
    pub accepted_events: u64,
    pub stale_events_filtered: u64,
    /// Events that arrived after their target frame had already played,
    /// indicating producer starvation rather than a host dropout.
    pub late_events: u64,
    /// Largest gap between callback completions. Scheduling delays and
    /// changes in callback work can both widen it.
    pub max_callback_gap_nanos: u64,
    /// Longest elapsed callback time, including waits and preemption.
    pub max_callback_busy_nanos: u64,
    /// Rolling callback work divided by the deadline implied by each block's
    /// own frame count and the stream's actual sample rate.
    pub realtime_load: crate::RealtimeLoadSnapshot,
    pub realtime_pressure: crate::RealtimePressureSnapshot,
    pub refused_voices: u64,
    pub callback_errors: u64,
    /// Recoverable xruns reported by the active output's host backend.
    pub callback_xruns: u64,
    /// Calls to the live DSP writer that occurred outside the callback
    /// tripwire scope. A non-zero value invalidates zero-allocation reporting.
    pub callback_scope_misses: u64,
    /// Allocator events observed while the owned live callback scope was set.
    /// These counts are valid only when the executable installs
    /// [`crate::tripwire::TripwireAlloc`] as its global allocator. Counters are
    /// process-global: overlapping devices can conservatively add violations,
    /// so exact attribution requires a single live stream.
    pub callback_allocations: u64,
    pub callback_frees: u64,
    /// A callback entered after Stop and rendered the end of the 10 ms stop
    /// ramp. Stays false when the stream closes before the ramp is out.
    pub stop_acknowledged: bool,
    pub ring_refusals: u64,
    /// Pushes or pops refused because a second thread tried to take a ring
    /// role. Always zero on a healthy run - non-zero means the
    /// single-producer/single-consumer contract was broken and the write was
    /// declined rather than allowed to race.
    pub ring_role_conflicts: u64,
    pub ring_depth: u64,
    pub ring_capacity: u64,
    pub ring_peak_depth: u64,
    pub asset_queues: AssetQueuePressureSnapshot,
    /// Blocks where generation changed after the backend's final pre-DSP
    /// check but before the device callback published its counters. Such a
    /// cutover is bounded to that block; the next block resets old voices.
    pub cutover_race_blocks: u64,
}

/// Why a live output cannot be trusted to report callback allocations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackTripwireArmError {
    MissingAllocator,
    PriorViolation {
        allocations: u64,
        frees: u64,
        scope_misses: u64,
    },
    CanaryMismatch {
        raw: tripwire::Violations,
        reported: tripwire::Violations,
    },
}

impl std::fmt::Display for CallbackTripwireArmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingAllocator => formatter.write_str(
                "audio callback tripwire allocator is absent; install rustel_audio::tripwire::TripwireAlloc in the binary",
            ),
            Self::PriorViolation {
                allocations,
                frees,
                scope_misses,
            } => write!(
                formatter,
                "audio callback tripwire recorded a prior violation ({allocations} allocations, {frees} frees, {scope_misses} scope misses); the output must be replaced before another launch"
            ),
            Self::CanaryMismatch { raw, reported } => write!(
                formatter,
                "audio callback tripwire reporting failed its positive control (raw {raw:?}, reported {reported:?})"
            ),
        }
    }
}

impl std::error::Error for CallbackTripwireArmError {}

const OUTPUT_RUNNING: u8 = 0;
const OUTPUT_STOP_REQUESTED: u8 = 1;
const OUTPUT_STOP_ACKNOWLEDGED: u8 = 2;
const OUTPUT_RESUME_PENDING: u8 = 3;

/// Resume waits for an outstanding stop ramp. A callback which observed an
/// acknowledged stop cannot acknowledge a later request after resume.
struct LiveStop(AtomicU8);

#[derive(Clone)]
struct LiveShared {
    ring: Arc<Ring>,
    /// Hand-played notes bypass the score ring and its lookahead.
    immediate: Arc<Ring>,
    live_controls: Arc<crate::assets::AssetRing<crate::live_control::LiveControlUpdate>>,
    max_polyphony: Arc<AtomicUsize>,
    confirmations: crate::confirmation::ConfirmationChannel,
    analysis: Arc<LiveAnalysisTap>,
    visual_analysis: Arc<[OnceLock<Box<LiveAnalysisTap>>; crate::scalar::MAX_UI_AUDIO_VISUALS]>,
    visual_analysis_mask: Arc<AtomicU64>,
    visual_analysis_generation_floor: Arc<AtomicU64>,
    record: Arc<RecordTap>,
    generation: Arc<AtomicU64>,
    /// Frame where the newest generation's re-query cursor starts; published
    /// BEFORE the generation flip so the consumer can keep old-generation
    /// events that sound before it (reload takeover contract).
    takeover_frame: Arc<AtomicU64>,
    /// The cut intent of the newest published cutover, as [`TakeoverCut`]
    /// bits: no cut (an edit), a cut at the consumer's flip (an immediate
    /// rewind), or a cut at the takeover frame (a quantised rewind, where
    /// the countdown plays). The consumer reads it once at the generation
    /// flip, with the takeover frame, before the new generation admits
    /// anything.
    takeover_cut: Arc<AtomicU64>,
    /// A launch's line, armed before its generation exists. The launch arms
    /// it at fire time, so the consumer cuts the outgoing rendition at the
    /// line even when the replacement's evaluation outlasts the countdown
    /// and its flip lands after the line. Without the arm, the old score
    /// plays through the line and its first beat sounds twice.
    ///
    /// ```text
    ///  63                           2   1      0
    /// +------------------------------+------+-------+
    /// | target frame (62 bits)       | drop | armed |
    /// +------------------------------+------+-------+
    /// ```
    ///
    /// `armed` cuts at the frame. `drop` also makes the consumer refuse
    /// outgoing ring events from the line on. A published flip stores zero
    /// ([`LiveScalarDevice::set_generation`]). A withdrawn arm stores
    /// [`crate::LINE_ARM_WITHDRAWN`] ([`LiveScalarDevice::clear_line_arm`]).
    line_arm: Arc<AtomicU64>,
    /// Asked for by the interface: silence what is sounding at the next
    /// block. A reload never does this - an edit that cut its own voices
    /// would click - so auditioning asks for it explicitly.
    cut_sounding: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    /// End of the latest DSP chunk that admitted scheduled events.
    /// Holds its position while Stop or its release ramp suspends intake.
    completed_intake_frame: Arc<AtomicU64>,
    /// End of the current 128-frame DSP chunk, announced before rendering.
    /// Unlike `frames`, this includes in-flight work, not certified host
    /// copies. A completed host callback leaves both frontiers equal.
    rendering_until_frame: Arc<AtomicU64>,
    playback_origin_nanos: Arc<AtomicU64>,
    buffer_playback_nanos: Arc<AtomicU64>,
    playback_latency_nanos: Arc<AtomicU64>,
    max_playback_latency_nanos: Arc<AtomicU64>,
    stop: Arc<LiveStop>,
    stop_ring_end: Arc<AtomicUsize>,
    stop_immediate_end: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    callbacks: Arc<AtomicU64>,
    accepted_events: Arc<AtomicU64>,
    stale_events: Arc<AtomicU64>,
    late_events: Arc<AtomicU64>,
    /// Nanos of the previous callback completion relative to `clock_base`.
    /// The completion gap alone cannot identify the cause of a dropout.
    last_callback_nanos: Arc<AtomicU64>,
    max_callback_gap_nanos: Arc<AtomicU64>,
    /// Longest elapsed callback time, including DSP, waits and preemption.
    /// Compare it with the completion gap without assuming either metric
    /// distinguishes plugin work from OS scheduling delays.
    max_callback_busy_nanos: Arc<AtomicU64>,
    realtime_load: Arc<RealtimeLoadPublisher>,
    realtime_pressure: Arc<RealtimePressurePublisher>,
    /// Monotonic origin for the callback-gap clock.
    clock_base: Instant,
    refused_voices: Arc<AtomicU64>,
    callback_errors: Arc<AtomicU64>,
    callback_xruns: Arc<AtomicU64>,
    callback_scope_misses: Arc<AtomicU64>,
    cutover_race_blocks: Arc<AtomicU64>,
    /// Sample delivery to the callback bank (install/return SPSC pair).
    samples: Arc<crate::assets::SampleChannel>,
    /// Master fader and post-fader metering, shared with the callback.
    meter: Arc<crate::meter::MasterMeterShared>,
    score_sources_active: Arc<AtomicBool>,
    score_peak: Arc<std::sync::atomic::AtomicU32>,
    /// Each orbit's peak since the producer last took them (f32 bits).
    orbit_peaks: Arc<Vec<std::sync::atomic::AtomicU32>>,
    /// Each orbit's fader, linear, as f32 bits: the mixer's strips, read
    /// once a block by the callback.
    orbit_gains: Arc<Vec<std::sync::atomic::AtomicU32>>,
    /// Which output pair each orbit is written to; 0 is the main pair.
    orbit_pairs: Arc<Vec<std::sync::atomic::AtomicU8>>,
    /// The insert chains the playing score has, and the number of the list:
    /// see [`LiveScalarDevice::set_named_insert_chains`]. The number is odd
    /// while the producer writes the list, and 0 before the first list.
    insert_named: Arc<Vec<AtomicU64>>,
    insert_named_list: Arc<AtomicU64>,
    /// The audio input's frames, for `s("in")` voices.
    input: Arc<crate::input::InputRing>,
}

/// Interleaved stereo samples of the final mix, callback-writer and
/// one-reader, on their way to a file. ~5.4 s at 48 kHz.
const RECORD_RING_CAPACITY: usize = 1 << 19;
const RECORD_OPEN: u64 = 1;
const RECORD_IN_FLIGHT: u64 = 2;
const RECORD_CLAIMED: u64 = 4;
const RECORD_FLAGS: u64 = RECORD_OPEN | RECORD_IN_FLIGHT | RECORD_CLAIMED;
const RECORD_EPOCH_SHIFT: u32 = 3;
const MAX_RECORD_EPOCH: u64 = u64::MAX >> RECORD_EPOCH_SHIFT;

/// A recording tap could not begin or service the requested capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordCaptureError {
    Busy,
    EpochExhausted,
    StaleCapture,
}

impl std::fmt::Display for RecordCaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "the recording tap is still in use",
            Self::EpochExhausted => "recording capture epochs are exhausted",
            Self::StaleCapture => "the recording capture no longer owns this tap",
        })
    }
}

impl std::error::Error for RecordCaptureError {}

/// Sole reader of one tap epoch, retained independently of the output device.
///
/// Request closure, poll `is_closed`, then drain the final samples and loss
/// count before dropping this handle. Closing does not itself release ownership
/// or reset unread PCM. Dropping without a final drain discards that opportunity.
pub struct RecordCapture {
    tap: Arc<RecordTap>,
    epoch: u64,
}

struct RecordWriteGuard<'a> {
    tap: &'a RecordTap,
}

/// The final mix, post-fader, queued for a recorder off the audio thread.
///
/// The callback never waits: when the reader has fallen more than the ring
/// holds behind, the block is dropped whole and counted, so a stalled disk
/// costs the take a gap rather than the audience a glitch. Disabled it costs
/// one relaxed load per block.
pub struct RecordTap {
    slots: Box<[std::sync::atomic::AtomicU32]>,
    state: AtomicU64,
    written: std::sync::atomic::AtomicUsize,
    read: std::sync::atomic::AtomicUsize,
    dropped_frames: AtomicU64,
}

/// Single-callback-writer, producer-reader view of a stereo mix before master gain.
///
/// The high half of each slot is the lap number and the low half is one mono
/// `f32`. Keeping both in one atomic prevents a reader from pairing an old tag
/// with newly overwritten sample bits. Only complete callback blocks are
/// published through `written`, and the callback never waits for a reader.
struct LiveAnalysisTap {
    slots: [AtomicU64; LIVE_ANALYSIS_RING_CAPACITY],
    /// Left and right of the same frames, packed high and low, for the
    /// master alone: a stereo picture wants the sides the mono window
    /// folds together. A visual tap has none.
    sides: Option<Box<[AtomicU64; LIVE_ANALYSIS_RING_CAPACITY]>>,
    enabled: AtomicBool,
    /// Even while quiescent and odd while the callback publishes a block.
    /// This lets the producer bind PCM and its absolute end frame without an
    /// unbounded retry loop.
    publication: AtomicU64,
    written: AtomicU64,
    valid_from: AtomicU64,
    end_frame: AtomicU64,
    epoch: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveAnalysisSnapshot {
    pub stream_id: u64,
    pub epoch: u64,
    /// Absolute, exclusive output-frame frontier (one past the final sample).
    pub end_frame: u64,
    pub sample_rate: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnalysisTapSnapshot {
    epoch: u64,
    end_frame: u64,
}

/// The most output channels the routed fan-out addresses.
const MAX_OUTPUT_CHANNELS: usize = 64;

/// 48 kHz, as most hardware; 256 frames, as the live default.
const SILENT_SAMPLE_RATE: u32 = 48_000;
const SILENT_BUFFER_FRAMES: u32 = 256;

struct LiveCallbackMeters {
    master: crate::meter::MasterMeter,
    master_gain: Option<crate::live_control::Ramp>,
    load: RealtimeLoadMeter,
    pressure: RealtimePressureMeter,
    sample_rate: u32,
    /// By value, not boxed: its rings are fixed arrays, so holding it here
    /// keeps the whole callback path free of any allocation at all - which
    /// the analysis-tap tripwire tests measure.
    limiter: crate::limiter::live::LiveLimiter,
}

const CALLBACK_PENDING: u8 = 0;
const CALLBACK_ACTIVE: u8 = 1;
const CALLBACK_FAILED: u8 = 2;
const CALLBACK_RETIRED: u8 = 3;

/// Stream-local admission, separate from the shared musical Stop request.
/// Pending callbacks must not consume assets or change the running clock.
struct CallbackGate {
    state: AtomicU8,
    errors_in_flight: AtomicUsize,
}

struct CallbackErrorGuard<'a>(&'a AtomicUsize);

enum OutputHandle {
    Native(cpal::Stream),
    Silent(SilentHandle),
}

/// Owns only the candidate stream, not the device or its shared Stop state.
/// Dropping an unactivated candidate closes it without stopping another output.
struct PreparedLiveOutput {
    handle: OutputHandle,
    facts: AudioOutputFacts,
    gate: Arc<CallbackGate>,
    tripwire_baseline: tripwire::Violations,
    /// See [`LiveScalarDevice::memory`]: read here, the last moment the
    /// backend is not the callback's.
    backend_bytes: usize,
}

/// The silent output's handle: its stop flag and the thread running it.
struct SilentHandle {
    close: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    /// Test-only clock hold, one of the `SILENT_CLOCK_*` states.
    #[cfg(feature = "test-support")]
    clock_hold: Arc<std::sync::atomic::AtomicU8>,
}

/// The silent thread delivers blocks as wall time passes.
#[cfg(feature = "test-support")]
const SILENT_CLOCK_RUNNING: u8 = 0;
/// A test asked the silent thread to stop delivering; not yet honoured.
#[cfg(feature = "test-support")]
const SILENT_CLOCK_HOLD_REQUESTED: u8 = 1;
/// The silent thread has stopped between blocks: the clock is frozen.
#[cfg(feature = "test-support")]
const SILENT_CLOCK_HELD: u8 = 2;

/// A silent device whose real callback runs only when explicitly rendered.
/// The device stays owned here so rendering cannot race its teardown.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub struct ManualLiveOutput {
    device: LiveScalarDevice,
    backend: LiveScalarBackend,
    meters: LiveCallbackMeters,
    gate: Arc<CallbackGate>,
    _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

/// One listed audio device, with optional default stream details.
pub struct AudioDeviceInfo {
    /// The stable backend identifier: what selection is keyed on.
    pub id: String,
    /// The friendly name for display, disambiguated with the id when two
    /// devices share a name.
    pub name: String,
    pub is_default: bool,
    /// The default rate, when the listing requests it and the device provides it.
    pub sample_rate: Option<u32>,
    /// The default channel count, when requested and available.
    pub channels: Option<u16>,
}

/// An audio input on its own stream, feeding a ring: what `s("in")` reads
/// when a device is live, and - on a ring of its own, owned by the
/// engine - what the meter reads while nothing plays, so a microphone
/// can be checked and its fader set before the music starts. Nothing
/// here depends on an output.
pub struct AudioInput {
    /// Kept for its lifetime: dropping it closes the input.
    _stream: cpal::Stream,
    facts: AudioInputFacts,
    ring: Arc<crate::input::InputRing>,
    /// Set by the stream's error callback: the device went, or the
    /// driver did. The owner opens it again.
    failed: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
enum AudioDeviceDetails {
    Metadata,
    DefaultConfig,
}

/// The output callback's MMCSS registration: `None` until the callback's
/// first call, then that call's answer, held for the stream's life.
///
/// CPAL requires the callback to be `Send`, and a registration holds a raw
/// Windows handle, which is not. The handle never actually crosses threads
/// in a registered state: the closure is moved to CPAL's render thread
/// while this is still `None`, the registration is made on that thread's
/// first call, and CPAL drops the closure on the same thread when the
/// stream ends, which is where the drop reverts it.
///
/// That last step is how CPAL is written, not something its API promises.
/// In cpal 0.18.2 (pinned with `=` in this crate's Cargo.toml) the WASAPI
/// host's `Stream::new_output` moves the data callback into the closure of
/// its `cpal_wasapi_out` thread, which runs `run_output` and drops the
/// callback when that returns, on that thread, even for a stream dropped
/// before its first callback. Only a thread that cannot be spawned drops
/// the closure where the stream was built, and then it never ran: `None`.
/// Upgrading cpal, or enabling another Windows host (cpal's `asio`
/// feature), changes who owns and drops the closure and requires checking
/// this again. As a backstop, a registration remembers the thread that made
/// it and a drop on any other thread skips the revert.
#[cfg(windows)]
struct CallbackProAudio(Option<Option<crate::mmcss::ProAudioThread>>);

#[cfg(test)]
pub(crate) mod tests {
    // The allocation counters are process-wide, so a positive-control canary
    // must not overlap another test's zero-delta assertion.
    static TRIPWIRE_ASSERTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn tripwire_assertion_guard() -> std::sync::MutexGuard<'static, ()> {
        TRIPWIRE_ASSERTION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Callback metering state for the direct `write_live_output` tests. The
    /// meter is per-stream in production; here each call gets a fresh one.
    pub(super) fn test_meter() -> LiveCallbackMeters {
        LiveCallbackMeters::new(48_000, &RealtimeLoadPublisher::new())
    }

    use super::*;

    pub(super) fn confirmation_offer(
        channel: &crate::confirmation::ConfirmationChannel,
        token: u64,
        generation: u64,
        start_frame: u64,
        end_frame: u64,
        converted: u32,
    ) -> crate::confirmation::WindowOffer {
        crate::confirmation::WindowOffer {
            key: crate::confirmation::ConfirmationKey {
                epoch: channel.epoch(),
                token,
            },
            generation,
            takeover_frame: start_frame,
            start_frame,
            end_frame,
            intended: converted,
            converted,
            skipped_loading: 0,
            refused: 0,
            external: 0,
        }
    }

    pub(super) fn confirmation_event(
        offer: crate::confirmation::WindowOffer,
        ordinal: u32,
        target_frame: u64,
    ) -> crate::QueuedAudioEvent {
        crate::QueuedAudioEvent {
            event: AudioEvent {
                onset_id: u64::from(ordinal) + 1,
                generation: offer.generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            },
            confirmation: Some(crate::confirmation::ConfirmationOnset {
                key: offer.key,
                ordinal,
            }),
            expected_sample_identity: None,
        }
    }

    pub(super) fn confirmation_backend(shared: &LiveShared, voices: usize) -> LiveScalarBackend {
        let mut backend = LiveScalarBackend::with_dispatch(48_000, voices, DspDispatch::portable())
            .expect("prepared backend");
        backend.set_confirmations(shared.confirmations.clone());
        backend
    }

    pub(super) fn kabel_event(program: rustel_kabel::Program, second: u64) -> crate::AudioEvent {
        crate::AudioEvent {
            onset_id: second,
            generation: 7,
            ui_visuals: 0,
            target_frame: second * 48_000,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 1.0,
            duration_secs: 0.1,
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
            controls: crate::OscillatorControls {
                kabel: Some(crate::KabelControls {
                    program: program.into(),
                    start_secs: second as f64,
                    gate_end_secs: second as f64 + 0.05,
                    end_secs: second as f64 + 0.1,
                }),
                ..crate::OscillatorControls::default()
            },
        }
    }

    pub(super) fn kabel_program() -> rustel_kabel::Program {
        rustel_kabel::Program::from_json(
            r#"{"nodes":[{"type":"n","value":0.25},{"type":"output","inputs":[0]}]}"#,
        )
        .expect("program")
    }
}
