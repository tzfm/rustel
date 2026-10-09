//! Opt-in CPAL output for finite and continuously scheduled scalar audio.
//!
//! Both paths keep scheduling, locks, syscalls and dynamic
//! allocation off the CPAL callback. The live path crosses a fixed-capacity
//! POD ring and filters replaced generations at the block boundary. Device
//! recovery after device loss and portable xrun reporting are not implemented
//! by this module.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

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

/// Translate an absolute frame coordinate without moving its point in time.
///
/// Round upward so a recycle never makes the producer clock run backwards;
/// the bounded forward adjustment is less than one frame at the new rate.
fn rebase_frame_rate(frame: u64, old_sample_rate: u32, new_sample_rate: u32) -> u64 {
    let old_sample_rate = u128::from(old_sample_rate.max(1));
    let scaled = u128::from(frame).saturating_mul(u128::from(new_sample_rate.max(1)));
    let rebased = scaled.saturating_add(old_sample_rate - 1) / old_sample_rate;
    u64::try_from(rebased).unwrap_or(u64::MAX)
}

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

/// The delay before retrying an audio input after an open or stream failure.
/// Studio and the live command use the same delay. Consecutive failures
/// increase the delay until a healthy stream is observed.
pub fn input_retry_delay(attempts: u64) -> Duration {
    let shift = attempts.saturating_sub(1).min(INPUT_RETRY_MAX_SHIFT) as u32;
    INPUT_RETRY_BASE.saturating_mul(1_u32 << shift)
}

/// Coalesces one producer turn's shared-orbit reverb requests. The final
/// scheduled parameters for each orbit are prepared once after the turn.
pub struct LiveReverbBatch {
    requested: [Option<crate::reverb::ReverbParams>; crate::scalar::MAX_ORBITS],
}

impl Default for LiveReverbBatch {
    fn default() -> Self {
        Self {
            requested: [None; crate::scalar::MAX_ORBITS],
        }
    }
}

impl LiveReverbBatch {
    fn request_orbit(&mut self, orbit: usize, params: crate::reverb::ReverbParams) {
        self.requested[orbit.min(crate::scalar::MAX_ORBITS - 1)] = Some(params);
    }

    /// Observe one event without constructing an orbit impulse response.
    /// Per-voice `.FX()` reverbs keep their distinct bounded cache.
    pub fn observe(&mut self, device: &LiveScalarDevice, event: &crate::AudioEvent) {
        if let Some(reverb) = event.controls.reverb {
            self.request_orbit(
                usize::from(event.controls.orbit),
                crate::reverb::ReverbParams {
                    ir: None,
                    size_secs: reverb.size_secs,
                    fade_secs: reverb.fade_secs,
                    lp_start_hz: reverb.lp_start_hz,
                    lp_end_hz: reverb.lp_end_hz,
                },
            );
        }
        for stage in event.controls.fx_stages.iter().flatten() {
            let Some(room) = stage.room else { continue };
            device.ensure_fx_reverb(crate::reverb::ReverbParams {
                ir: room.ir,
                size_secs: room.size_secs,
                fade_secs: room.fade_secs,
                lp_start_hz: room.lp_start_hz,
                lp_end_hz: room.lp_end_hz,
            });
        }
    }

    /// Prepare and ship at most one shared reverb per orbit.
    pub fn flush(&mut self, device: &LiveScalarDevice) {
        for (orbit, request) in self.requested.iter_mut().enumerate() {
            if let Some(params) = request.take() {
                device.ensure_reverb(orbit, params);
            }
        }
    }
}

/// One continuously running output fed by the scheduler through a
/// fixed-capacity SPSC ring. The query thread owns pushes and generation
/// changes; the audio callback owns DSP and pops.
pub struct LiveScalarDevice {
    stream: Option<cpal::Stream>,
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
/// CPAL does not expose a portable xrun counter here. Wall-clock timestamps
/// are also avoided because collecting them in the callback would violate its
/// real-time constraints.
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
    /// Largest gap between audio callbacks; beyond the buffer duration this
    /// is a host-side dropout (platform starvation, not ours).
    pub max_callback_gap_nanos: u64,
    /// The longest single callback's own render time.
    pub max_callback_busy_nanos: u64,
    /// Rolling callback work divided by the deadline implied by each block's
    /// own frame count and the stream's actual sample rate.
    pub realtime_load: crate::RealtimeLoadSnapshot,
    pub realtime_pressure: crate::RealtimePressureSnapshot,
    pub refused_voices: u64,
    pub callback_errors: u64,
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
    /// End of the current 128-frame DSP chunk, announced before rendering.
    /// Unlike `frames`, this includes in-flight work, not certified host
    /// copies. A completed host callback leaves both frontiers equal.
    rendering_until_frame: Arc<AtomicU64>,
    playback_origin_nanos: Arc<AtomicU64>,
    buffer_playback_nanos: Arc<AtomicU64>,
    playback_latency_nanos: Arc<AtomicU64>,
    max_playback_latency_nanos: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    callbacks: Arc<AtomicU64>,
    accepted_events: Arc<AtomicU64>,
    stale_events: Arc<AtomicU64>,
    late_events: Arc<AtomicU64>,
    /// Nanos of the previous callback entry (relative to `clock_base`); the
    /// max observed gap is the host-starvation signal (an audible dropout on
    /// most hosts once the gap exceeds the buffer duration).
    last_callback_nanos: Arc<AtomicU64>,
    max_callback_gap_nanos: Arc<AtomicU64>,
    /// The longest a single callback spent RENDERING, as opposed to the gap
    /// between callbacks. The gap is measured exit to exit, so it already
    /// contains this: a slow render and a preempted thread both widen it, and
    /// only this number tells them apart.
    max_callback_busy_nanos: Arc<AtomicU64>,
    realtime_load: Arc<RealtimeLoadPublisher>,
    realtime_pressure: Arc<RealtimePressurePublisher>,
    /// Monotonic origin for the callback-gap clock.
    clock_base: Instant,
    refused_voices: Arc<AtomicU64>,
    callback_errors: Arc<AtomicU64>,
    callback_scope_misses: Arc<AtomicU64>,
    cutover_race_blocks: Arc<AtomicU64>,
    stop_acknowledged: Arc<AtomicBool>,
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
    /// The audio input's frames, for `s("in")` voices.
    input: Arc<crate::input::InputRing>,
}

impl LiveShared {
    fn new(initial_generation: u64) -> Self {
        Self {
            ring: Arc::new(Ring::new(LIVE_RING_CAPACITY)),
            immediate: Arc::new(Ring::new(64)),
            live_controls: Arc::new(crate::assets::AssetRing::new()),
            max_polyphony: Arc::new(AtomicUsize::new(crate::MAX_POLYPHONY)),
            confirmations: crate::confirmation::ConfirmationChannel::new(),
            analysis: Arc::new(LiveAnalysisTap::with_sides()),
            visual_analysis: Arc::new(std::array::from_fn(|_| OnceLock::new())),
            visual_analysis_mask: Arc::new(AtomicU64::new(0)),
            visual_analysis_generation_floor: Arc::new(AtomicU64::new(initial_generation)),
            record: Arc::new(RecordTap::new()),
            orbit_peaks: Arc::new(
                (0..crate::scalar::MAX_ORBITS)
                    .map(|_| std::sync::atomic::AtomicU32::new(0))
                    .collect(),
            ),
            orbit_pairs: Arc::new(
                (0..crate::scalar::MAX_ORBITS)
                    .map(|_| std::sync::atomic::AtomicU8::new(0))
                    .collect(),
            ),
            orbit_gains: Arc::new(
                (0..crate::scalar::MAX_ORBITS)
                    .map(|_| std::sync::atomic::AtomicU32::new(1.0f32.to_bits()))
                    .collect(),
            ),
            input: Arc::new(crate::input::InputRing::new()),
            samples: Arc::new(crate::assets::SampleChannel::new()),
            generation: Arc::new(AtomicU64::new(initial_generation)),
            takeover_frame: Arc::new(AtomicU64::new(0)),
            takeover_cut: Arc::new(AtomicU64::new(0)),
            line_arm: Arc::new(AtomicU64::new(0)),
            cut_sounding: Arc::new(AtomicBool::new(false)),
            frames: Arc::new(AtomicU64::new(0)),
            rendering_until_frame: Arc::new(AtomicU64::new(0)),
            playback_origin_nanos: Arc::new(AtomicU64::new(UNSET_STREAM_INSTANT_NANOS)),
            buffer_playback_nanos: Arc::new(AtomicU64::new(0)),
            playback_latency_nanos: Arc::new(AtomicU64::new(0)),
            max_playback_latency_nanos: Arc::new(AtomicU64::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            callbacks: Arc::new(AtomicU64::new(0)),
            accepted_events: Arc::new(AtomicU64::new(0)),
            stale_events: Arc::new(AtomicU64::new(0)),
            late_events: Arc::new(AtomicU64::new(0)),
            last_callback_nanos: Arc::new(AtomicU64::new(0)),
            max_callback_gap_nanos: Arc::new(AtomicU64::new(0)),
            max_callback_busy_nanos: Arc::new(AtomicU64::new(0)),
            realtime_load: Arc::new(RealtimeLoadPublisher::new()),
            realtime_pressure: Arc::new(RealtimePressurePublisher::new()),
            clock_base: Instant::now(),
            refused_voices: Arc::new(AtomicU64::new(0)),
            callback_errors: Arc::new(AtomicU64::new(0)),
            callback_scope_misses: Arc::new(AtomicU64::new(0)),
            cutover_race_blocks: Arc::new(AtomicU64::new(0)),
            stop_acknowledged: Arc::new(AtomicBool::new(false)),
            meter: Arc::new(crate::meter::MasterMeterShared::new()),
            score_sources_active: Arc::new(AtomicBool::new(false)),
            score_peak: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    fn report(&self, stream_id: u64) -> LiveDeviceReport {
        LiveDeviceReport {
            stream_id,
            submitted_frames: self.clock_frames(),
            playhead_nanos: 0,
            buffer_playback_nanos: self.buffer_playback_nanos.load(Ordering::Acquire),
            playback_latency_nanos: self.playback_latency_nanos.load(Ordering::Acquire),
            max_playback_latency_nanos: self.max_playback_latency_nanos.load(Ordering::Acquire),
            generation: self.generation.load(Ordering::Acquire),
            callbacks: self.callbacks.load(Ordering::Acquire),
            accepted_events: self.accepted_events.load(Ordering::Acquire),
            stale_events_filtered: self.stale_events.load(Ordering::Acquire),
            late_events: self.late_events.load(Ordering::Acquire),
            max_callback_gap_nanos: self.max_callback_gap_nanos.load(Ordering::Acquire),
            max_callback_busy_nanos: self.max_callback_busy_nanos.load(Ordering::Acquire),
            realtime_load: self.realtime_load.try_snapshot().unwrap_or_default(),
            realtime_pressure: self.realtime_pressure.try_snapshot().unwrap_or_default(),
            refused_voices: self.refused_voices.load(Ordering::Acquire),
            callback_errors: self.callback_errors.load(Ordering::Acquire),
            callback_scope_misses: self.callback_scope_misses.load(Ordering::Acquire),
            callback_allocations: 0,
            callback_frees: 0,
            stop_acknowledged: self.stop_acknowledged.load(Ordering::Acquire),
            ring_refusals: self.ring.refused.load(Ordering::Acquire) as u64,
            ring_role_conflicts: self.ring.role_conflicts.load(Ordering::Acquire) as u64,
            ring_depth: self.ring.len() as u64,
            ring_capacity: self.ring.capacity() as u64,
            ring_peak_depth: self.ring.peak_depth.load(Ordering::Acquire) as u64,
            asset_queues: AssetQueuePressureSnapshot {
                capacity: self.samples.installs.capacity() as u64,
                sample_installs: self.samples.installs.len() as u64,
                sample_returns: self.samples.returns.len() as u64,
                orbit_reverb_installs: self.samples.reverb_installs.len() as u64,
                orbit_reverb_returns: self.samples.reverb_returns.len() as u64,
                fx_reverb_installs: self.samples.fx_reverb_installs.len() as u64,
                fx_reverb_returns: self.samples.fx_reverb_returns.len() as u64,
                fx_reverb_refusals: self.samples.fx_reverb_refusals.load(Ordering::Acquire),
                leaked: self.samples.leaked.load(Ordering::Acquire),
            },
            cutover_race_blocks: self.cutover_race_blocks.load(Ordering::Acquire),
        }
    }

    fn clock_frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    /// The four handshake words the callback reads, each under its own name.
    fn flip_atomics(&self) -> crate::LiveFlipAtomics<'_> {
        crate::LiveFlipAtomics {
            generation: &self.generation,
            takeover_frame: &self.takeover_frame,
            takeover_cut: &self.takeover_cut,
            line_arm: &self.line_arm,
        }
    }

    fn render_frontier_frames(&self) -> u64 {
        self.clock_frames()
            .max(self.rendering_until_frame.load(Ordering::Acquire))
    }

    /// Move the shared absolute frame domain to a replacement stream's rate
    /// and clear telemetry whose origin belongs to the discarded CPAL stream.
    /// The old callback is joined and the replacement remains gated.
    fn prepare_recycled_stream(&self, old_sample_rate: u32, new_sample_rate: u32) {
        if old_sample_rate != new_sample_rate {
            self.record.close_current();
            while let Some(install) = self.samples.reverb_installs.pop() {
                // SAFETY: neither callback can consume the queue. Popping
                // transfers this pending allocation to the producer.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
            while let Some(install) = self.samples.fx_reverb_installs.pop() {
                // SAFETY: the same exclusive handoff applies to stage IRs.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
        }
        let frames = rebase_frame_rate(
            self.frames.load(Ordering::Acquire),
            old_sample_rate,
            new_sample_rate,
        );
        let takeover_frame = rebase_frame_rate(
            self.takeover_frame.load(Ordering::Acquire),
            old_sample_rate,
            new_sample_rate,
        );
        self.takeover_frame.store(takeover_frame, Ordering::Release);
        self.takeover_cut
            .store(TakeoverCut::None as u64, Ordering::Release);
        // A launch's pre-armed line lives in the same frame domain: left in
        // the old rate's frames, a countdown across a rate change would cut
        // early or late by the ratio. Only an armed word carries a frame;
        // zero and the withdrawal marker mean the same at any rate.
        let line_arm = self.line_arm.load(Ordering::Acquire);
        if line_arm & 0b01 != 0 {
            let line = rebase_frame_rate(line_arm >> 2, old_sample_rate, new_sample_rate);
            self.line_arm
                .store((line << 2) | (line_arm & 0b11), Ordering::Release);
        }
        self.frames.store(frames, Ordering::Release);
        // The old callback has joined: there is no remaining in-flight
        // reservation to carry into the new frame domain.
        self.rendering_until_frame.store(frames, Ordering::Release);
        self.playback_origin_nanos
            .store(UNSET_STREAM_INSTANT_NANOS, Ordering::Release);
        self.buffer_playback_nanos.store(0, Ordering::Release);
        self.playback_latency_nanos.store(0, Ordering::Release);
        self.last_callback_nanos.store(0, Ordering::Release);
        self.realtime_load.reset();
        self.realtime_pressure.reset();
    }

    fn reset_analysis(&self) {
        self.analysis.reset();
        for tap in self.visual_analysis.iter().filter_map(OnceLock::get) {
            tap.reset();
        }
    }

    fn visual_analysis_tap(&self, slot: usize) -> Option<&LiveAnalysisTap> {
        self.visual_analysis.get(slot)?.get().map(Box::as_ref)
    }

    fn ensure_visual_analysis_tap(&self, slot: usize) -> Option<&LiveAnalysisTap> {
        Some(
            self.visual_analysis
                .get(slot)?
                .get_or_init(|| Box::new(LiveAnalysisTap::new()))
                .as_ref(),
        )
    }

    fn set_visual_analysis_mask(&self, mask: u64) {
        let previous = self.visual_analysis_mask.load(Ordering::Acquire);
        let mut removed = previous & !mask;
        while removed != 0 {
            let slot = removed.trailing_zeros() as usize;
            if let Some(tap) = self.visual_analysis_tap(slot) {
                tap.set_enabled(false);
            }
            removed &= removed - 1;
        }
        let mut added = mask & !previous;
        while added != 0 {
            let slot = added.trailing_zeros() as usize;
            if let Some(tap) = self.ensure_visual_analysis_tap(slot) {
                tap.set_enabled(true);
            }
            added &= added - 1;
        }
        self.visual_analysis_mask.store(mask, Ordering::Release);
    }

    fn reset_visual_analysis_mask(&self, mask: u64) {
        self.visual_analysis_mask.store(0, Ordering::Release);
        self.visual_analysis_generation_floor
            .store(self.generation.load(Ordering::Acquire), Ordering::Release);
        for tap in self.visual_analysis.iter().filter_map(OnceLock::get) {
            tap.set_enabled(false);
        }
        let mut slots = mask;
        while slots != 0 {
            let slot = slots.trailing_zeros() as usize;
            if let Some(tap) = self.ensure_visual_analysis_tap(slot) {
                tap.set_enabled(true);
            }
            slots &= slots - 1;
        }
        self.visual_analysis_mask.store(mask, Ordering::Release);
    }

    fn playhead_at(&self, now: StreamInstant) -> u64 {
        let origin = self.playback_origin_nanos.load(Ordering::Acquire);
        if origin == UNSET_STREAM_INSTANT_NANOS {
            return 0;
        }
        let now = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
        now.saturating_sub(origin)
    }
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

impl std::fmt::Debug for RecordCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordCapture")
            .field("epoch", &self.epoch)
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl RecordCapture {
    fn state(&self) -> Result<u64, RecordCaptureError> {
        let state = self.tap.state.load(Ordering::Acquire);
        if state >> RECORD_EPOCH_SHIFT != self.epoch || state & RECORD_CLAIMED == 0 {
            return Err(RecordCaptureError::StaleCapture);
        }
        Ok(state)
    }

    /// Stop admitting blocks without waiting for an already admitted writer.
    pub fn request_close(&self) -> Result<(), RecordCaptureError> {
        self.state()?;
        // A live handle retains CLAIMED, so its epoch cannot change here.
        self.tap.close_current();
        Ok(())
    }

    /// True only after this epoch stopped admission and its writer retired.
    /// No further callback invocation is needed when no writer was admitted.
    pub fn is_closed(&self) -> bool {
        self.state()
            .is_ok_and(|state| state & (RECORD_OPEN | RECORD_IN_FLIGHT) == 0)
    }

    /// Append the published stereo sample prefix, including after closure.
    /// The return value counts samples, not frames. This does not locate gaps.
    pub fn drain(&mut self, out: &mut Vec<f32>) -> Result<usize, RecordCaptureError> {
        self.state()?;
        Ok(self.tap.drain_owned(out))
    }

    /// Cumulative whole-block losses in stereo frames for this capture.
    pub fn dropped_frames(&self) -> Result<u64, RecordCaptureError> {
        self.state()?;
        Ok(self.tap.dropped_frames())
    }
}

impl Drop for RecordCapture {
    fn drop(&mut self) {
        if self.state().is_ok() {
            // An admitted writer still retains IN_FLIGHT until its guard drops.
            self.tap
                .state
                .fetch_and(!(RECORD_OPEN | RECORD_CLAIMED), Ordering::AcqRel);
        }
    }
}

struct RecordWriteGuard<'a> {
    tap: &'a RecordTap,
}

impl Drop for RecordWriteGuard<'_> {
    fn drop(&mut self) {
        self.tap
            .state
            .fetch_and(!RECORD_IN_FLIGHT, Ordering::Release);
    }
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

impl RecordTap {
    fn new() -> Self {
        Self {
            slots: crate::input::zeroed_atomics(RECORD_RING_CAPACITY),
            state: AtomicU64::new(0),
            written: std::sync::atomic::AtomicUsize::new(0),
            read: std::sync::atomic::AtomicUsize::new(0),
            dropped_frames: AtomicU64::new(0),
        }
    }

    fn open(&self, claimed: bool) -> Result<u64, RecordCaptureError> {
        let previous = self.state.load(Ordering::Acquire);
        if previous & RECORD_FLAGS != 0 {
            return Err(RecordCaptureError::Busy);
        }
        let epoch = (previous >> RECORD_EPOCH_SHIFT)
            .checked_add(1)
            .filter(|epoch| *epoch <= MAX_RECORD_EPOCH)
            .ok_or(RecordCaptureError::EpochExhausted)?;
        let reserved = (epoch << RECORD_EPOCH_SHIFT) | RECORD_CLAIMED;
        self.state
            .compare_exchange(previous, reserved, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RecordCaptureError::Busy)?;
        self.read
            .store(self.written.load(Ordering::Acquire), Ordering::Release);
        self.dropped_frames.store(0, Ordering::Relaxed);
        let owner = if claimed { RECORD_CLAIMED } else { 0 };
        self.state.store(
            (epoch << RECORD_EPOCH_SHIFT) | owner | RECORD_OPEN,
            Ordering::Release,
        );
        Ok(epoch)
    }

    fn capture(self: &Arc<Self>) -> Result<RecordCapture, RecordCaptureError> {
        let epoch = self.open(true)?;
        Ok(RecordCapture {
            tap: Arc::clone(self),
            epoch,
        })
    }

    fn close_current(&self) {
        self.state.fetch_and(!RECORD_OPEN, Ordering::AcqRel);
    }

    /// Legacy control cannot replace a claimed capture or reset an active writer.
    fn set_enabled(&self, enabled: bool) -> Result<(), RecordCaptureError> {
        let state = self.state.load(Ordering::Acquire);
        if state & RECORD_CLAIMED != 0 {
            return Err(RecordCaptureError::Busy);
        }
        if enabled {
            if state & RECORD_OPEN == 0 {
                self.open(false)?;
            }
        } else {
            self.state
                .compare_exchange(
                    state,
                    state & !RECORD_OPEN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| RecordCaptureError::Busy)?;
        }
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.state.load(Ordering::Acquire) & RECORD_OPEN != 0
    }

    fn admit(&self, observed: u64) -> Option<RecordWriteGuard<'_>> {
        if observed & RECORD_OPEN == 0 || observed & RECORD_IN_FLIGHT != 0 {
            return None;
        }
        self.state
            .compare_exchange(
                observed,
                observed | RECORD_IN_FLIGHT,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()?;
        Some(RecordWriteGuard { tap: self })
    }

    /// Callback side. Whole blocks or nothing, never a wait.
    fn write_stereo(&self, stereo: &[f32]) {
        let observed = self.state.load(Ordering::Relaxed);
        if observed & RECORD_OPEN == 0 {
            return;
        }
        let Some(admission) = self.admit(observed) else {
            return;
        };
        admission.tap.write_admitted(stereo);
    }

    fn write_admitted(&self, stereo: &[f32]) {
        let written = self.written.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        let free = RECORD_RING_CAPACITY - written.wrapping_sub(read);
        if stereo.len() > free {
            self.dropped_frames
                .fetch_add((stereo.len() / 2) as u64, Ordering::Relaxed);
            return;
        }
        for (index, sample) in stereo.iter().enumerate() {
            self.slots[written.wrapping_add(index) % RECORD_RING_CAPACITY]
                .store(sample.to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(written.wrapping_add(stereo.len()), Ordering::Release);
    }

    /// Reader side: append everything written since the last drain.
    fn drain(&self, out: &mut Vec<f32>) -> usize {
        if self.state.load(Ordering::Acquire) & RECORD_CLAIMED != 0 {
            return 0;
        }
        self.drain_owned(out)
    }

    fn drain_owned(&self, out: &mut Vec<f32>) -> usize {
        let read = self.read.load(Ordering::Relaxed);
        let written = self.written.load(Ordering::Acquire);
        let available = written.wrapping_sub(read);
        out.reserve(available);
        for index in 0..available {
            out.push(f32::from_bits(
                self.slots[read.wrapping_add(index) % RECORD_RING_CAPACITY].load(Ordering::Relaxed),
            ));
        }
        self.read
            .store(read.wrapping_add(available), Ordering::Release);
        available
    }

    fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// Bytes of the ring takes have reached: zeroed memory, resident only
    /// once written, and all of it once a take outlasts the ring.
    fn touched_bytes(&self) -> usize {
        self.written
            .load(Ordering::Relaxed)
            .min(RECORD_RING_CAPACITY)
            * std::mem::size_of::<std::sync::atomic::AtomicU32>()
    }
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

impl LiveAnalysisTap {
    fn new() -> Self {
        Self {
            slots: [const { AtomicU64::new(0) }; LIVE_ANALYSIS_RING_CAPACITY],
            sides: None,
            enabled: AtomicBool::new(false),
            publication: AtomicU64::new(0),
            written: AtomicU64::new(0),
            valid_from: AtomicU64::new(0),
            end_frame: AtomicU64::new(0),
            epoch: AtomicU64::new(1),
        }
    }

    /// The master's tap, which keeps left and right as well.
    fn with_sides() -> Self {
        let mut tap = Self::new();
        tap.sides = Some(Box::new(
            [const { AtomicU64::new(0) }; LIVE_ANALYSIS_RING_CAPACITY],
        ));
        tap
    }

    fn set_enabled(&self, enabled: bool) {
        if !enabled {
            self.enabled.store(false, Ordering::Release);
            return;
        }
        if self.enabled.load(Ordering::Acquire) {
            return;
        }
        // Begin a fresh capture epoch without clearing slots concurrently
        // with the callback. `valid_from` excludes every pre-enable sample;
        // an in-flight writer from the prior epoch declines to publish.
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.valid_from
            .store(self.written.load(Ordering::Acquire), Ordering::Release);
        self.enabled.store(true, Ordering::Release);
    }

    /// Publish interleaved stereo scratch as mono. Callback-only: fixed
    /// atomics and arithmetic, with no allocation, lock, or syscall.
    fn write_stereo(&self, stereo: &[f32], end_frame: u64) {
        self.write_stereo_scaled(stereo, end_frame, 1.0);
    }

    fn write_stereo_scaled(&self, stereo: &[f32], end_frame: u64, gain: f32) {
        // Visual telemetry is explicitly opt-in. Ordinary playback pays one
        // predictable branch per 128-frame DSP block and no analysis stores.
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        let frames = stereo.len() / 2;
        let publication = self.publication.load(Ordering::Relaxed) & !1;
        self.publication
            .store(publication.wrapping_add(1), Ordering::Release);
        let start = self.written.load(Ordering::Relaxed);
        for frame in 0..frames {
            let sequence = start.wrapping_add(frame as u64);
            let slot = sequence as usize % LIVE_ANALYSIS_RING_CAPACITY;
            let lap = sequence / LIVE_ANALYSIS_RING_CAPACITY as u64;
            if let Some(sides) = &self.sides {
                let left = stereo[frame * 2] * gain;
                let right = stereo[frame * 2 + 1] * gain;
                sides[slot].store(
                    (u64::from(left.to_bits()) << 32) | u64::from(right.to_bits()),
                    Ordering::Relaxed,
                );
            }
            let mono = (stereo[frame * 2] + stereo[frame * 2 + 1]) * (0.5 * gain);
            let packed = ((lap.wrapping_add(1) as u32 as u64) << 32) | u64::from(mono.to_bits());
            self.slots[slot].store(packed, Ordering::Release);
        }
        if self.enabled.load(Ordering::Acquire) && self.epoch.load(Ordering::Acquire) == epoch {
            self.written
                .store(start.wrapping_add(frames as u64), Ordering::Relaxed);
            self.end_frame.store(end_frame, Ordering::Relaxed);
        }
        self.publication
            .store(publication.wrapping_add(2), Ordering::Release);
    }

    /// Copy the newest frames as left and right, oldest first, for a
    /// stereo picture. Unlike [`Self::copy_latest`] this reader checks no
    /// laps: a frame the callback overwrites mid-copy is a newer frame in
    /// the same place, which a scope shows as well as the old one. The
    /// prefix is silence before the ring fills. False when the tap keeps
    /// no sides or has nothing yet.
    fn copy_latest_sides(&self, output: &mut [(f32, f32); LIVE_ANALYSIS_SIDES_SAMPLES]) -> bool {
        let Some(sides) = &self.sides else {
            return false;
        };
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }
        let end = self.written.load(Ordering::Acquire);
        let valid_from = self.valid_from.load(Ordering::Acquire);
        let available = end
            .saturating_sub(valid_from)
            .min(LIVE_ANALYSIS_SIDES_SAMPLES as u64) as usize;
        if available == 0 {
            return false;
        }
        let padding = LIVE_ANALYSIS_SIDES_SAMPLES - available;
        output[..padding].fill((0.0, 0.0));
        let start = end.wrapping_sub(available as u64);
        for (offset, frame) in output[padding..].iter_mut().enumerate() {
            let slot = start.wrapping_add(offset as u64) as usize % LIVE_ANALYSIS_RING_CAPACITY;
            let packed = sides[slot].load(Ordering::Relaxed);
            *frame = (
                f32::from_bits((packed >> 32) as u32),
                f32::from_bits(packed as u32),
            );
        }
        true
    }

    /// Copy the newest complete window in chronological order. Before the
    /// window fills, the missing prefix is silence. A reader makes only a few
    /// attempts if the callback laps a required slot; the callback itself
    /// remains wait-free.
    fn copy_latest(
        &self,
        output: &mut [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    ) -> Option<AnalysisTapSnapshot> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        // Never let editor telemetry monopolise the scheduling producer if a
        // very small callback buffer repeatedly laps this reader.
        for _ in 0..3 {
            let publication = self.publication.load(Ordering::Acquire);
            if publication & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let end = self.written.load(Ordering::Acquire);
            let valid_from = self.valid_from.load(Ordering::Acquire);
            let end_frame = self.end_frame.load(Ordering::Relaxed);
            let epoch = self.epoch.load(Ordering::Relaxed);
            let available = end
                .saturating_sub(valid_from)
                .min(LIVE_ANALYSIS_WINDOW_SAMPLES as u64) as usize;
            if available == 0 {
                return None;
            }
            let padding = LIVE_ANALYSIS_WINDOW_SAMPLES - available;
            output[..padding].fill(0.0);
            let start = end.wrapping_sub(available as u64);
            let mut complete = true;
            for offset in 0..available {
                let sequence = start.wrapping_add(offset as u64);
                let slot = sequence as usize % LIVE_ANALYSIS_RING_CAPACITY;
                let expected_lap =
                    (sequence / LIVE_ANALYSIS_RING_CAPACITY as u64).wrapping_add(1) as u32;
                let packed = self.slots[slot].load(Ordering::Acquire);
                if (packed >> 32) as u32 != expected_lap {
                    complete = false;
                    break;
                }
                output[padding + offset] = f32::from_bits(packed as u32);
            }
            if complete && self.publication.load(Ordering::Acquire) == publication {
                return Some(AnalysisTapSnapshot { epoch, end_frame });
            }
            std::hint::spin_loop();
        }
        None
    }

    fn reset(&self) {
        for slot in &self.slots {
            slot.store(0, Ordering::Relaxed);
        }
        self.written.store(0, Ordering::Release);
        self.valid_from.store(0, Ordering::Release);
        self.end_frame.store(0, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

impl LiveScalarDevice {
    /// Deliver a decoded sample to the callback's bank. Reclaims returned
    /// (displaced) samples first, so a full drain can never overflow the
    /// return ring; a full INSTALL ring hands the sample back for retry.
    pub fn install_sample(
        &self,
        id: crate::sample::SampleId,
        sample: crate::sample::DecodedSample,
    ) -> Result<(), crate::sample::DecodedSample> {
        self.reclaim_assets();
        let pointer = Box::into_raw(Box::new(sample));
        match self
            .shared
            .samples
            .installs
            .push(crate::assets::SampleInstall {
                id,
                sample: pointer,
            }) {
            Ok(()) => Ok(()),
            Err(install) => {
                // SAFETY: never left this thread; unique owner.
                Err(*unsafe { Box::from_raw(install.sample) })
            }
        }
    }

    /// Empty a live slot. A null install is the uninstall baton; the
    /// callback clears the bank and returns whatever it displaced. The
    /// bundled `bd` is refused here so a policy cannot silence the
    /// fallback hit.
    pub fn uninstall_sample(&self, id: crate::sample::SampleId) -> bool {
        if id == crate::sample::BUNDLED_BD_SAMPLE_ID {
            return true;
        }
        self.reclaim_assets();
        self.shared
            .samples
            .installs
            .push(crate::assets::SampleInstall {
                id,
                sample: std::ptr::null_mut(),
            })
            .is_ok()
    }

    /// Free any samples the callback displaced. Call only from the single
    /// producer thread that also drives `install_sample` / `ensure_reverb`;
    /// the return rings are SPSC.
    pub fn reclaim_samples(&self) {
        self.reclaim_assets();
    }

    /// Make sure `orbit`'s reverb matches `params`, generating and shipping
    /// a new one when it does not. Producer-side and potentially SLOW (IR
    /// synthesis + FFT partitioning, tens of ms) - the schedule lead absorbs
    /// it, and a repeat call with unchanged params is a cache hit.
    pub fn ensure_reverb(&self, orbit: usize, params: crate::reverb::ReverbParams) {
        let orbit = orbit.min(crate::scalar::MAX_ORBITS - 1);
        let mut cache = self.reverb_cache.lock().expect("producer reverb cache");
        if cache[orbit].is_some_and(|(cached, _)| cached == params) {
            return;
        }
        self.reclaim_assets();
        let reverb = Box::new(crate::reverb::OrbitReverb::generate_with_dispatch(
            self.sample_rate,
            params,
            self.dispatch(),
        ));
        let bytes = reverb.approx_bytes();
        let pointer = Box::into_raw(reverb);
        match self
            .shared
            .samples
            .reverb_installs
            .push(crate::assets::ReverbInstall {
                orbit: orbit as u8,
                reverb: pointer,
            }) {
            Ok(()) => {
                // The orbit's previous reverb goes back to be freed, so
                // this one takes its place in the count.
                cache[orbit] = Some((params, bytes));
            }
            Err(install) => {
                // Ring full: free and retry on a later batch.
                // SAFETY: never left this thread; unique owner.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
        }
    }

    /// Generate a `.FX()` stage reverb off the audio thread and ship it into
    /// the callback's stage pool. The eight most recent fingerprints avoid
    /// duplicate synthesis; older ones can be regenerated after eviction.
    pub fn ensure_fx_reverb(&self, params: crate::reverb::ReverbParams) {
        self.reclaim_assets();
        let mut cache = self
            .fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache");
        if let Some(index) = cache.iter().position(|(cached, _, _)| cached == &params) {
            let hit = cache.remove(index);
            cache.push(hit);
            return;
        }
        let reverb = Box::new(
            crate::reverb::OrbitReverb::generate_streaming_with_dispatch(
                self.sample_rate,
                crate::reverb::ReverbParams { ir: None, ..params },
                self.dispatch(),
            ),
        );
        let bytes = reverb.approx_bytes();
        if bytes > crate::scalar::MAX_FX_REVERB_BYTES {
            // The callback refuses a room above its byte budget. Keep the
            // fingerprint, or each later event generates the room again.
            self.shared
                .samples
                .fx_reverb_refusals
                .fetch_add(1, Ordering::Relaxed);
            if cache.len() >= 8 {
                cache.remove(0);
            }
            cache.push((params, 0, 0));
            return;
        }
        let pointer = Box::into_raw(reverb);
        match self
            .shared
            .samples
            .fx_reverb_installs
            .push(crate::assets::FxReverbInstall { reverb: pointer })
        {
            Ok(()) => {
                if cache.len() >= 8 {
                    cache.remove(0);
                }
                cache.push((params, bytes, pointer as usize));
            }
            Err(install) => {
                self.shared
                    .samples
                    .fx_reverb_refusals
                    .fetch_add(1, Ordering::Relaxed);
                // SAFETY: never left this thread; unique owner.
                drop(unsafe { Box::from_raw(install.reverb) });
            }
        }
    }

    fn reclaim_assets(&self) {
        let mut cache = self
            .fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache");
        self.shared.samples.reclaim_with_fx(|returned| {
            cache.retain(|(_, _, installed)| *installed != returned as usize);
        });
    }

    pub fn start_default(initial_generation: u64) -> Result<Self, DevicePlaybackError> {
        Self::start_output(None, initial_generation)
    }

    /// Open live output on a named device, or the host default when
    /// `selector` is `None`.
    pub fn start_output(
        selector: Option<&str>,
        initial_generation: u64,
    ) -> Result<Self, DevicePlaybackError> {
        Self::start_output_with_options(selector, initial_generation, LiveOutputOptions::default())
    }

    /// Open live output with engine-owned kernels selected before preparation.
    /// The same selection is used for producer-prepared assets and recycled
    /// streams. FFT planning retains its independent automatic policy.
    /// Sample interpolation is set before playback and retained on replacement.
    /// Invalid buffer requests are rejected before probing a device or
    /// allocating the backend; `Auto` retains the platform/environment policy.
    pub fn start_output_with_options(
        selector: Option<&str>,
        initial_generation: u64,
        options: LiveOutputOptions,
    ) -> Result<Self, DevicePlaybackError> {
        options
            .buffer_preference()
            .validate()
            .map_err(buffer_preference_error)?;
        if selector == Some(SILENT_OUTPUT_NAME) {
            return Self::start_silent_with_options(
                SILENT_SAMPLE_RATE,
                initial_generation,
                options,
            );
        }
        let opened = ScalarDevice::open_output(selector)?;
        let shared = LiveShared::new(initial_generation);
        let prepared = PreparedLiveOutput::native(opened, shared.clone(), options, &[])?;
        Self::start_prepared(shared, options, prepared)
    }

    /// Live output that makes no sound: see [`SILENT_OUTPUT_NAME`].
    pub fn start_silent(
        sample_rate: u32,
        initial_generation: u64,
    ) -> Result<Self, DevicePlaybackError> {
        Self::start_silent_with_options(
            sample_rate,
            initial_generation,
            LiveOutputOptions::default(),
        )
    }

    /// Open silent output with the same kernel selection as an explicit
    /// offline or device-backed renderer and the requested callback period.
    /// Sample interpolation uses the mode supplied in `options`.
    /// Automatic silent output retains its fixed default buffer size.
    pub fn start_silent_with_options(
        sample_rate: u32,
        initial_generation: u64,
        options: LiveOutputOptions,
    ) -> Result<Self, DevicePlaybackError> {
        options
            .buffer_preference()
            .validate()
            .map_err(buffer_preference_error)?;
        let shared = LiveShared::new(initial_generation);
        let prepared = PreparedLiveOutput::silent(shared.clone(), sample_rate, options, &[])?;
        Self::start_prepared(shared, options, prepared)
    }

    fn start_prepared(
        shared: LiveShared,
        options: LiveOutputOptions,
        prepared: PreparedLiveOutput,
    ) -> Result<Self, DevicePlaybackError> {
        prepared.start_gated()?;
        let facts = &prepared.facts;
        let mut device = Self {
            stream: None,
            silent: None,
            input: None,
            reverb_cache: std::sync::Mutex::new(vec![None; crate::scalar::MAX_ORBITS]),
            fx_reverb_cache: std::sync::Mutex::new(Vec::with_capacity(8)),
            shared,
            sample_rate: facts.sample_rate_hz(),
            channels: facts.channels(),
            host_id: facts.host().id().map(Into::into),
            output_sample_format: facts.sample_format(),
            name: facts.device_id().to_owned(),
            stream_id: NEXT_LIVE_STREAM_ID.fetch_add(1, Ordering::Relaxed),
            options,
            requested_buffer_frames: facts.requested_buffer_frames(),
            reported_buffer_frames: facts.reported_buffer_frames(),
            device_period_frames: facts.device_period_frames(),
            tripwire_baseline: prepared.tripwire_baseline,
            tripwire_canaries: tripwire::Violations::default(),
            backend_bytes: 0,
        };
        device.activate_prepared(prepared)?;
        Ok(device)
    }

    fn activate_prepared(
        &mut self,
        prepared: PreparedLiveOutput,
    ) -> Result<(), DevicePlaybackError> {
        let backend_bytes = prepared.backend_bytes;
        match prepared.activate()? {
            OutputHandle::Native(stream) => self.stream = Some(stream),
            OutputHandle::Silent(silent) => self.silent = Some(silent),
        }
        self.backend_bytes = backend_bytes;
        Ok(())
    }

    pub fn is_silent(&self) -> bool {
        self.silent.is_some()
    }

    /// Whether an output handle is still owned. Unlike health, this separates
    /// an untouched but unhealthy stream from failed replacement after teardown.
    pub fn has_output(&self) -> bool {
        self.stream.is_some() || self.silent.is_some()
    }

    /// Post-fader master level, read and reset. See [`crate::meter`].
    pub fn take_levels(&self) -> crate::meter::MasterLevels {
        self.shared.meter.take()
    }

    /// Remaining score sources and largest orbit peak since the last take.
    /// Excludes the independent computer piano bus, including its held voices.
    pub fn take_score_activity(&self) -> (bool, f32) {
        (
            self.shared.score_sources_active.load(Ordering::Acquire),
            f32::from_bits(self.shared.score_peak.swap(0, Ordering::AcqRel)),
        )
    }

    /// The safety limiter's ceiling and character, or `None` for off. Off
    /// still refuses to hand the device a sample past full scale.
    pub fn set_limiter(&self, settings: Option<crate::meter::LimiterSettings>) {
        self.shared.meter.set_limiter(settings);
    }

    /// Whether the limiter's ceiling is brought back up to full scale.
    pub fn set_limiter_makeup(&self, on: bool) {
        self.shared.meter.set_limiter_makeup(on);
    }

    /// What the callback will read on its next block. The producer side
    /// needs this to tell "nobody set it" from "it was set and did
    /// nothing".
    pub fn limiter(&self) -> Option<crate::meter::LimiterSettings> {
        self.shared.meter.limiter_settings()
    }

    /// Set the linear master gain applied to the final mix.
    ///
    /// This is an engine control, not a score control: it multiplies the mix
    /// on its way to the device and never changes what the score evaluates to.
    pub fn set_master_gain(&self, gain: f32) {
        self.shared.meter.set_gain(gain);
    }

    /// Submit one exact continuous-control target. The sole device owner
    /// produces these POD messages; the callback drains them without locks.
    /// A full ring leaves the newest value with the engine for its next tick.
    pub fn try_set_live_control(&mut self, update: crate::live_control::LiveControlUpdate) -> bool {
        self.shared.live_controls.push(update).is_ok()
    }

    /// Requested voice limit, adopted by the callback at its next block edge.
    pub fn max_polyphony(&self) -> usize {
        self.shared.max_polyphony.load(Ordering::Relaxed)
    }

    /// Raising the limit permits more simultaneous sounds and more CPU work.
    /// Lowering it fades excess voices without restarting the stream.
    pub fn set_max_polyphony(&self, limit: usize) {
        self.shared.max_polyphony.store(
            limit.clamp(1, crate::MAX_CONFIGURABLE_POLYPHONY),
            Ordering::Relaxed,
        );
    }

    pub fn master_gain(&self) -> f32 {
        self.shared.meter.gain()
    }

    /// One orbit's fader, linear; 1 is unity. An engine control like the
    /// master: it scales the orbit on its way to the sum and never changes
    /// what the score evaluates to.
    pub fn set_orbit_gain(&self, orbit: usize, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 4.0)
        } else {
            1.0
        };
        if let Some(slot) = self.shared.orbit_gains.get(orbit) {
            slot.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn orbit_gain(&self, orbit: usize) -> f32 {
        self.shared
            .orbit_gains
            .get(orbit)
            .map_or(1.0, |slot| f32::from_bits(slot.load(Ordering::Relaxed)))
    }

    /// The audio input's fader, applied as the frames land.
    pub fn set_input_gain(&self, gain: f32) {
        self.shared.input.set_gain(gain);
    }

    pub fn input_gain(&self) -> f32 {
        self.shared.input.gain()
    }

    /// The lag a reader of the input is placed at, in frames: a fixed few
    /// milliseconds, or twice the largest delivery so far when the driver
    /// writes in bursts. This is the number a readout owes the player.
    pub fn input_read_lag_frames(&self) -> u64 {
        self.shared.input.read_lag()
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Enable or disable the mix telemetry tap before master gain and limiting.
    /// It is disabled by default so ordinary CLI playback performs no per-sample
    /// atomic stores.
    pub fn set_analysis_enabled(&self, enabled: bool) {
        self.shared.analysis.set_enabled(enabled);
    }

    pub fn set_visual_analysis_mask(&self, mask: u64) {
        self.shared.set_visual_analysis_mask(mask);
    }

    pub fn reset_visual_analysis_mask(&self, mask: u64) {
        self.shared.reset_visual_analysis_mask(mask);
    }

    /// Copy the newest mono samples before master gain and limiting, oldest to
    /// newest. A newly started stream is left-padded with silence until its first full window
    /// has been written. A bounded collision with the callback returns `None`
    /// so UI work can simply try again on its next frame.
    ///
    /// This is producer-side telemetry: it performs no callback work and
    /// allocates no storage because the caller owns the fixed output array.
    pub fn copy_analysis_window(
        &self,
        output: &mut [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    ) -> Option<LiveAnalysisSnapshot> {
        let snapshot = self.shared.analysis.copy_latest(output)?;
        Some(LiveAnalysisSnapshot {
            stream_id: self.stream_id,
            epoch: snapshot.epoch,
            end_frame: snapshot.end_frame,
            sample_rate: self.sample_rate,
        })
    }

    /// The newest frames of the mix as left and right, oldest first, for
    /// a stereo picture; false with nothing to show.
    pub fn copy_analysis_sides(
        &self,
        output: &mut [(f32, f32); LIVE_ANALYSIS_SIDES_SAMPLES],
    ) -> bool {
        self.shared.analysis.copy_latest_sides(output)
    }

    pub fn copy_visual_analysis_window(
        &self,
        slot: u8,
        output: &mut [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    ) -> Option<LiveAnalysisSnapshot> {
        let tap = self.shared.visual_analysis_tap(usize::from(slot))?;
        let snapshot = tap.copy_latest(output)?;
        Some(LiveAnalysisSnapshot {
            stream_id: self.stream_id,
            epoch: snapshot.epoch,
            end_frame: snapshot.end_frame,
            sample_rate: self.sample_rate,
        })
    }

    /// Open the audio input - the host default, or a named device - so
    /// `s("in")` voices have something to read. The input runs on its own
    /// stream beside the output (or beside the silent output); its frames
    /// go into a ring the backend reads a little behind.
    pub fn open_input(&mut self, selector: Option<&str>) -> Result<String, DevicePlaybackError> {
        self.input = None;
        let input = AudioInput::open(
            selector,
            Arc::clone(&self.shared.input),
            Some(self.sample_rate),
        )?;
        let name = input.name().to_owned();
        self.input = Some(input);
        Ok(name)
    }

    /// Close the audio input; `s("in")` voices read silence.
    pub fn close_input(&mut self) {
        self.input = None;
        self.shared.input.set_channels(0);
    }

    pub fn input_name(&self) -> Option<&str> {
        self.input.as_ref().map(AudioInput::name)
    }

    /// The ring the open input writes into - what `s("in")` reads, and
    /// what a recorded sample is drained from. `None` with no input open.
    pub fn input_ring(&self) -> Option<Arc<crate::input::InputRing>> {
        self.input.as_ref().map(|_| Arc::clone(&self.shared.input))
    }

    /// Whether the open input's stream has reported an error - a device
    /// unplugged, a driver gone - so the owner can open it again.
    pub fn input_failed(&self) -> bool {
        self.input.as_ref().is_some_and(AudioInput::failed)
    }

    /// Channels the open input delivers; zero without one.
    pub fn input_channels(&self) -> usize {
        self.shared.input.channels()
    }

    /// The loudest sample the input has delivered since the last take, for
    /// a meter; 0 while none is open or nothing has come in.
    pub fn take_input_peak(&self) -> f32 {
        self.shared.input.take_peak()
    }

    /// Each orbit's highest absolute sample, post-fader, since the last
    /// take: the orbit meters.
    pub fn take_orbit_levels(&self) -> [f32; crate::scalar::MAX_ORBITS] {
        let mut levels = [0.0f32; crate::scalar::MAX_ORBITS];
        for (level, slot) in levels.iter_mut().zip(self.shared.orbit_peaks.iter()) {
            *level = f32::from_bits(slot.swap(0, Ordering::AcqRel));
        }
        levels
    }

    /// Send an orbit to an output pair (0 is the main pair). Takes effect
    /// at the next block; a pair the device does not have is the main pair.
    pub fn set_orbit_output(&self, orbit: usize, pair: u8) {
        let orbit = orbit.min(crate::scalar::MAX_ORBITS - 1);
        self.shared.orbit_pairs[orbit].store(pair, Ordering::Relaxed);
    }

    pub fn orbit_outputs(&self) -> [u8; crate::scalar::MAX_ORBITS] {
        let mut pairs = [0u8; crate::scalar::MAX_ORBITS];
        for (slot, pair) in pairs.iter_mut().zip(self.shared.orbit_pairs.iter()) {
            *slot = pair.load(Ordering::Relaxed);
        }
        pairs
    }

    /// Stereo pairs the output has: 1 for a stereo device.
    pub fn output_pairs(&self) -> u16 {
        (self.channels / 2).max(1)
    }

    /// Claim a new recording epoch. A closed capture remains busy until its
    /// handle is dropped, leaving its final PCM available for that reader.
    pub fn start_recording_capture(&self) -> Result<RecordCapture, RecordCaptureError> {
        self.shared.record.capture()
    }

    /// Whether this output uses the tap retained by the capture. This does
    /// not claim the capture is open or that the sample rate is unchanged.
    pub fn owns_record_capture(&self, capture: &RecordCapture) -> bool {
        Arc::ptr_eq(&self.shared.record, &capture.tap)
    }

    /// Legacy recording control. Starting discards the prior unclaimed epoch;
    /// a new epoch returns Busy while a capture or retiring writer owns it.
    /// Repeated enabling is idempotent. Successful disabling does not wait for
    /// an admitted writer; use RecordCapture for a complete final drain.
    ///
    /// Legacy control and draining belong to one producer/reader and must not
    /// race each other or `start_recording_capture`.
    pub fn set_recording_enabled(&self, enabled: bool) -> Result<(), RecordCaptureError> {
        self.shared.record.set_enabled(enabled)
    }

    pub fn recording_enabled(&self) -> bool {
        self.shared.record.is_enabled()
    }

    /// Take everything the callback has queued since the last drain:
    /// interleaved stereo, post-fader, at [`Self::sample_rate`]. Producer
    /// side; never blocks the callback. Returns the number of f32 SAMPLES
    /// appended (two per stereo frame), not frames. Returns zero while a
    /// RecordCapture owns the tap; that handle is then its only reader.
    /// The legacy producer must serialize this with recording control/start.
    pub fn drain_recording(&self, out: &mut Vec<f32>) -> usize {
        self.shared.record.drain(out)
    }

    /// Stereo FRAMES the callback had to drop because the recorder fell
    /// behind (contrast the sample count from `drain_recording`).
    pub fn recording_dropped_frames(&self) -> u64 {
        self.shared.record.dropped_frames()
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub fn requested_buffer_frames(&self) -> u32 {
        self.requested_buffer_frames
    }

    /// Selection retained by this device and its producer-prepared assets.
    pub const fn dispatch(&self) -> DspDispatch {
        self.options.dispatch()
    }

    /// Original requests retained across stream recycling.
    pub const fn output_options(&self) -> LiveOutputOptions {
        self.options
    }

    /// Adopt a buffer preference for this and every later stream open,
    /// including the recycle path. The current callback keeps its size
    /// until the next open; the caller recycles to apply it at once.
    pub fn set_buffer_preference(&mut self, preference: AudioBufferPreference) {
        self.options = self.options.with_buffer_preference(preference);
    }

    /// Original preference, retained across automatic stream recycling.
    pub const fn buffer_preference(&self) -> AudioBufferPreference {
        self.options.buffer_preference()
    }

    /// Backend's best estimate of frames per callback, when supported.
    pub fn reported_buffer_frames(&self) -> Option<u32> {
        self.reported_buffer_frames
    }

    /// Stable facts used by every diagnostics projection.
    pub fn audio_facts(&self) -> AudioStreamFacts {
        let host = self
            .host_id
            .as_ref()
            .map_or(AudioHost::Silent, |id| AudioHost::cpal(id.clone()));
        let output = AudioOutputFacts::new(
            host,
            self.name.clone(),
            self.sample_rate,
            self.channels,
            self.output_sample_format,
            self.requested_buffer_frames,
            self.reported_buffer_frames,
        )
        .with_buffer_preference(self.buffer_preference())
        .with_device_period_frames(self.device_period_frames);
        AudioStreamFacts::new(
            output,
            self.input.as_ref().map(|input| input.facts().clone()),
        )
    }

    pub fn clock_seconds(&self) -> f64 {
        self.clock_nanos() as f64 / 1_000_000_000.0
    }

    /// Callbacks completed so far. Bumped as each one exits, after it has
    /// drained the install ring at its entry and rendered its block - so
    /// two more than the count read after a push proves one whole block
    /// began after the push, took the baton at its top, and rendered with
    /// the slot empty.
    pub fn callbacks(&self) -> u64 {
        self.shared.callbacks.load(Ordering::Acquire)
    }

    /// Exact render-frontier frame used by callback generation cutovers.
    ///
    /// Unlike converting [`Self::clock_nanos`] back into frames, this cannot
    /// lose one frame to two integer divisions at non-divisible sample rates.
    /// Zero before the first callback is deliberately conservative.
    pub fn clock_frames(&self) -> u64 {
        self.shared.clock_frames()
    }

    /// Render frontier including the DSP chunk currently being written.
    ///
    /// Use for replacement freshness checks, not playback/confirmation
    /// reporting: the announced chunk has not necessarily reached its host
    /// slice yet. The callback checks generation again for each subsequent
    /// chunk, including within an oversized host callback. This observation
    /// does not make generation publication atomic with rendering; the
    /// existing single-chunk cutover race remains bounded as before. Zero
    /// before the first callback, rather than a future playback timestamp.
    pub fn render_frontier_seconds(&self) -> f64 {
        self.render_frontier_frames() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// [`Self::render_frontier_seconds`] in frames: the end of the block
    /// the callback is rendering, or the clock when none is.
    pub fn render_frontier_frames(&self) -> u64 {
        self.shared.render_frontier_frames()
    }

    /// Render-frontier playhead: frames we have actually handed the device.
    ///
    /// WSLg's RDP sink reports jumpy CPAL playback timestamps (stall, then a
    /// leap). Using those as the live DSP/event clock can cluster onsets into
    /// the current callback after an underrun. Submitted frames are monotonic
    /// in the callback, so a kick
    /// grid stays even in the PCM we emit even when the sink's clock is not.
    /// Before the first callback, fall back to the stream timestamp so start
    /// prefetch still has a playhead.
    pub fn clock_nanos(&self) -> u64 {
        let frames = self.shared.frames.load(Ordering::Acquire);
        if frames == 0 {
            if let Some(stream) = &self.stream {
                return self.shared.playhead_at(stream.now());
            }
            return 0;
        }
        let sample_rate = u64::from(self.sample_rate.max(1));
        (u128::from(frames) * 1_000_000_000 / u128::from(sample_rate)) as u64
    }

    /// Pipeline depth used for startup schedule lead.
    ///
    /// The live clock is submitted frames (the render frontier), not Pulse's
    /// audible timestamp. Extra Pulse delay is already-rendered PCM sitting in
    /// the host and must not widen the query window, so the depth is capped at
    /// a few buffers.
    fn pipeline_latency_seconds(&self) -> f64 {
        let buffer_secs =
            f64::from(self.requested_buffer_frames) / f64::from(self.sample_rate.max(1));
        let latency_nanos = self.shared.playback_latency_nanos.load(Ordering::Acquire);
        let raw = if latency_nanos == 0 {
            3.0 * buffer_secs
        } else {
            latency_nanos as f64 / 1e9
        };
        let cap = (4.0 * buffer_secs).max(0.20);
        raw.min(cap)
    }

    /// How far ahead of the render-frontier clock an event must be targeted.
    /// Before the first callback, a conservative three-buffer estimate
    /// stands in. (Telemetry: events inside this window count as
    /// `late_events` - the signature of every start/reload tick.)
    pub fn schedule_lead_seconds(&self) -> f64 {
        let buffer_secs =
            f64::from(self.requested_buffer_frames) / f64::from(self.sample_rate.max(1));
        let lead = self.pipeline_latency_seconds() + 2.0 * buffer_secs;
        // WSLg's RDP sink can deepen its buffer during the first seconds of a
        // stream, so a lead taken at startup may undershoot the growing render
        // frontier. Floor the lead there.
        if std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some()
        {
            lead.max(0.35)
        } else {
            lead
        }
    }

    /// Replacement reserve relative to the completed-frame clock: in-flight
    /// rendering plus one requested buffer. Downstream playback latency is
    /// already-rendered PCM and must not be added to this boundary again.
    ///
    /// Startup still uses [`Self::schedule_lead_seconds`]. Candidate
    /// preparation headroom belongs to the producer; it must also recheck
    /// [`Self::render_frontier_seconds`] before publishing a replacement.
    /// Existing ringing/pre-onset voice semantics are unchanged here.
    pub fn continuity_margin_seconds(&self) -> f64 {
        let completed = self.shared.clock_frames();
        let rendering_until = self.shared.rendering_until_frame.load(Ordering::Acquire);
        let reserve_frames = rendering_until
            .saturating_sub(completed)
            .saturating_add(u64::from(self.requested_buffer_frames));
        reserve_frames as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Silence everything sounding, over the choke ramp.
    ///
    /// For swapping an audition: the snippet being replaced has to stop
    /// rather than ring on under the one that was asked for. The score's
    /// own reloads leave voices alone, as they must.
    pub fn cut_sounding(&self) {
        self.shared.cut_sounding.store(true, Ordering::Release);
    }

    /// Publish a replacement generation with its takeover frame (where its
    /// re-query cursor starts) and its cut intent.
    ///
    /// `cut` selects where the consumer silences the voices that sound
    /// under the new generation. [`TakeoverCut::AtFlip`] cuts at the
    /// consumer's flip (an immediate rewind). [`TakeoverCut::AtTakeover`]
    /// cuts at the takeover frame (a quantised rewind): the old score plays
    /// its countdown to the line, then the restarted loop sounds alone. An
    /// ordinary reload passes [`TakeoverCut::None`]: the old voices ring
    /// out, so a live edit does not click.
    pub fn set_generation(&self, generation: u64, takeover_frame: u64, cut: TakeoverCut) {
        self.shared
            .takeover_frame
            .store(takeover_frame, Ordering::Release);
        self.shared
            .takeover_cut
            .store(cut as u64, Ordering::Release);
        // A published flip supersedes any line arm: from here the flip's
        // own cut intent and takeover frame carry the handoff, and a stale
        // arm from a cancelled launch must not cut a later generation's
        // line that never asked for one.
        self.shared.line_arm.store(0, Ordering::Release);
        self.shared.generation.store(generation, Ordering::Release);
    }

    /// Pre-arm a cut at `frame`, the line of a quantised launch, before the
    /// generation that takes over there exists. The consumer silences the
    /// outgoing rendition at the frame, whenever the incoming generation is
    /// published. An evaluation that outlasts the countdown then cannot let
    /// the old score play through the line and sound its first beat twice.
    /// With `drop_from` set the consumer also refuses outgoing ring events
    /// at or after the frame (the restart's ghost window), so late
    /// countdown onsets never sound under the new loop.
    ///
    /// Cleared by the next [`Self::set_generation`] flip (whose own cut
    /// intent supersedes) or [`Self::clear_line_arm`]. An armed frame in
    /// the past still cuts: a launch whose evaluation overran the line
    /// asked for exactly that.
    pub fn arm_line_cut(&self, frame: u64, drop_from: bool) {
        let word = (frame << 2) | 0b01 | (u64::from(drop_from) << 1);
        self.shared.line_arm.store(word, Ordering::Release);
    }

    /// Withdraw a pre-armed line cut: the launch was cancelled or its
    /// evaluation failed, and the outgoing rendition keeps playing whole.
    /// Leaves [`crate::LINE_ARM_WITHDRAWN`] rather than zero so a consumer
    /// whose arm already fired knows no flip is coming and lets it go.
    pub fn clear_line_arm(&self) {
        self.shared
            .line_arm
            .store(crate::LINE_ARM_WITHDRAWN, Ordering::Release);
    }

    /// The raw armed line word: a test and telemetry probe of exactly what
    /// [`Self::arm_line_cut`] stored, [`Self::clear_line_arm`] leaves and a
    /// published flip clears. Bit 0 is the arm; see [`Self::line_cut_armed`].
    pub fn armed_line_word(&self) -> u64 {
        self.shared.line_arm.load(Ordering::Acquire)
    }

    /// Whether a line cut is armed on the consumer.
    pub fn line_cut_armed(&self) -> bool {
        self.armed_line_frame().is_some()
    }

    /// The frame a line cut is armed at, while one is. The consumer fires it
    /// in the first block that ends past that frame.
    pub fn armed_line_frame(&self) -> Option<u64> {
        let word = self.armed_line_word();
        (word & 0b01 != 0).then_some(word >> 2)
    }

    /// Generation currently published to the audio consumer.
    ///
    /// This is cheaper than building a complete device report and is the
    /// authoritative boundary for clients that must describe what can
    /// actually be heard rather than a replacement still being prefetched by
    /// the producer.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Play a helper note at the next callback boundary, independently of
    /// the score producer. Its target frame is intentionally ignored.
    pub fn push_immediate(&self, event: crate::AudioEvent) -> bool {
        self.shared.immediate.push(event)
    }

    /// False is back-pressure: the producer must retain and retry this event.
    pub fn push(&self, event: impl Into<crate::QueuedAudioEvent>) -> bool {
        self.shared.ring.push_queued(event.into())
    }

    /// Producer endpoint for finite prefill-window host-copy confirmations.
    pub fn confirmations(&self) -> crate::confirmation::ConfirmationChannel {
        self.shared.confirmations.clone()
    }

    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Release);
    }

    /// Request silence and wait until a data callback has rendered the whole
    /// stop ramp.
    ///
    /// This is an off-callback wait used by the product's graceful Stop path;
    /// it does not infer when already-submitted host buffers reach speakers.
    pub fn stop_and_wait(&self, timeout: Duration) -> bool {
        self.stop();
        let deadline = Instant::now() + timeout;
        while !self.shared.stop_acknowledged.load(Ordering::Acquire) {
            if Instant::now() >= deadline || self.shared.failed.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // DRAIN before teardown: acknowledgement only means the declick fade
        // was RENDERED - the sink still holds up to a playback-latency of
        // queued audio, and closing the stream then discards it mid-sample,
        // producing a stop click. Let the faded tail actually
        // reach the speaker, bounded so a stuck sink cannot hang a stop.
        let latency = self.shared.playback_latency_nanos.load(Ordering::Acquire);
        let buffer_nanos = u64::from(self.requested_buffer_frames) * 1_000_000_000
            / u64::from(self.sample_rate.max(1));
        let drain = Duration::from_nanos((latency + 2 * buffer_nanos).min(1_500_000_000));
        std::thread::sleep(drain);
        true
    }
}

impl Drop for LiveScalarDevice {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        if let Some(mut silent) = self.silent.take() {
            silent.stop();
        }
        // Finish the output owner before publishing retirement to any
        // producer endpoint retained beyond this device's lifetime.
        self.stream = None;
        self.shared.confirmations.retire_after_join();
    }
}

/// The most output channels the routed fan-out addresses.
const MAX_OUTPUT_CHANNELS: usize = 64;

/// 48 kHz, as most hardware; 256 frames, as the live default.
const SILENT_SAMPLE_RATE: u32 = 48_000;
const SILENT_BUFFER_FRAMES: u32 = 256;

struct LiveCallbackMeters {
    master: crate::meter::MasterMeter,
    load: RealtimeLoadMeter,
    pressure: RealtimePressureMeter,
    sample_rate: u32,
    /// By value, not boxed: its rings are fixed arrays, so holding it here
    /// keeps the whole callback path free of any allocation at all - which
    /// the analysis-tap tripwire tests measure.
    limiter: crate::limiter::Limiter,
    /// Whether the limiter ran on the last block. Off holds its rings
    /// where they were, so coming back on has to clear them.
    limiter_running: bool,
}

impl LiveCallbackMeters {
    fn new(sample_rate: u32, publisher: &RealtimeLoadPublisher) -> Self {
        Self {
            master: crate::meter::MasterMeter::new(sample_rate),
            load: RealtimeLoadMeter::new(sample_rate, publisher),
            pressure: RealtimePressureMeter::default(),
            sample_rate,
            limiter_running: false,
            limiter: crate::limiter::Limiter::new(
                sample_rate,
                crate::limiter::DEFAULT_THRESHOLD_DB,
                crate::limiter::Character::default(),
            ),
        }
    }
}

const CALLBACK_PENDING: u8 = 0;
const CALLBACK_ACTIVE: u8 = 1;
const CALLBACK_FAILED: u8 = 2;

/// Stream-local admission, separate from the shared musical Stop request.
/// Pending callbacks must not consume assets or change the running clock.
struct CallbackGate(AtomicU8);

impl CallbackGate {
    fn new() -> Self {
        Self(AtomicU8::new(CALLBACK_PENDING))
    }

    fn is_active(&self) -> bool {
        self.0.load(Ordering::Acquire) == CALLBACK_ACTIVE
    }

    fn activate(&self) -> Result<(), DevicePlaybackError> {
        self.0
            .compare_exchange(
                CALLBACK_PENDING,
                CALLBACK_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| {
                DevicePlaybackError::Unavailable(
                    "live output failed before activation or was already activated".into(),
                )
            })
    }

    fn render_or_silence<T>(&self, output: &mut [T], render: impl FnOnce(&mut [T]))
    where
        T: Sample + FromSample<f32>,
    {
        tripwire::audio_scope(|| {
            if self.is_active() {
                render(output);
            } else {
                output.fill(T::from_sample(0.0));
            }
        });
    }

    fn report_error(&self, failed: &AtomicBool, errors: &AtomicU64) {
        // An error before activation belongs only to this candidate. The CAS
        // also closes the race between an error and the activation release.
        if self.0.compare_exchange(
            CALLBACK_PENDING,
            CALLBACK_FAILED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) == Err(CALLBACK_ACTIVE)
        {
            errors.fetch_add(1, Ordering::Relaxed);
            failed.store(true, Ordering::Release);
        }
    }
}

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

impl PreparedLiveOutput {
    fn backend(
        shared: &LiveShared,
        sample_rate: u32,
        options: LiveOutputOptions,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<LiveScalarBackend, DevicePlaybackError> {
        validate_output_samples(samples)?;
        let mut backend =
            LiveScalarBackend::with_dispatch(sample_rate, MAX_LIVE_VOICES, options.dispatch())
                .map_err(|message| {
                    DevicePlaybackError::Unavailable(format!(
                        "cannot initialise live scalar DSP: {message}"
                    ))
                })?;
        backend.set_sample_resampling_mode(options.sample_resampling_mode());
        backend.set_max_polyphony(shared.max_polyphony.load(Ordering::Relaxed));
        backend.set_input(Some(shared.input.clone()));
        backend.set_confirmations(shared.confirmations.clone());
        for (id, sample) in samples {
            let displaced = backend
                .install_prepared_sample(*id, Box::new(sample.clone()))
                .map_err(|_| {
                    DevicePlaybackError::ResourceLimit("sample id exceeds output bank".into())
                })?;
            drop(displaced);
        }
        Ok(backend)
    }

    fn native(
        opened: ScalarDevice,
        shared: LiveShared,
        options: LiveOutputOptions,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<Self, DevicePlaybackError> {
        let sample_rate = opened.sample_rate();
        let native_format = opened.config.sample_format();
        let format = sample_format_from_cpal(native_format).ok_or_else(|| {
            DevicePlaybackError::Unavailable(format!(
                "audio output sample format {native_format} is not supported by live scalar playback"
            ))
        })?;
        let requested =
            select_live_buffer_frames(opened.config.buffer_size(), options.buffer_preference())?;
        let backend = Self::backend(&shared, sample_rate, options, samples)?;
        let backend_bytes = backend.prepared_bytes();
        let gate = Arc::new(CallbackGate::new());
        // Include callbacks invoked synchronously while CPAL builds a stream.
        let tripwire_baseline = tripwire::Violations::capture();
        let stream = build_live_stream(
            &opened.device,
            &opened.config,
            requested,
            backend,
            shared,
            Arc::clone(&gate),
        )?;
        let reported = stream.buffer_size().ok();
        let facts = AudioOutputFacts::new(
            AudioHost::cpal(opened.host_id),
            opened.name,
            sample_rate,
            opened.config.channels(),
            Some(format),
            requested,
            reported,
        )
        .with_buffer_preference(options.buffer_preference())
        .with_device_period_frames(fixed_device_period(opened.config.buffer_size()));
        Ok(Self {
            handle: OutputHandle::Native(stream),
            facts,
            gate,
            tripwire_baseline,
            backend_bytes,
        })
    }

    fn silent(
        shared: LiveShared,
        sample_rate: u32,
        options: LiveOutputOptions,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<Self, DevicePlaybackError> {
        let buffer_frames = options
            .buffer_preference()
            .resolve(SILENT_BUFFER_FRAMES, AudioBufferRange::Unknown)
            .map_err(buffer_preference_error)?;
        let backend = Self::backend(&shared, sample_rate, options, samples)?;
        let backend_bytes = backend.prepared_bytes();
        let gate = Arc::new(CallbackGate::new());
        let tripwire_baseline = tripwire::Violations::capture();
        let silent = spawn_silent_output(
            shared,
            backend,
            sample_rate,
            buffer_frames,
            Arc::clone(&gate),
        )?;
        Ok(Self {
            handle: OutputHandle::Silent(silent),
            facts: AudioOutputFacts::new(
                AudioHost::Silent,
                SILENT_OUTPUT_NAME,
                sample_rate,
                2,
                None,
                buffer_frames,
                Some(buffer_frames),
            )
            .with_buffer_preference(options.buffer_preference()),
            gate,
            tripwire_baseline,
            backend_bytes,
        })
    }

    fn start_gated(&self) -> Result<(), DevicePlaybackError> {
        if let OutputHandle::Native(stream) = &self.handle {
            stream.play().map_err(|error| {
                DevicePlaybackError::Unavailable(format!(
                    "cannot start live audio output {}: {error}",
                    self.facts.device_id(),
                ))
            })?;
        }
        Ok(())
    }

    fn activate(self) -> Result<OutputHandle, DevicePlaybackError> {
        self.gate.activate()?;
        Ok(self.handle)
    }
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

impl SilentHandle {
    /// Stop the thread and wait for it to leave `LiveShared`, so a
    /// replacement stream never runs concurrently with it.
    fn stop(&mut self) {
        self.close.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(feature = "test-support")]
impl LiveScalarDevice {
    /// Freeze (or release) the silent output's clock, for tests that must
    /// observe a moment before a line without racing wall time.
    ///
    /// Holding returns only once the silent thread has stopped between two
    /// blocks, so the render frontier read afterwards stays put until the
    /// release. Released, the clock resumes from where it stopped. Returns
    /// false, doing nothing, when this device is not a running silent output.
    #[doc(hidden)]
    pub fn hold_silent_clock_for_test(&self, hold: bool) -> bool {
        let Some(silent) = self.silent.as_ref().filter(|silent| silent.join.is_some()) else {
            return false;
        };
        if !hold {
            silent
                .clock_hold
                .store(SILENT_CLOCK_RUNNING, Ordering::Release);
            return true;
        }
        let _ = silent.clock_hold.compare_exchange(
            SILENT_CLOCK_RUNNING,
            SILENT_CLOCK_HOLD_REQUESTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while silent.clock_hold.load(Ordering::Acquire) != SILENT_CLOCK_HELD {
            assert!(
                Instant::now() < deadline,
                "the silent output never paused its clock"
            );
            std::thread::sleep(Duration::from_micros(200));
        }
        true
    }
}

impl Drop for SilentHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

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

#[cfg(feature = "test-support")]
impl ManualLiveOutput {
    pub fn new(sample_rate: u32, initial_generation: u64) -> Result<Self, DevicePlaybackError> {
        Self::new_with_samples(sample_rate, initial_generation, &[])
    }

    pub fn new_with_samples(
        sample_rate: u32,
        initial_generation: u64,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<Self, DevicePlaybackError> {
        Self::new_with_samples_and_options(
            sample_rate,
            initial_generation,
            samples,
            LiveOutputOptions::default(),
        )
    }

    pub fn new_with_samples_and_options(
        sample_rate: u32,
        initial_generation: u64,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
        options: LiveOutputOptions,
    ) -> Result<Self, DevicePlaybackError> {
        let buffer_frames = options
            .buffer_preference()
            .resolve(SILENT_BUFFER_FRAMES, AudioBufferRange::Unknown)
            .map_err(buffer_preference_error)?;
        let shared = LiveShared::new(initial_generation);
        let backend = PreparedLiveOutput::backend(&shared, sample_rate, options, samples)?;
        let meters = LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
        let gate = Arc::new(CallbackGate::new());
        let prepared = PreparedLiveOutput {
            handle: OutputHandle::Silent(SilentHandle {
                close: Arc::new(AtomicBool::new(false)),
                join: None,
                clock_hold: Arc::new(std::sync::atomic::AtomicU8::new(SILENT_CLOCK_RUNNING)),
            }),
            facts: AudioOutputFacts::new(
                AudioHost::Silent,
                SILENT_OUTPUT_NAME,
                sample_rate,
                2,
                None,
                buffer_frames,
                Some(buffer_frames),
            )
            .with_buffer_preference(options.buffer_preference()),
            gate: Arc::clone(&gate),
            tripwire_baseline: tripwire::Violations::capture(),
            backend_bytes: backend.prepared_bytes(),
        };
        let device = LiveScalarDevice::start_prepared(shared, options, prepared)?;
        Ok(Self {
            device,
            backend,
            meters,
            gate,
            _same_thread: std::marker::PhantomData,
        })
    }

    pub fn device(&self) -> &LiveScalarDevice {
        &self.device
    }

    /// Render interleaved stereo through the ordinary gated callback body.
    pub fn render(&mut self, output: &mut [f32]) {
        let shared = &self.device.shared;
        self.gate.render_or_silence(output, |output| {
            let stop_requested = shared.stopped.load(Ordering::Acquire);
            write_live_output(&mut self.backend, &mut self.meters, output, 2, shared);
            // The ramp spans several callbacks at a short buffer. An earlier
            // acknowledgement lets the owner close the stream inside the ramp.
            if stop_requested && self.backend.stop_ramp_complete() {
                shared.stop_acknowledged.store(true, Ordering::Release);
            }
        });
    }
}

fn silent_elapsed(
    started: &mut Option<Instant>,
    gate: &CallbackGate,
    clock: impl FnOnce() -> Instant,
) -> Option<Duration> {
    if !gate.is_active() {
        return None;
    }
    // A timestamp captured before admission could include preparation time
    // if activation occurs while this thread is descheduled.
    let now = clock();
    Some(now.duration_since(*started.get_or_insert(now)))
}

fn spawn_silent_output(
    shared: LiveShared,
    mut backend: LiveScalarBackend,
    sample_rate: u32,
    buffer_frames: u32,
    gate: Arc<CallbackGate>,
) -> Result<SilentHandle, DevicePlaybackError> {
    let close = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&close);
    #[cfg(feature = "test-support")]
    let clock_hold = Arc::new(std::sync::atomic::AtomicU8::new(SILENT_CLOCK_RUNNING));
    #[cfg(feature = "test-support")]
    let hold = Arc::clone(&clock_hold);
    let mut meters = LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
    let mut output = vec![0.0f32; buffer_frames as usize * 2];
    let join = std::thread::Builder::new()
        .name("silent-output".into())
        .spawn(move || {
            let mut started: Option<Instant> = None;
            let mut delivered = 0u64;
            #[cfg(feature = "test-support")]
            let mut held_since: Option<Instant> = None;
            while !flag.load(Ordering::Acquire) {
                // A held clock delivers nothing, so a test can look at a
                // moment that wall time would otherwise carry away. On
                // release the origin moves forward by the time spent held,
                // so the clock resumes where it stopped (partial progress
                // towards the next block included) instead of rendering the
                // held span in one catch-up burst.
                #[cfg(feature = "test-support")]
                {
                    if hold.load(Ordering::Acquire) != SILENT_CLOCK_RUNNING {
                        held_since.get_or_insert_with(Instant::now);
                        // Acknowledge only a standing request: a release
                        // that lands between the load and here must win.
                        let _ = hold.compare_exchange(
                            SILENT_CLOCK_HOLD_REQUESTED,
                            SILENT_CLOCK_HELD,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                        std::thread::sleep(Duration::from_micros(500));
                        continue;
                    }
                    if let Some(since) = held_since.take()
                        && let Some(origin) = started.as_mut()
                    {
                        *origin += since.elapsed();
                    }
                }
                let first = started.is_none();
                let Some(elapsed) = silent_elapsed(&mut started, &gate, Instant::now) else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                if first {
                    shared.playback_origin_nanos.store(0, Ordering::Release);
                }
                let due = (elapsed.as_secs_f64() * f64::from(sample_rate)) as u64;
                while delivered + u64::from(buffer_frames) <= due && !flag.load(Ordering::Acquire) {
                    gate.render_or_silence(&mut output, |output| {
                        let stop_requested = shared.stopped.load(Ordering::Acquire);
                        shared.buffer_playback_nanos.store(
                            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
                            Ordering::Release,
                        );
                        shared.playback_latency_nanos.store(0, Ordering::Release);
                        write_live_output(&mut backend, &mut meters, output, 2, &shared);
                        if stop_requested && backend.stop_ramp_complete() {
                            shared.stop_acknowledged.store(true, Ordering::Release);
                        }
                    });
                    delivered += u64::from(buffer_frames);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot start the silent output: {error}"))
        })?;
    Ok(SilentHandle {
        close,
        join: Some(join),
        #[cfg(feature = "test-support")]
        clock_hold,
    })
}

impl LiveScalarDevice {
    /// Verify that allocator events cross this device's reporting path.
    ///
    /// Opening a stream is not enough: a missing global allocator and a report
    /// accidentally hard-coded to zero both look like a clean callback. The
    /// canary must be exactly one allocation plus its release, the report must
    /// expose that exact delta, and no earlier violation may exist.
    pub fn arm_callback_tripwire(&mut self) -> bool {
        self.arm_callback_tripwire_checked().is_ok()
    }

    /// Diagnose a failed arm so callers can distinguish an absent allocator
    /// from a callback violation on an output that should be retired.
    pub fn arm_callback_tripwire_checked(&mut self) -> Result<(), CallbackTripwireArmError> {
        let before_report = self.report();
        let before_raw = tripwire::Violations::capture();
        let allocator_armed = tripwire::allocator_is_armed();
        let after_raw = tripwire::Violations::capture();
        let after_report = self.report();
        let raw_canary = after_raw.since(before_raw);
        let reported_canary = tripwire::Violations {
            allocs: after_report
                .callback_allocations
                .wrapping_sub(before_report.callback_allocations),
            frees: after_report
                .callback_frees
                .wrapping_sub(before_report.callback_frees),
        };
        // The positive control is not callback work. Exclude only its own
        // allocation and free; rebasing to `after_raw` would also erase a
        // previous or concurrent real-time violation.
        if allocator_armed {
            self.tripwire_canaries.allocs = self.tripwire_canaries.allocs.wrapping_add(1);
            self.tripwire_canaries.frees = self.tripwire_canaries.frees.wrapping_add(1);
        }
        if !allocator_armed {
            return Err(CallbackTripwireArmError::MissingAllocator);
        }
        if before_report.callback_allocations != 0
            || before_report.callback_frees != 0
            || before_report.callback_scope_misses != 0
        {
            return Err(CallbackTripwireArmError::PriorViolation {
                allocations: before_report.callback_allocations,
                frees: before_report.callback_frees,
                scope_misses: before_report.callback_scope_misses,
            });
        }
        if raw_canary
            != (tripwire::Violations {
                allocs: 1,
                frees: 1,
            })
            || reported_canary != raw_canary
        {
            return Err(CallbackTripwireArmError::CanaryMismatch {
                raw: raw_canary,
                reported: reported_canary,
            });
        }
        Ok(())
    }

    pub fn report(&self) -> LiveDeviceReport {
        let mut report = self.shared.report(self.stream_id);
        report.playhead_nanos = self.clock_nanos();
        let violations = tripwire::Violations::capture()
            .since(self.tripwire_baseline)
            .since(self.tripwire_canaries);
        apply_tripwire_report(report, violations)
    }

    /// What this output holds in memory. Call it from the producer: it
    /// reads counters the producer moves, the caches it keeps and what was
    /// measured when the backend was prepared, and never the callback's own
    /// state, which it cannot see. A handful of loads and two short locks.
    pub fn memory(&self) -> LiveDeviceMemory {
        use std::mem::{size_of, size_of_val};
        let ring =
            |ring: &Ring| ring.pushed().min(ring.capacity()) * size_of::<crate::QueuedAudioEvent>();
        let orbit_reverbs: usize = self
            .reverb_cache
            .lock()
            .expect("producer reverb cache")
            .iter()
            .flatten()
            .map(|(_, bytes)| bytes)
            .sum();
        // The producer cache sees pending installs but forgets older
        // fingerprints after eight; the callback reports its full resident
        // pool, including reverbs leased to voices. Use the larger view.
        let pending_stage_reverbs = self
            .fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache")
            .iter()
            .fold(0usize, |held, (_, bytes, _)| {
                if *bytes <= crate::scalar::MAX_FX_REVERB_BYTES.saturating_sub(held) {
                    held + bytes
                } else {
                    held
                }
            });
        let stage_reverbs = pending_stage_reverbs.max(
            self.shared
                .samples
                .fx_reverb_resident_bytes
                .load(Ordering::Acquire),
        );
        let master = &self.shared.analysis;
        let visual_taps = self
            .shared
            .visual_analysis
            .iter()
            .filter(|tap| tap.get().is_some())
            .count();
        LiveDeviceMemory {
            event_rings: ring(&self.shared.ring) + ring(&self.shared.immediate),
            input: self.shared.input.touched_bytes(),
            record: self.shared.record.touched_bytes(),
            reverbs: orbit_reverbs + stage_reverbs,
            backend: self.backend_bytes,
            analysis: size_of_val(&**master)
                + master.sides.as_deref().map_or(0, size_of_val)
                + visual_taps * size_of::<LiveAnalysisTap>(),
        }
    }

    /// Cumulative voices the callback had to refuse.
    ///
    /// Cheap enough to poll every live iteration, unlike [`Self::report`],
    /// which also captures tripwire violations. The live loop watches this for
    /// CHANGE so it can surface a recoverable `live_error` without ending the
    /// set.
    pub fn refused_voices(&self) -> u64 {
        self.shared.refused_voices.load(Ordering::Acquire)
    }

    /// Whether the output stream itself has reported an error: the device
    /// went away under the music.
    ///
    /// Told apart from the rest of [`Self::check_health`] because it is
    /// the recoverable one. A Bluetooth headset changing profile, an
    /// interface unplugged, a machine waking from sleep - the stream is
    /// gone and a new one opens in its place, where a run of unbounded
    /// buffering says the machine cannot keep up and reopening would only
    /// do it again.
    pub fn output_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    pub fn check_health(&self) -> Result<(), DevicePlaybackError> {
        if self.shared.failed.load(Ordering::Acquire) {
            return Err(DevicePlaybackError::Unavailable(format!(
                "audio output {} failed while playing live",
                self.name
            )));
        }
        // A refused voice is not fatal. Running out of voices means that
        // some notes did not sound: that is degradation, not failure, and
        // stopping the music is worse than a dropped note. `refused_voices`
        // is cumulative and never decays, so it cannot gate health. The
        // live report carries it as recoverable telemetry and the set
        // plays on.

        // Judge the CURRENT latency, not the high-water mark: a single
        // transient spike must not kill a live set minutes later (the mark
        // never decays; it remains telemetry). Unbounded buffering is still a
        // failure. WSLg's RDP sink can deepen its buffer after jitter, so its
        // default ceiling is higher. RUSTEL_MAX_LIVE_LATENCY_MS overrides the
        // ceiling on every platform.
        let latency = self.shared.playback_latency_nanos.load(Ordering::Acquire);
        let limit = live_latency_limit_nanos();
        if latency > limit {
            return Err(DevicePlaybackError::Unavailable(format!(
                "live output {} buffered {:.1}ms ahead, above the {:.1}ms safety limit",
                self.name,
                latency as f64 / 1_000_000.0,
                limit as f64 / 1_000_000.0
            )));
        }
        Ok(())
    }

    /// Tear the CPAL stream and open a fresh one on the same shared clock.
    ///
    /// Voices in the old callback die (new backend). Generation and clock time
    /// stay put; absolute frame coordinates are rescaled if the replacement
    /// device uses a different rate. The caller must re-query the cleared
    /// scheduling horizon before resuming ordinary producer steps.
    pub fn recycle_output(&mut self) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to(None)
    }

    /// Recycle onto a named output, or the host default when `selector` is
    /// `None`. Playback continues on the replacement stream at the same
    /// generation and clock time.
    ///
    /// A failed device lookup leaves the current output untouched. Querying
    /// its default configuration and opening the replacement happen after the
    /// old stream closes, since some hosts require exclusive device access.
    pub fn recycle_output_to(&mut self, selector: Option<&str>) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with(
            selector,
            ScalarDevice::discover_output,
            Self::prepare_recycled_output,
        )
    }

    /// Recycle with cancellation checks before teardown and around candidate
    /// preparation/start. Cancellation after teardown leaves no output; the
    /// caller must not schedule new work and should release the live owner.
    pub fn recycle_output_to_guarded(
        &mut self,
        selector: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with_cancel(
            selector,
            ScalarDevice::discover_output,
            Self::prepare_recycled_output,
            cancelled,
        )
    }

    /// Replace the output with a complete snapshot of its decoded sample bank.
    /// PCM is shared, not decoded again, and installed before callbacks activate.
    /// Pending sample installs and uninstalls are superseded after the old
    /// callback joins. The caller retains the snapshot for a failed-open retry.
    pub fn recycle_output_to_with_samples(
        &mut self,
        selector: Option<&str>,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
        cancelled: impl Fn() -> bool,
    ) -> Result<(), DevicePlaybackError> {
        validate_output_samples(samples)?;
        self.recycle_output_to_with_cancel(
            selector,
            ScalarDevice::discover_output,
            |device, opened| device.prepare_recycled_output_with_samples(opened, samples),
            cancelled,
        )
    }

    /// Mark the stream as having reported an error, the way a device that
    /// went away under the music does. What a Bluetooth headset changing
    /// profile leaves behind, without needing one.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn fail_output_stream_for_test(&self) {
        self.shared.failed.store(true, Ordering::Release);
    }

    /// Inject a preparation error after closing the old callback and draining
    /// its ring. This exercises the same teardown as an exclusive-device open
    /// failure without depending on a particular host or physical device.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn fail_output_replacement_for_test(&mut self) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with(
            Some(SILENT_OUTPUT_NAME),
            |_| unreachable!("silent output does not require discovery"),
            |_, _| {
                Err(DevicePlaybackError::Unavailable(
                    "output preparation failed".into(),
                ))
            },
        )
    }

    fn recycle_output_to_with(
        &mut self,
        selector: Option<&str>,
        discover: impl FnOnce(Option<&str>) -> Result<DiscoveredOutput, DevicePlaybackError>,
        prepare: impl FnOnce(
            &Self,
            Option<DiscoveredOutput>,
        ) -> Result<PreparedLiveOutput, DevicePlaybackError>,
    ) -> Result<(), DevicePlaybackError> {
        self.recycle_output_to_with_cancel(selector, discover, prepare, || false)
    }

    fn recycle_output_to_with_cancel(
        &mut self,
        selector: Option<&str>,
        discover: impl FnOnce(Option<&str>) -> Result<DiscoveredOutput, DevicePlaybackError>,
        prepare: impl FnOnce(
            &Self,
            Option<DiscoveredOutput>,
        ) -> Result<PreparedLiveOutput, DevicePlaybackError>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), DevicePlaybackError> {
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        self.buffer_preference()
            .validate()
            .map_err(buffer_preference_error)?;
        let opened = if selector == Some(SILENT_OUTPUT_NAME)
            || (selector.is_none() && self.name == SILENT_OUTPUT_NAME)
        {
            None
        } else {
            Some(discover(selector)?)
        };
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        let old_sample_rate = self.sample_rate;
        self.shared.stopped.store(true, Ordering::Release);
        if let Some(mut silent) = self.silent.take() {
            silent.stop();
        }
        // Dropping the stream joins the CPAL callback thread, so the consumer
        // role is free from here. Saying so explicitly keeps the ring's
        // single-consumer check able to tell this legitimate handoff from a
        // second consumer running concurrently.
        self.stream = None;
        // No output remains if replacement preparation or startup fails.
        self.shared.failed.store(true, Ordering::Release);
        // SAFETY: the stream and the silent callback are gone, so no callback
        // thread pops either ring again.
        unsafe {
            self.shared.ring.release_consumer();
            self.shared.immediate.release_consumer();
        }
        while self.shared.ring.pop().is_some() {}
        while self.shared.immediate.pop().is_some() {}
        // The producer thread above temporarily became the drain consumer.
        // Hand the role back once more so the replacement callback can claim
        // it instead of refusing every pop as a second consumer.
        // SAFETY: this thread's drain has finished, and no replacement
        // callback exists yet.
        unsafe {
            self.shared.ring.release_consumer();
            self.shared.immediate.release_consumer();
        }
        self.shared.reset_analysis();
        self.shared.meter.reset_levels();

        // The old consumer has joined. Keep its completed receipts available
        // to the producer, but never let a replacement certify unfinished
        // work using a new bank or a rebased output clock.
        self.shared
            .confirmations
            .advance_epoch_after_join()
            .map_err(|error| match error {
                crate::confirmation::EpochAdvanceError::Busy => DevicePlaybackError::Unavailable(
                    "output confirmation producer is busy; retry output recovery".to_owned(),
                ),
                crate::confirmation::EpochAdvanceError::Exhausted => {
                    DevicePlaybackError::ResourceLimit(
                        "output confirmation epoch exhausted".to_owned(),
                    )
                }
                crate::confirmation::EpochAdvanceError::Closed => DevicePlaybackError::Unavailable(
                    "output confirmation channel is closed".to_owned(),
                ),
            })?;

        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        let prepared = prepare(self, opened)?;
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        // A host may invoke callbacks during build or play. They remain
        // gated until both the shared clock and producer facts are committed.
        prepared.start_gated()?;
        if cancelled() {
            return Err(DevicePlaybackError::Cancelled);
        }
        let facts = &prepared.facts;
        self.shared
            .prepare_recycled_stream(old_sample_rate, facts.sample_rate_hz());
        self.shared
            .stop_acknowledged
            .store(false, Ordering::Release);
        self.shared.failed.store(false, Ordering::Release);
        self.shared.stopped.store(false, Ordering::Release);
        self.sample_rate = facts.sample_rate_hz();
        self.channels = facts.channels();
        self.host_id = facts.host().id().map(Into::into);
        self.output_sample_format = facts.sample_format();
        self.name = facts.device_id().to_owned();
        self.requested_buffer_frames = facts.requested_buffer_frames();
        self.reported_buffer_frames = facts.reported_buffer_frames();
        self.device_period_frames = facts.device_period_frames();
        self.clear_reverb_caches();
        if let Err(error) = self.activate_prepared(prepared) {
            self.shared.stopped.store(true, Ordering::Release);
            self.shared.failed.store(true, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    fn prepare_recycled_output(
        &self,
        opened: Option<DiscoveredOutput>,
    ) -> Result<PreparedLiveOutput, DevicePlaybackError> {
        self.prepare_output(opened, &[])
    }

    fn prepare_recycled_output_with_samples(
        &self,
        opened: Option<DiscoveredOutput>,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<PreparedLiveOutput, DevicePlaybackError> {
        // The old callback has joined and only this producer owns the channel.
        // Its queued commands predate the authoritative replacement snapshot.
        for install in self.shared.samples.installs.drain_available() {
            if !install.sample.is_null() {
                // SAFETY: this unconsumed producer-owned baton has no other owner.
                drop(unsafe { Box::from_raw(install.sample) });
            }
        }
        self.reclaim_assets();
        self.prepare_output(opened, samples)
    }

    fn prepare_output(
        &self,
        opened: Option<DiscoveredOutput>,
        samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
    ) -> Result<PreparedLiveOutput, DevicePlaybackError> {
        match opened {
            Some(opened) => PreparedLiveOutput::native(
                ScalarDevice::configure_output(opened)?,
                self.shared.clone(),
                self.options,
                samples,
            ),
            None => PreparedLiveOutput::silent(
                self.shared.clone(),
                SILENT_SAMPLE_RATE,
                self.options,
                samples,
            ),
        }
    }

    fn clear_reverb_caches(&self) {
        // The fresh callback owns a fresh scalar backend. Its old producer
        // cache must not suppress regeneration of reverbs that disappeared
        // with the previous backend.
        self.reverb_cache
            .lock()
            .expect("producer reverb cache")
            .fill(None);
        self.fx_reverb_cache
            .lock()
            .expect("producer fx reverb cache")
            .clear();
        self.shared
            .samples
            .fx_reverb_resident_bytes
            .store(0, Ordering::Release);
    }
}

fn validate_output_samples(
    samples: &[(crate::sample::SampleId, crate::sample::DecodedSample)],
) -> Result<(), DevicePlaybackError> {
    if samples.len() > crate::sample::SAMPLE_BANK_CAPACITY
        || samples
            .iter()
            .any(|(id, _)| id.0 as usize >= crate::sample::SAMPLE_BANK_CAPACITY)
    {
        return Err(DevicePlaybackError::ResourceLimit(
            "sample snapshot exceeds output bank capacity".into(),
        ));
    }
    Ok(())
}

fn live_latency_limit_nanos() -> u64 {
    let default_ms = if std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
    {
        2000
    } else {
        MAX_LIVE_PLAYBACK_LATENCY_NANOS / 1_000_000
    };
    std::env::var(MAX_LIVE_LATENCY_ENV)
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .filter(|ms| (50..=5_000).contains(ms))
        .unwrap_or(default_ms)
        * 1_000_000
}

fn apply_tripwire_report(
    mut report: LiveDeviceReport,
    violations: tripwire::Violations,
) -> LiveDeviceReport {
    report.callback_allocations = violations.allocs;
    report.callback_frees = violations.frees;
    report
}

fn buffer_preference_error(error: crate::AudioBufferError) -> DevicePlaybackError {
    DevicePlaybackError::Unavailable(error.to_string())
}

fn select_live_buffer_frames(
    supported: &SupportedBufferSize,
    preference: AudioBufferPreference,
) -> Result<u32, DevicePlaybackError> {
    // Forwarded audio paths (WSLg's RDP sink, which shares one channel with
    // window video) underrun audibly at an interactive size, so the default
    // there is large. A typed-in request overrides either platform default:
    // the env knob is how an arbitrary frame count reaches the automatic
    // path, and the report records what was actually used.
    let platform_default = if std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
    {
        2048
    } else {
        TARGET_LIVE_BUFFER_FRAMES
    };
    let target = std::env::var(LIVE_BUFFER_FRAMES_ENV)
        .ok()
        .and_then(|frames| frames.parse::<u32>().ok())
        .filter(|frames| {
            (AudioBufferPreference::MIN_FRAMES..=AudioBufferPreference::MAX_FRAMES).contains(frames)
        })
        .unwrap_or(platform_default);
    preference
        .resolve(target, live_buffer_range(supported, cfg!(windows)))
        .map_err(buffer_preference_error)
}

/// The range a buffer request is resolved against.
///
/// WASAPI in shared mode advertises its device period as a degenerate range
/// (min == max, 480 frames at 48 kHz), but that period is the callback's
/// cadence, not a limit on the buffer: the request is passed to
/// `IAudioClient::Initialize` as the buffer duration, which accepts any
/// positive length and keeps that much audio queued ahead of the period.
/// A ceiling would clamp every request (`--buffer-frames 2048`, the
/// studio's output-latency row, `RUSTEL_LIVE_BUFFER_FRAMES`) to the same
/// 10 ms, and a larger buffer could not fix underruns on Windows.
/// With `fixed_period_is_floor` such a range is a floor only: smaller
/// requests still get the period, larger ones pass through. Other hosts
/// keep a degenerate range as the hard constraint it is there.
fn live_buffer_range(
    supported: &SupportedBufferSize,
    fixed_period_is_floor: bool,
) -> AudioBufferRange {
    match *supported {
        SupportedBufferSize::Range { min, max } if fixed_period_is_floor && min == max => {
            AudioBufferRange::Range {
                min,
                max: AudioBufferPreference::MAX_FRAMES.max(min),
            }
        }
        SupportedBufferSize::Range { min, max } => AudioBufferRange::Range { min, max },
        SupportedBufferSize::Unknown => AudioBufferRange::Unknown,
    }
}

/// A host's fixed callback period, when it advertises one as a degenerate
/// buffer range: kept beside the request so the stream facts can say both
/// when a shared-mode buffer is larger than its period.
fn fixed_device_period(supported: &SupportedBufferSize) -> Option<u32> {
    match *supported {
        SupportedBufferSize::Range { min, max } if min == max => Some(min),
        _ => None,
    }
}

fn sample_format_from_cpal(format: SampleFormat) -> Option<AudioSampleFormat> {
    match format {
        SampleFormat::I8 => Some(AudioSampleFormat::I8),
        SampleFormat::I16 => Some(AudioSampleFormat::I16),
        SampleFormat::I24 => Some(AudioSampleFormat::I24),
        SampleFormat::I32 => Some(AudioSampleFormat::I32),
        SampleFormat::I64 => Some(AudioSampleFormat::I64),
        SampleFormat::U8 => Some(AudioSampleFormat::U8),
        SampleFormat::U16 => Some(AudioSampleFormat::U16),
        SampleFormat::U24 => Some(AudioSampleFormat::U24),
        SampleFormat::U32 => Some(AudioSampleFormat::U32),
        SampleFormat::U64 => Some(AudioSampleFormat::U64),
        SampleFormat::F32 => Some(AudioSampleFormat::F32),
        SampleFormat::F64 => Some(AudioSampleFormat::F64),
        _ => None,
    }
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

/// The input device whose name is, or contains, `wanted`.
fn find_input_device(host: &cpal::Host, wanted: &str) -> Result<cpal::Device, DevicePlaybackError> {
    let candidates = host
        .input_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .filter_map(|device| {
            device.id().ok().map(|id| {
                let friendly = device
                    .description()
                    .ok()
                    .map(|description| description.name().to_owned())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_default();
                (id.to_string(), friendly, device)
            })
        })
        .collect::<Vec<_>>();
    let matched = match_listed_device(&candidates, wanted).ok_or_else(|| {
        DevicePlaybackError::Unavailable(format!(
            "audio input {wanted:?} not found; available: {}",
            candidates
                .iter()
                .map(|(_, friendly, _)| friendly.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    Ok(candidates
        .into_iter()
        .nth(matched)
        .map(|(_, _, device)| device)
        .expect("matched"))
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

impl AudioInput {
    /// Open the host default, or a named device, onto `ring`.
    pub fn open(
        selector: Option<&str>,
        ring: Arc<crate::input::InputRing>,
        preferred_rate: Option<u32>,
    ) -> Result<Self, DevicePlaybackError> {
        let device = with_preferred_host(|host| match selector {
            None => host.default_input_device().ok_or_else(|| {
                DevicePlaybackError::Unavailable("no default audio input device".into())
            }),
            Some(wanted) => find_input_device(host, wanted),
        })?;
        let name = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "default-input".into());
        let config = device.default_input_config().map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot read default input configuration for {name}: {error}"
            ))
        })?;
        // The output's rate when the device offers it, so the ring is read
        // frame for frame; otherwise the device's own, and the reader walks
        // the ring at the ratio.
        let config = match preferred_rate {
            Some(rate) if rate != config.sample_rate() => {
                input_config_at_rate(&device, &config, rate).unwrap_or(config)
            }
            _ => config,
        };
        if config.channels() == 0 {
            return Err(DevicePlaybackError::Unavailable(format!(
                "audio input {name} reports no channels"
            )));
        }
        let native_sample_format = config.sample_format();
        let sample_format = sample_format_from_cpal(native_sample_format).ok_or_else(|| {
            DevicePlaybackError::Unavailable(format!(
                "audio input sample format {native_sample_format} is not supported"
            ))
        })?;
        ring.set_channels(usize::from(config.channels()));
        ring.set_sample_rate(config.sample_rate());
        ring.forget_deliveries();
        let failed = Arc::new(AtomicBool::new(false));
        let stream = build_input_stream(&device, &config, Arc::clone(&ring), Arc::clone(&failed))?;
        stream.play().map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot start audio input {name}: {error}"))
        })?;
        Ok(Self {
            _stream: stream,
            facts: AudioInputFacts::new(
                name,
                config.sample_rate(),
                config.channels(),
                sample_format,
            ),
            ring,
            failed,
        })
    }

    pub fn name(&self) -> &str {
        self.facts.device_id()
    }

    /// The ring this input writes into.
    pub fn ring(&self) -> &Arc<crate::input::InputRing> {
        &self.ring
    }

    pub fn facts(&self) -> &AudioInputFacts {
        &self.facts
    }

    pub fn channels(&self) -> usize {
        self.ring.channels()
    }

    /// The loudest sample since the last take, post-fader.
    pub fn take_peak(&self) -> f32 {
        self.ring.take_peak()
    }

    pub fn set_gain(&self, gain: f32) {
        self.ring.set_gain(gain);
    }

    pub fn gain(&self) -> f32 {
        self.ring.gain()
    }

    /// Whether the stream reported an error since it opened.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

/// An input stream feeding the ring, in whatever sample format the device
/// speaks, converted to f32 a chunk at a time on the stack.
/// The device's configuration at `rate` with the default's channels and
/// sample format, when it offers one.
fn input_config_at_rate(
    device: &cpal::Device,
    default: &cpal::SupportedStreamConfig,
    rate: u32,
) -> Option<cpal::SupportedStreamConfig> {
    let ranges: Vec<_> = device.supported_input_configs().ok()?.collect();
    ranges
        .iter()
        .filter(|range| range.min_sample_rate() <= rate && rate <= range.max_sample_rate())
        .find(|range| {
            range.channels() == default.channels()
                && range.sample_format() == default.sample_format()
        })
        .or_else(|| {
            ranges.iter().find(|range| {
                range.min_sample_rate() <= rate
                    && rate <= range.max_sample_rate()
                    && range.channels() == default.channels()
            })
        })
        .map(|range| range.with_sample_rate(rate))
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ring: Arc<crate::input::InputRing>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, DevicePlaybackError> {
    macro_rules! build {
        ($sample:ty) => {
            build_typed_input_stream::<$sample>(device, config.clone().into(), ring, failed)
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(U24),
        SampleFormat::U32 => build!(u32),
        format => Err(DevicePlaybackError::Unavailable(format!(
            "audio input sample format {format} is not supported"
        ))),
    }
}

/// Whether a stream error fails the stream. An xrun is a glitch the host
/// recovers from; any other kind, a rerouted default device included, has
/// the owner open the stream again or report the failure.
fn fails_stream(error: &cpal::Error) -> bool {
    error.kind() != cpal::ErrorKind::Xrun
}

/// The error callback of a stream whose owner watches `failed`.
fn flag_stream_failure(failed: Arc<AtomicBool>) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if fails_stream(&error) {
            failed.store(true, Ordering::Release);
        }
    }
}

/// The live stream's error callback: an error that fails the stream goes to
/// its [`CallbackGate`].
fn live_error_callback(
    gate: Arc<CallbackGate>,
    failed: Arc<AtomicBool>,
    errors: Arc<AtomicU64>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if fails_stream(&error) {
            gate.report_error(&failed, &errors);
        }
    }
}

fn build_typed_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ring: Arc<crate::input::InputRing>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, DevicePlaybackError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                // The whole delivery, before it is written in pieces: the
                // reader's lag follows the delivery.
                ring.note_delivery((data.len() / channels) as u64);
                let mut chunk = [0.0f32; 4096];
                for frames in data.chunks(chunk.len() / channels * channels) {
                    for (slot, sample) in chunk.iter_mut().zip(frames) {
                        *slot = f32::from_sample(*sample);
                    }
                    ring.write(&chunk[..frames.len()], channels);
                }
            },
            // A device that goes is not silence to be lived with: the
            // owner sees the flag and opens the input again.
            flag_stream_failure(failed),
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot build audio input stream: {error}"))
        })
}

fn find_output_device(
    host: &cpal::Host,
    wanted: &str,
) -> Result<cpal::Device, DevicePlaybackError> {
    let candidates = host
        .output_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .filter_map(|device| {
            device.id().ok().map(|id| {
                let friendly = device
                    .description()
                    .ok()
                    .map(|description| description.name().to_owned())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_default();
                (id.to_string(), friendly, device)
            })
        })
        .collect::<Vec<_>>();
    let matched = match_listed_device(&candidates, wanted).ok_or_else(|| {
        DevicePlaybackError::Unavailable(format!("no audio output named {wanted:?}"))
    })?;
    Ok(candidates
        .into_iter()
        .nth(matched)
        .expect("matched index is in range")
        .2)
}

/// List output and input devices with their default stream configurations.
///
/// Uses the playback host. Devices with unreadable configurations remain in
/// the list, with no rate or channel count.
pub fn audio_devices() -> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), DevicePlaybackError>
{
    with_preferred_host(|host| list_audio_devices(host, AudioDeviceDetails::DefaultConfig))
}

/// List output and input device metadata without requesting default stream configurations.
///
/// Uses the playback host and leaves rate and channel count absent. Use this
/// for periodic discovery. A backend can still inspect capabilities during
/// enumeration, but Rustel does not request a default configuration per device.
pub fn audio_device_inventory()
-> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), DevicePlaybackError> {
    with_preferred_host(|host| list_audio_devices(host, AudioDeviceDetails::Metadata))
}

#[derive(Clone, Copy)]
enum AudioDeviceDetails {
    Metadata,
    DefaultConfig,
}

fn list_audio_devices<H: HostTrait>(
    host: &H,
    details: AudioDeviceDetails,
) -> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), DevicePlaybackError> {
    let describe = |device: &H::Device, default_name: Option<&str>, input: bool| {
        let id = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "?".into());
        // The backend id is a handle, not a name a musician reads. Prefer
        // the friendly name, falling back to the id when none is known.
        let friendly = device
            .description()
            .ok()
            .map(|description| description.name().to_owned())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let config = match details {
            AudioDeviceDetails::Metadata => None,
            AudioDeviceDetails::DefaultConfig if input => device.default_input_config().ok(),
            AudioDeviceDetails::DefaultConfig => device.default_output_config().ok(),
        };
        AudioDeviceInfo {
            is_default: default_name == Some(id.as_str()),
            sample_rate: config.as_ref().map(|c| c.sample_rate()),
            channels: config.as_ref().map(|c| c.channels()),
            name: friendly,
            id,
        }
    };
    let default_out = host
        .default_output_device()
        .and_then(|d| d.id().ok().map(|id| id.to_string()));
    let default_in = host
        .default_input_device()
        .and_then(|d| d.id().ok().map(|id| id.to_string()));

    let mut outputs = host
        .output_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .map(|device| describe(&device, default_out.as_deref(), false))
        .collect::<Vec<_>>();
    let mut inputs = host
        .input_devices()
        .map_err(|error| DevicePlaybackError::Unavailable(error.to_string()))?
        .map(|device| describe(&device, default_in.as_deref(), true))
        .collect::<Vec<_>>();
    disambiguate_device_names(&mut outputs);
    disambiguate_device_names(&mut inputs);
    Ok((outputs, inputs))
}

/// Match a wanted selector against enumerated `(id, friendly, device)` rows.
///
/// Order is the contract the picker and `rustel devices` rely on: the stable
/// id, then the friendly name, then the disambiguated `name (id)` spelling
/// the list prints for a repeated name, then a case-insensitive substring
/// of the friendly name. Input and output used to disagree about the
/// case-insensitive exact-name step, so `mic` could steal `Microphone`
/// ahead of `Mic` on the input side.
fn match_listed_device<T>(candidates: &[(String, String, T)], wanted: &str) -> Option<usize> {
    candidates
        .iter()
        .position(|(id, _, _)| id == wanted)
        .or_else(|| {
            candidates
                .iter()
                .position(|(id, _, _)| id.eq_ignore_ascii_case(wanted))
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(_, friendly, _)| friendly == wanted)
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(_, friendly, _)| friendly.eq_ignore_ascii_case(wanted))
        })
        .or_else(|| {
            candidates.iter().position(|(id, friendly, _)| {
                let disambiguated = format!("{friendly} ({id})");
                disambiguated == wanted || disambiguated.eq_ignore_ascii_case(wanted)
            })
        })
        .or_else(|| {
            let needle = wanted.to_lowercase();
            if needle.is_empty() {
                return None;
            }
            candidates.iter().position(|(_, friendly, _)| {
                !friendly.is_empty() && friendly.to_lowercase().contains(&needle)
            })
        })
}

/// Two devices with the same friendly name must still be tellable apart on
/// the picker; the second and later get their stable id appended, which the
/// device finders match back.
fn disambiguate_device_names(list: &mut [AudioDeviceInfo]) {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for device in list.iter_mut() {
        let count = seen.entry(device.name.clone()).or_insert(0);
        if *count > 0 {
            let id = device.id.clone();
            device.name = format!("{name} ({id})", name = device.name);
        }
        *count += 1;
    }
}

impl ScalarDevice {
    pub fn open_default() -> Result<Self, DevicePlaybackError> {
        Self::open_output(None)
    }

    /// Open a named output, or the host default when `selector` is `None`.
    ///
    /// The name is matched exactly against the identifiers [`audio_devices`]
    /// reports, then case-insensitively, then as a substring, so a UI can pass
    /// through what a person picked from a list without normalising it.
    pub fn open_output(selector: Option<&str>) -> Result<Self, DevicePlaybackError> {
        Self::configure_output(Self::discover_output(selector)?)
    }

    fn discover_output(selector: Option<&str>) -> Result<DiscoveredOutput, DevicePlaybackError> {
        let (host_id, device) = with_preferred_host(|host| {
            let device = match selector {
                None => host.default_output_device().ok_or_else(|| {
                    DevicePlaybackError::Unavailable("no default audio output device".into())
                })?,
                Some(wanted) => find_output_device(host, wanted)?,
            };
            Ok((host.id().to_string().into_boxed_str(), device))
        })?;
        let name = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "default-output".into());
        Ok(DiscoveredOutput {
            device,
            host_id,
            name,
        })
    }

    fn configure_output(output: DiscoveredOutput) -> Result<Self, DevicePlaybackError> {
        let DiscoveredOutput {
            device,
            host_id,
            name,
        } = output;
        let config = device.default_output_config().map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot read default output configuration for {name}: {error}"
            ))
        })?;
        if config.channels() == 0 || config.sample_rate() == 0 {
            return Err(DevicePlaybackError::Unavailable(format!(
                "audio output {name} reported an invalid {}/{} configuration",
                config.sample_rate(),
                config.channels()
            )));
        }
        Ok(Self {
            device,
            config,
            host_id,
            name,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate()
    }

    pub fn channels(&self) -> u16 {
        self.config.channels()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Play a finite prepared event list, observing the caller's independent
    /// Stop flag while the callback runs.
    pub fn play(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples(events, duration_secs, stopped, Vec::new())
    }

    /// [`Self::play`] with decoded library samples installed into the fresh
    /// backend first. This is the finite path's equivalent of the live
    /// install ring. Without the samples, the device backend knows only
    /// the bundled `bd`.
    pub fn play_with_samples(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples_and_dispatch(
            events,
            duration_secs,
            stopped,
            samples,
            DspDispatch::automatic(),
        )
    }

    /// Finite playback with one selection shared by voice kernels and prepared
    /// orbit/stage reverbs. No dispatch decision is made in the callback.
    pub fn play_with_samples_and_dispatch(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
        dispatch: DspDispatch,
    ) -> Result<(), DevicePlaybackError> {
        self.play_with_samples_dispatch_and_polyphony(
            events,
            duration_secs,
            stopped,
            samples,
            dispatch,
            crate::MAX_POLYPHONY,
        )
    }

    /// Finite playback with the same configurable voice policy as live audio.
    pub fn play_with_samples_dispatch_and_polyphony(
        &self,
        events: &[OnsetEvent],
        duration_secs: f64,
        stopped: &AtomicBool,
        samples: Vec<(crate::sample::SampleId, Box<crate::sample::DecodedSample>)>,
        dispatch: DspDispatch,
        max_polyphony: usize,
    ) -> Result<(), DevicePlaybackError> {
        let total_frames = checked_frames(duration_secs, self.sample_rate())?;
        if total_frames == 0 {
            return Ok(());
        }
        if stopped.load(Ordering::Acquire) {
            return Err(DevicePlaybackError::Cancelled);
        }

        let mut backend = ScalarBackend::with_dispatch(dispatch);
        backend.set_max_polyphony(max_polyphony);
        backend.init(self.sample_rate()).map_err(|message| {
            DevicePlaybackError::Unavailable(format!("cannot initialise scalar backend: {message}"))
        })?;
        for (id, decoded) in samples {
            let _ = backend.install_sample(id, decoded);
        }
        // The event list is complete up front: synthesise every needed orbit
        // reverb HERE, off the callback, so activation never generates.
        let mut stage_reverbs: Vec<crate::reverb::ReverbParams> = Vec::new();
        for event in events {
            if let Some(reverb) = event.controls.reverb {
                let orbit = usize::from(event.controls.orbit).min(crate::scalar::MAX_ORBITS - 1);
                let params = crate::reverb::ReverbParams {
                    ir: None,
                    size_secs: reverb.size_secs,
                    fade_secs: reverb.fade_secs,
                    lp_start_hz: reverb.lp_start_hz,
                    lp_end_hz: reverb.lp_end_hz,
                };
                if backend.orbit_reverb_params(orbit) != Some(params) {
                    let _ = backend.install_reverb(
                        orbit,
                        Box::new(crate::reverb::OrbitReverb::generate_with_dispatch(
                            self.sample_rate(),
                            params,
                            dispatch,
                        )),
                    );
                }
            }
            // An `.FX()` stage's reverb is per VOICE and its impulse response
            // comes from that hap's own roomsize, so it cannot be one of the
            // fixed per-orbit set. Synthesise the distinct ones here, off the
            // callback, for the same reason: 2.7 ms at roomsize 0.5 and 31 ms
            // at 6, against the 2.7 ms a 128-frame callback has.
            for stage in event.controls.fx_stages.iter().flatten() {
                let Some(room) = stage.room else { continue };
                let params = crate::reverb::ReverbParams {
                    ir: room.ir,
                    size_secs: room.size_secs,
                    fade_secs: room.fade_secs,
                    lp_start_hz: room.lp_start_hz,
                    lp_end_hz: room.lp_end_hz,
                };
                if !stage_reverbs.contains(&params) {
                    stage_reverbs.push(params);
                    let _ = backend.install_fx_reverb(Box::new(
                        crate::reverb::OrbitReverb::generate_streaming_with_dispatch(
                            self.sample_rate(),
                            params,
                            dispatch,
                        ),
                    ));
                }
            }
        }
        backend.forbid_inline_reverb();
        for event in events {
            backend.note(*event);
        }

        let written = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = build_stream(
            &self.device,
            &self.config,
            backend,
            total_frames,
            written.clone(),
            failed.clone(),
        )?;
        stream.play().map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot start audio output {}: {error}",
                self.name
            ))
        })?;

        // PulseAudio may accept the complete timeline into its server buffer
        // much faster than the speaker consumes it. Keep the stream alive
        // until both submission and real elapsed time reach the requested
        // window so queued audio is not truncated.
        let started = Instant::now();
        let audible_window = Duration::from_secs_f64(duration_secs);
        let progress_deadline = started
            .checked_add(audible_window.saturating_add(Duration::from_secs(3)))
            .ok_or_else(|| {
                DevicePlaybackError::Unavailable(
                    "device playback deadline is not representable".into(),
                )
            })?;
        while written.load(Ordering::Acquire) < total_frames || started.elapsed() < audible_window {
            if stopped.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Cancelled);
            }
            if failed.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Unavailable(format!(
                    "audio output {} failed while playing",
                    self.name
                )));
            }
            if Instant::now() >= progress_deadline {
                return Err(DevicePlaybackError::Unavailable(format!(
                    "audio output {} stopped requesting frames",
                    self.name
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        // The callback has filled the final host buffer, which is still queued
        // for the device. Keep the stream alive for one conservative block,
        // while still observing cancellation and device failure. This is a
        // smoke path, not a latency measurement.
        let drain_until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < drain_until {
            if stopped.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Cancelled);
            }
            if failed.load(Ordering::Acquire) {
                return Err(DevicePlaybackError::Unavailable(format!(
                    "audio output {} failed while draining",
                    self.name
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

fn checked_frames(duration_secs: f64, sample_rate: u32) -> Result<u64, DevicePlaybackError> {
    let frames = duration_secs * f64::from(sample_rate);
    if !frames.is_finite() || frames < 0.0 || frames > u64::MAX as f64 {
        return Err(DevicePlaybackError::Unavailable(format!(
            "device duration {duration_secs}s is not representable at {sample_rate}Hz"
        )));
    }
    Ok(frames.round() as u64)
}

/// Run `use_host` on the audio host. Prefer PulseAudio when available
/// (WSLg and many desktops have no raw ALSA device), else CPAL's default.
///
/// The PulseAudio host is created once and reused. A dropped connection
/// leaks its reader thread and a 1 MiB buffer, and devices are listed
/// every few seconds. When a call fails, the function requests the device
/// list as a health probe. If the probe also fails, the function drops
/// the host and the next call reconnects. A missing device selector does
/// not discard a working connection.
#[cfg(target_os = "linux")]
fn with_preferred_host<T>(
    use_host: impl FnOnce(&cpal::Host) -> Result<T, DevicePlaybackError>,
) -> Result<T, DevicePlaybackError> {
    static PULSE: std::sync::Mutex<Option<cpal::Host>> = std::sync::Mutex::new(None);
    if cpal::available_hosts().contains(&cpal::HostId::PulseAudio) {
        let mut pulse = PULSE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pulse.is_none() {
            *pulse = cpal::host_from_id(cpal::HostId::PulseAudio).ok();
        }
        if let Some(host) = pulse.as_ref() {
            let result = use_host(host);
            return finish_cached_host_request(&mut pulse, result, |host| host.devices().is_err());
        }
    }
    use_host(&cpal::default_host())
}

#[cfg(any(target_os = "linux", test))]
fn finish_cached_host_request<H, T>(
    host: &mut Option<H>,
    result: Result<T, DevicePlaybackError>,
    disconnected: impl FnOnce(&H) -> bool,
) -> Result<T, DevicePlaybackError> {
    if result.is_err() && host.as_ref().is_some_and(disconnected) {
        *host = None;
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn with_preferred_host<T>(
    use_host: impl FnOnce(&cpal::Host) -> Result<T, DevicePlaybackError>,
) -> Result<T, DevicePlaybackError> {
    use_host(&cpal::default_host())
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    backend: ScalarBackend,
    total_frames: u64,
    written: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, DevicePlaybackError> {
    macro_rules! build {
        ($sample:ty) => {
            build_typed_stream::<$sample>(
                device,
                config.clone().into(),
                backend,
                total_frames,
                written,
                failed,
            )
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        format => Err(DevicePlaybackError::Unavailable(format!(
            "audio output sample format {format} is not supported by the scalar smoke backend"
        ))),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut backend: ScalarBackend,
    total_frames: u64,
    written: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, DevicePlaybackError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                write_output(&mut backend, output, channels, total_frames, &written);
            },
            flag_stream_failure(failed),
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!("cannot build audio output stream: {error}"))
        })
}

fn build_live_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    buffer_frames: u32,
    backend: LiveScalarBackend,
    shared: LiveShared,
    gate: Arc<CallbackGate>,
) -> Result<cpal::Stream, DevicePlaybackError> {
    macro_rules! build {
        ($sample:ty) => {
            build_live_typed_stream::<$sample>(
                device,
                config.clone().into(),
                buffer_frames,
                backend,
                shared,
                gate,
            )
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        format => Err(DevicePlaybackError::Unavailable(format!(
            "audio output sample format {format} is not supported by live scalar playback"
        ))),
    }
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

// SAFETY: see the type's documentation. Under cpal =0.18.2's WASAPI host the
// handle is created, used and reverted on the `cpal_wasapi_out` render
// thread, and only the empty value is sent there; a cpal upgrade or another
// Windows host (ASIO) must recheck that ownership.
#[cfg(windows)]
unsafe impl Send for CallbackProAudio {}

#[cfg(windows)]
impl CallbackProAudio {
    /// Register the calling thread on the first call only. A method rather
    /// than a field access in the closure: the closure must capture the
    /// whole `Send` wrapper, not the field inside it.
    fn attach_once(&mut self) {
        if self.0.is_none() {
            self.0 = Some(crate::mmcss::ProAudioThread::attach_critical());
        }
    }
}

fn build_live_typed_stream<T>(
    device: &cpal::Device,
    mut config: cpal::StreamConfig,
    buffer_frames: u32,
    mut backend: LiveScalarBackend,
    shared: LiveShared,
    gate: Arc<CallbackGate>,
) -> Result<cpal::Stream, DevicePlaybackError>
where
    T: SizedSample + FromSample<f32>,
{
    // Default PulseAudio buffering accepted tens of seconds of PCM in a few
    // wall-clock seconds on WSLg. That made both edits and the scheduler clock
    // race far ahead of what was audible. A fixed period asks CPAL backends for
    // a bounded live buffer (Pulse uses a two-period end-to-end target).
    config.buffer_size = BufferSize::Fixed(buffer_frames);
    let channels = usize::from(config.channels);
    let on_error = live_error_callback(
        Arc::clone(&gate),
        shared.failed.clone(),
        shared.callback_errors.clone(),
    );
    let mut playback_origin = None;
    // Callback-owned meter state. It belongs to the stream, not to
    // `LiveShared`, because its filters are tuned to this stream's rate.
    let sample_rate = config.sample_rate;
    let mut meters = LiveCallbackMeters::new(sample_rate, &shared.realtime_load);
    // The callback's thread is CPAL's; the one place to register it with
    // MMCSS is its first call. Held for the stream's life, reverted when
    // the closure drops with the stream.
    #[cfg(windows)]
    let mut pro_audio = CallbackProAudio(None);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], info| {
                #[cfg(windows)]
                pro_audio.attach_once();
                gate.render_or_silence(output, |output| {
                    let stop_requested = shared.stopped.load(Ordering::Acquire);
                    let playback = info.timestamp().playback;
                    let origin = *playback_origin.get_or_insert(playback);
                    let origin_nanos = u64::try_from(origin.as_nanos()).unwrap_or(u64::MAX - 1);
                    shared
                        .playback_origin_nanos
                        .store(origin_nanos, Ordering::Release);
                    let elapsed = playback.saturating_duration_since(origin);
                    let elapsed_nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
                    let latency_nanos = u64::try_from(
                        playback
                            .saturating_duration_since(info.timestamp().callback)
                            .as_nanos(),
                    )
                    .unwrap_or(u64::MAX);
                    shared
                        .buffer_playback_nanos
                        .store(elapsed_nanos, Ordering::Release);
                    shared
                        .playback_latency_nanos
                        .store(latency_nanos, Ordering::Release);
                    shared
                        .max_playback_latency_nanos
                        .fetch_max(latency_nanos, Ordering::Relaxed);
                    write_live_output(&mut backend, &mut meters, output, channels, &shared);
                    if stop_requested && backend.stop_ramp_complete() {
                        shared.stop_acknowledged.store(true, Ordering::Release);
                    }
                });
            },
            on_error,
            Some(Duration::from_secs(3)),
        )
        .map_err(|error| {
            DevicePlaybackError::Unavailable(format!(
                "cannot build live audio output stream: {error}"
            ))
        })
}

fn write_live_output<T>(
    backend: &mut LiveScalarBackend,
    meters: &mut LiveCallbackMeters,
    output: &mut [T],
    channels: usize,
    shared: &LiveShared,
) where
    T: Sample + FromSample<f32>,
{
    write_live_output_inner(backend, meters, output, channels, shared, true, true);
}

#[cfg_attr(test, inline(never))]
fn write_live_output_inner<T>(
    backend: &mut LiveScalarBackend,
    meters: &mut LiveCallbackMeters,
    output: &mut [T],
    channels: usize,
    shared: &LiveShared,
    measure_load: bool,
    measure_pressure: bool,
) where
    T: Sample + FromSample<f32>,
{
    const BLOCK: usize = 128;
    let entered_nanos = shared.clock_base.elapsed().as_nanos() as u64;
    if !tripwire::in_audio_scope() {
        shared.callback_scope_misses.fetch_add(1, Ordering::Relaxed);
    }
    let silence = T::from_sample(0.0);
    output.fill(silence);
    if channels == 0 {
        return;
    }
    let frame_count = output.len() / channels;
    // Monotonic callback frames, not CPAL's playback timestamp. See
    // `LiveScalarDevice::clock_nanos`.
    let start = shared.frames.load(Ordering::Acquire);
    let generation_at_entry = shared.generation.load(Ordering::Acquire);
    // Pointer writes only: adopt freshly decoded samples into the bank and
    // hand displaced ones back for the producer to free.
    backend.drain_sample_installs(&shared.samples);
    backend.drain_reverb_installs(&shared.samples);
    for update in shared.live_controls.drain_available() {
        backend.set_live_control(update);
    }
    let mut scratch = [0.0f32; BLOCK * 2];
    let mut rendered = 0usize;
    let mut callback_late = 0u64;
    let mut callback_refused = 0u64;
    // Routing set by the producer reaches the backend at a block edge.
    let mut pairs = [0u8; crate::scalar::MAX_ORBITS];
    for (slot, pair) in pairs.iter_mut().zip(shared.orbit_pairs.iter()) {
        *slot = pair.load(Ordering::Relaxed);
    }
    backend.set_orbit_pairs(pairs);
    let pairs_available = channels / 2;
    // Asked for between blocks: silence what is sounding before this one
    // is filled, so a swapped audition stops rather than ringing on under
    // the next.
    if shared.cut_sounding.swap(false, Ordering::AcqRel) {
        backend.cut_sounding_voices(start);
    }
    while rendered < frame_count {
        let count = (frame_count - rendered).min(BLOCK);
        let end_frame = start
            .saturating_add(rendered as u64)
            .saturating_add(count as u64);
        // Publish only the chunk about to render, not the entire host
        // buffer: process_block rechecks generation at every chunk edge.
        // The completed/certified frontier below remains untouched.
        shared
            .rendering_until_frame
            .store(end_frame, Ordering::Release);
        backend.set_max_polyphony(shared.max_polyphony.load(Ordering::Relaxed));
        let visual_analysis_mask = shared.visual_analysis_mask.load(Ordering::Acquire);
        let visual_generation_floor = shared
            .visual_analysis_generation_floor
            .load(Ordering::Acquire);
        backend.set_ui_visual_capture_mask(visual_analysis_mask);
        backend.set_ui_visual_capture_generation_floor(visual_generation_floor);
        // The mixer's orbit faders, read once a block: sixteen loads.
        let mut orbit_gains = [1.0f32; crate::scalar::MAX_ORBITS];
        for (gain, slot) in orbit_gains.iter_mut().zip(shared.orbit_gains.iter()) {
            *gain = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        backend.set_orbit_gains(&orbit_gains);
        let immediate = backend.admit_immediate(
            &shared.immediate,
            start.saturating_add(rendered as u64),
            count,
        );
        let mut report = backend.process_block_with(
            &mut scratch[..count * 2],
            count,
            start.saturating_add(rendered as u64),
            &shared.ring,
            shared.flip_atomics(),
            &shared.stopped,
        );
        report.accepted += immediate.accepted;
        report.refused += immediate.refused;
        // Visuals follow the score independently of listening volume and
        // the output limiter.
        shared
            .analysis
            .write_stereo(&scratch[..count * 2], end_frame);
        // Metering and the device see the same post-fader signal.
        let gain = shared.meter.gain();
        if gain != 1.0 {
            for sample in &mut scratch[..count * 2] {
                *sample *= gain;
            }
        }
        // What the meter would have said before the limiter touched it. CLIP
        // means "this score is asking for more than full scale", and a ceiling
        // below full scale would otherwise make it permanently dark and turn
        // the indicator into a lie.
        shared.meter.observe_unlimited(&scratch[..count * 2]);
        // The safety limiter, between the fader and every consumer of the
        // output buffer: the meter and the recorder tap both see
        // what the device is about to be handed.
        if let Some(settings) = shared.meter.limiter_settings() {
            // Coming back from off, the delay line still holds whatever
            // was passing through when it was switched off - a burst of
            // stale audio, then the runway re-filling. Turning a limiter
            // on should not play something from a minute ago.
            if !meters.limiter_running {
                meters.limiter.reset();
                meters.limiter_running = true;
            }
            meters.limiter.set_threshold_db(settings.threshold_db);
            meters.limiter.set_character(settings.character);
            meters.limiter.process_stereo(&mut scratch[..count * 2]);
            // The ceiling back up to full scale, when asked for. Without
            // it, switching the limiter on makes a set quieter by exactly
            // the headroom it was given, which reads as the limiter having
            // broken the volume rather than having changed the sound. The
            // gain is `1 / ceiling` and nothing else: the limiter has
            // already promised nothing leaves above the ceiling, so this
            // lands the ceiling on full scale and cannot pass it.
            let makeup = shared.meter.limiter_makeup_gain();
            if makeup != 1.0 {
                for sample in &mut scratch[..count * 2] {
                    *sample *= makeup;
                }
            }
            shared
                .meter
                .publish_reduction(meters.limiter.take_reduction());
        } else {
            // Off is a character, not an absence of protection. Nothing may
            // reach a float device above full scale, and no NaN may reach it
            // at all - the integer formats saturate on their own, `f32` and
            // `f64` do not.
            for sample in &mut scratch[..count * 2] {
                *sample = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
            }
            meters.limiter_running = false;
            shared.meter.publish_reduction(1.0);
        }
        meters
            .master
            .observe_stereo(&scratch[..count * 2], &shared.meter);
        for (slot, peak) in shared.orbit_peaks.iter().zip(backend.take_orbit_peaks()) {
            shared
                .score_peak
                .fetch_max(peak.to_bits(), Ordering::Relaxed);
            // Non-negative floats order as their bits do.
            slot.fetch_max((peak * gain).to_bits(), Ordering::Relaxed);
        }
        shared.record.write_stereo(&scratch[..count * 2]);
        let visual_generation_current = backend.generation()
            == shared.generation.load(Ordering::Acquire)
            && visual_generation_floor
                == shared
                    .visual_analysis_generation_floor
                    .load(Ordering::Acquire);
        let current_visual_mask = shared.visual_analysis_mask.load(Ordering::Acquire);
        let mut slots = visual_analysis_mask;
        while slots != 0 {
            let slot = slots.trailing_zeros() as usize;
            if visual_generation_current
                && current_visual_mask & (1_u64 << slot) != 0
                && let Some(tap) = shared.visual_analysis_tap(slot)
            {
                tap.write_stereo(backend.ui_visual_mix(slot, count), end_frame);
            }
            slots &= slots - 1;
        }
        shared
            .refused_voices
            .fetch_add(report.refused as u64, Ordering::Relaxed);
        shared
            .accepted_events
            .fetch_add(report.accepted as u64, Ordering::Relaxed);
        shared
            .stale_events
            .fetch_add(report.stale as u64, Ordering::Relaxed);
        shared
            .late_events
            .fetch_add(report.late as u64, Ordering::Relaxed);
        callback_late = callback_late.saturating_add(report.late as u64);
        callback_refused = callback_refused.saturating_add(report.refused as u64);
        // Route whenever any orbit is sent off the main pair; a pair the
        // device does not have falls back to the main pair rather than
        // vanishing (which it did when this was gated on pairs_available).
        let routed = backend.routing_active();
        for frame in 0..count {
            let output_frame =
                &mut output[(rendered + frame) * channels..(rendered + frame + 1) * channels];
            let left = scratch[frame * 2];
            let right = scratch[frame * 2 + 1];
            if channels == 1 {
                output_frame[0] = T::from_sample((left + right) * 0.5);
            } else if routed {
                // Orbits go to their pairs; a pair nobody is routed to is
                // silent, and the main pair carries the rest. Summed in f32
                // first, on the stack: the callback allocates nothing.
                let mut mixed = [0.0f32; MAX_OUTPUT_CHANNELS];
                mixed[0] = left;
                mixed[1] = right;
                for (orbit, pair) in pairs.iter().enumerate() {
                    let pair = usize::from(*pair);
                    if pair == 0 {
                        continue;
                    }
                    // A pair the device does not have is the main pair, so
                    // the orbit is heard rather than dropped.
                    let dest = if pair >= pairs_available || pair * 2 + 1 >= MAX_OUTPUT_CHANNELS {
                        0
                    } else {
                        pair
                    };
                    let mix = backend.orbit_mix(orbit, count);
                    if mix.len() < (frame + 1) * 2 {
                        continue;
                    }
                    mixed[dest * 2] += mix[frame * 2] * gain;
                    mixed[dest * 2 + 1] += mix[frame * 2 + 1] * gain;
                }
                for (channel, sample) in output_frame.iter_mut().enumerate() {
                    *sample = T::from_sample(mixed.get(channel).copied().unwrap_or(0.0));
                }
            } else {
                output_frame[0] = T::from_sample(left);
                output_frame[1] = T::from_sample(right);
                let center = T::from_sample((left + right) * 0.5);
                for sample in &mut output_frame[2..] {
                    *sample = center;
                }
            }
        }
        // DSP is not consumption: only this completed host-slice copy can
        // certify the actual generation selected for the sub-block.
        backend.confirmation_copied(start.saturating_add(rendered as u64), end_frame);
        rendered += count;
    }
    shared
        .frames
        .fetch_add(frame_count as u64, Ordering::Release);
    shared.callbacks.fetch_add(1, Ordering::Release);
    shared
        .score_sources_active
        .store(backend.score_sources_active(), Ordering::Release);
    // Host-starvation telemetry: the largest gap between callback entries,
    // and the largest render inside one. Instant::now + atomics only - no
    // allocation inside the callback.
    let now_nanos = shared.clock_base.elapsed().as_nanos() as u64;
    let busy_nanos = now_nanos.saturating_sub(entered_nanos);
    if measure_load {
        meters.load.observe(
            frame_count,
            busy_nanos,
            meters.sample_rate,
            &shared.realtime_load,
        );
    }
    if measure_pressure {
        meters.pressure.observe(
            backend.pressure_observation(),
            callback_late,
            callback_refused,
            &shared.realtime_pressure,
        );
    }
    shared
        .max_callback_busy_nanos
        .fetch_max(busy_nanos, Ordering::AcqRel);
    let previous = shared.last_callback_nanos.swap(now_nanos, Ordering::AcqRel);
    if previous != 0 {
        let gap = now_nanos.saturating_sub(previous);
        shared
            .max_callback_gap_nanos
            .fetch_max(gap, Ordering::AcqRel);
    }
    if shared.generation.load(Ordering::Acquire) != generation_at_entry {
        shared.cutover_race_blocks.fetch_add(1, Ordering::Relaxed);
    }
}

fn write_output<T>(
    backend: &mut ScalarBackend,
    output: &mut [T],
    channels: usize,
    total_frames: u64,
    written: &AtomicU64,
) where
    T: Sample + FromSample<f32>,
{
    const BLOCK: usize = 128;
    let silence = T::from_sample(0.0);
    output.fill(silence);
    if channels == 0 {
        return;
    }

    let start = written.load(Ordering::Relaxed);
    let available = total_frames.saturating_sub(start) as usize;
    let frames = (output.len() / channels).min(available);
    let mut scratch = [0.0f32; BLOCK * 2];
    let mut rendered = 0usize;
    while rendered < frames {
        let count = (frames - rendered).min(BLOCK);
        backend.process_block(&mut scratch[..count * 2], count);
        for frame in 0..count {
            let output_frame =
                &mut output[(rendered + frame) * channels..(rendered + frame + 1) * channels];
            let left = scratch[frame * 2];
            let right = scratch[frame * 2 + 1];
            if channels == 1 {
                output_frame[0] = T::from_sample((left + right) * 0.5);
            } else {
                output_frame[0] = T::from_sample(left);
                output_frame[1] = T::from_sample(right);
                let center = T::from_sample((left + right) * 0.5);
                for sample in &mut output_frame[2..] {
                    *sample = center;
                }
            }
        }
        rendered += count;
    }
    written.fetch_add(frames as u64, Ordering::Release);
}

#[cfg(test)]
mod tests {
    // The allocation counters are process-wide, so a positive-control canary
    // must not overlap another test's zero-delta assertion.
    static TRIPWIRE_ASSERTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn tripwire_assertion_guard() -> std::sync::MutexGuard<'static, ()> {
        TRIPWIRE_ASSERTION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Callback metering state for the direct `write_live_output` tests. The
    /// meter is per-stream in production; here each call gets a fresh one.
    fn test_meter() -> LiveCallbackMeters {
        LiveCallbackMeters::new(48_000, &RealtimeLoadPublisher::new())
    }

    use super::*;

    fn confirmation_offer(
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

    fn confirmation_event(
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

    fn confirmation_backend(shared: &LiveShared, voices: usize) -> LiveScalarBackend {
        let mut backend = LiveScalarBackend::with_dispatch(48_000, voices, DspDispatch::portable())
            .expect("prepared backend");
        backend.set_confirmations(shared.confirmations.clone());
        backend
    }

    mod analysis {
        use super::*;

        #[test]
        fn master_volume_changes_output_without_changing_visuals() {
            let _tripwire_guard = tripwire_assertion_guard();
            let render = |gain| {
                let shared = LiveShared::new(1);
                shared.analysis.set_enabled(true);
                shared.set_visual_analysis_mask(1);
                shared.meter.set_gain(gain);
                assert!(shared.ring.push(AudioEvent {
                    onset_id: 1,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 440.0,
                    gain: 0.8,
                    duration_secs: 0.1,
                    ui_visuals: 1,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
                let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
                let mut output = [0.0f32; 128 * 2];
                let mut meters = test_meter();
                let before = tripwire::Violations::capture();
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(tripwire::Violations::capture().since(before).clean());
                let mut master = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
                let mut visual = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
                let mut sides = [(0.0, 0.0); LIVE_ANALYSIS_SIDES_SAMPLES];
                shared.analysis.copy_latest(&mut master).expect("master");
                assert!(shared.analysis.copy_latest_sides(&mut sides));
                shared
                    .visual_analysis_tap(0)
                    .expect("visual tap")
                    .copy_latest(&mut visual)
                    .expect("visual");
                (output, master, visual, sides)
            };

            let (reference, master, visual, sides) = render(1.0);
            assert!(reference.iter().any(|sample| sample.abs() > 1e-5));
            assert!(master.iter().any(|sample| sample.abs() > 1e-5));
            assert!(visual.iter().any(|sample| sample.abs() > 1e-5));
            for gain in [0.0, 0.25, 4.0] {
                let (output, actual_master, actual_visual, actual_sides) = render(gain);
                assert_eq!(actual_master, master, "master visual at gain {gain}");
                assert_eq!(actual_visual, visual, "receiver visual at gain {gain}");
                assert_eq!(actual_sides, sides, "stereo visual at gain {gain}");
                for (actual, expected) in output.iter().zip(reference) {
                    assert_eq!(*actual, (expected * gain).clamp(-1.0, 1.0));
                }
            }
        }

        #[test]
        fn live_analysis_window_is_zero_padded_post_mix_mono() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.8,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            assert!(tripwire::allocator_is_armed());
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            let violations = tripwire::Violations::capture().since(before);
            assert!(
                violations.clean(),
                "analysis tap violated callback allocation contract: {violations:?}"
            );

            let mut analysis = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let snapshot = shared
                .analysis
                .copy_latest(&mut analysis)
                .expect("published analysis");
            assert_eq!(snapshot.end_frame, 128);
            assert!(
                analysis[..LIVE_ANALYSIS_WINDOW_SAMPLES - 128]
                    .iter()
                    .all(|sample| sample.to_bits() == 0.0f32.to_bits())
            );
            for frame in 0..128 {
                let expected = (output[frame * 2] + output[frame * 2 + 1]) * 0.5;
                assert_eq!(
                    analysis[LIVE_ANALYSIS_WINDOW_SAMPLES - 128 + frame].to_bits(),
                    expected.to_bits(),
                    "analysis diverged from final stereo mix at frame {frame}"
                );
            }
        }

        #[test]
        fn live_visual_analysis_keeps_sibling_receivers_out_of_each_other() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            shared.set_visual_analysis_mask(0b11);
            for (onset_id, frequency, ui_visuals) in [(1, 220.0, 0b01), (2, 440.0, 0b10)] {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: frequency,
                    gain: 0.4,
                    duration_secs: 0.1,
                    ui_visuals,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert!(
                tripwire::Violations::capture().since(before).clean(),
                "per-visual capture allocated in the callback"
            );

            let mut master = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let mut first = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let mut second = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared.analysis.copy_latest(&mut master).expect("master");
            shared
                .visual_analysis_tap(0)
                .expect("first tap")
                .copy_latest(&mut first)
                .expect("first visual");
            shared
                .visual_analysis_tap(1)
                .expect("second tap")
                .copy_latest(&mut second)
                .expect("second visual");
            assert!(first.iter().any(|sample| sample.abs() > 1e-5));
            assert!(second.iter().any(|sample| sample.abs() > 1e-5));
            for ((master, first), second) in master.iter().zip(first).zip(second) {
                assert!((master - first - second).abs() < 1e-6);
            }
        }

        #[test]
        fn visual_analysis_taps_are_allocated_only_for_requested_slots() {
            let shared = LiveShared::new(1);
            assert!(shared.visual_analysis.iter().all(|tap| tap.get().is_none()));

            shared.set_visual_analysis_mask(1_u64 << 7);
            assert_eq!(
                shared
                    .visual_analysis
                    .iter()
                    .filter(|tap| tap.get().is_some())
                    .count(),
                1
            );
            assert!(shared.visual_analysis_tap(7).is_some());
        }

        #[test]
        fn reload_does_not_publish_a_ringing_old_voice_into_the_new_visual() {
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            shared.set_visual_analysis_mask(1);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 1.0,
                ui_visuals: 1,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert!(output.iter().any(|sample| sample.abs() > 1e-5));

            shared.takeover_frame.store(128, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            shared.reset_visual_analysis_mask(1);
            output.fill(0.0);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert!(
                output.iter().any(|sample| sample.abs() > 1e-5),
                "the old voice should still ring in the master mix"
            );

            let mut visual = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared
                .visual_analysis_tap(0)
                .expect("visual tap")
                .copy_latest(&mut visual)
                .expect("new-generation visual frame");
            assert!(
                visual.iter().all(|sample| *sample == 0.0),
                "the old generation leaked into the new receiver"
            );
        }

        #[test]
        fn unchanged_visual_revision_keeps_its_ringing_voice_across_generation() {
            let shared = LiveShared::new(1);
            shared.set_visual_analysis_mask(1);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 1.0,
                ui_visuals: 1,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });

            shared.takeover_frame.store(128, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            output.fill(0.0);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });

            let mut visual = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared
                .visual_analysis_tap(0)
                .expect("visual tap")
                .copy_latest(&mut visual)
                .expect("same-revision visual frame");
            assert!(
                visual[LIVE_ANALYSIS_WINDOW_SAMPLES - 128..]
                    .iter()
                    .any(|sample| sample.abs() > 1e-5),
                "a generation-only cutover discarded the same receiver's tail"
            );
        }

        #[test]
        fn live_analysis_window_survives_ring_wrap_and_keeps_latest_frames() {
            let shared = LiveShared::new(1);
            shared.analysis.set_enabled(true);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.7,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut expected = [0.0f32; LIVE_ANALYSIS_WINDOW_SAMPLES];
            for _ in 0..(LIVE_ANALYSIS_RING_CAPACITY / 128 + 4) {
                let mut output = [0.0f32; 128 * 2];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
                });
                expected.rotate_left(128);
                for frame in 0..128 {
                    expected[LIVE_ANALYSIS_WINDOW_SAMPLES - 128 + frame] =
                        (output[frame * 2] + output[frame * 2 + 1]) * 0.5;
                }
            }

            let mut analysis = [0.0f32; LIVE_ANALYSIS_WINDOW_SAMPLES];
            shared
                .analysis
                .copy_latest(&mut analysis)
                .expect("published analysis");
            for (index, (actual, expected)) in analysis.iter().zip(expected).enumerate() {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "wrapped analysis diverged at sample {index}"
                );
            }
        }

        #[test]
        fn the_master_tap_keeps_left_and_right_for_a_stereo_picture() {
            let tap = LiveAnalysisTap::with_sides();
            tap.set_enabled(true);
            let mut stereo = [0.0f32; 128 * 2];
            for frame in stereo.as_chunks_mut::<2>().0 {
                frame[0] = 0.5;
                frame[1] = -0.25;
            }
            tap.write_stereo(&stereo, 128);
            let mut sides = [(f32::NAN, f32::NAN); LIVE_ANALYSIS_SIDES_SAMPLES];
            assert!(tap.copy_latest_sides(&mut sides));
            assert!(
                sides[..LIVE_ANALYSIS_SIDES_SAMPLES - 128]
                    .iter()
                    .all(|&(left, right)| left == 0.0 && right == 0.0),
                "silence before the ring fills"
            );
            assert!(
                sides[LIVE_ANALYSIS_SIDES_SAMPLES - 128..]
                    .iter()
                    .all(|&(left, right)| left == 0.5 && right == -0.25)
            );
            let mut mono = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];
            tap.copy_latest(&mut mono)
                .expect("the mono window is kept as before");
            assert_eq!(mono[LIVE_ANALYSIS_WINDOW_SAMPLES - 1], 0.125);

            let plain = LiveAnalysisTap::new();
            plain.set_enabled(true);
            plain.write_stereo(&stereo, 128);
            assert!(
                !plain.copy_latest_sides(&mut sides),
                "a visual tap keeps only the mono window"
            );
        }

        #[test]
        fn live_analysis_is_opt_in_and_reenable_starts_a_clean_epoch() {
            let tap = LiveAnalysisTap::new();
            let stereo = [1.0f32; 128 * 2];
            let mut analysis = [f32::NAN; LIVE_ANALYSIS_WINDOW_SAMPLES];

            tap.write_stereo(&stereo, 128);
            assert_eq!(tap.written.load(Ordering::Acquire), 0);
            assert_eq!(tap.copy_latest(&mut analysis), None);

            tap.set_enabled(true);
            tap.write_stereo(&stereo, 256);
            let first = tap.copy_latest(&mut analysis).expect("first epoch");
            assert_eq!(first.end_frame, 256);
            assert!(
                analysis[LIVE_ANALYSIS_WINDOW_SAMPLES - 128..]
                    .iter()
                    .all(|sample| *sample == 1.0)
            );

            tap.set_enabled(false);
            tap.write_stereo(&[0.25; 128 * 2], 384);
            tap.set_enabled(true);
            tap.write_stereo(&[0.5; 128 * 2], 512);
            let second = tap.copy_latest(&mut analysis).expect("second epoch");
            assert!(second.epoch > first.epoch);
            assert_eq!(second.end_frame, 512);
            assert!(
                analysis[..LIVE_ANALYSIS_WINDOW_SAMPLES - 128]
                    .iter()
                    .all(|sample| *sample == 0.0)
            );
            assert!(
                analysis[LIVE_ANALYSIS_WINDOW_SAMPLES - 128..]
                    .iter()
                    .all(|sample| *sample == 0.5)
            );
        }

        #[test]
        fn live_analysis_reader_gives_up_on_an_in_progress_publication() {
            let tap = LiveAnalysisTap::new();
            tap.set_enabled(true);
            tap.publication.store(1, Ordering::Release);
            let mut analysis = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            assert_eq!(tap.copy_latest(&mut analysis), None);
        }

        #[test]
        fn live_analysis_reset_changes_epoch_and_accepts_absolute_frontier() {
            let tap = LiveAnalysisTap::new();
            tap.set_enabled(true);
            tap.write_stereo(&[0.25; 128 * 2], 10_128);
            let mut analysis = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let before = tap.copy_latest(&mut analysis).expect("before reset");

            tap.reset();
            tap.write_stereo(&[0.5; 128 * 2], 10_256);
            let after = tap.copy_latest(&mut analysis).expect("after reset");
            assert!(after.epoch > before.epoch);
            assert!(after.end_frame > before.end_frame);
            assert!(
                analysis[..LIVE_ANALYSIS_WINDOW_SAMPLES - 128]
                    .iter()
                    .all(|sample| *sample == 0.0)
            );
        }

        #[test]
        fn live_analysis_concurrent_snapshots_do_not_mix_publications() {
            let tap = Arc::new(LiveAnalysisTap::new());
            tap.set_enabled(true);
            let writer_tap = Arc::clone(&tap);
            let gate = Arc::new(std::sync::Barrier::new(2));
            let writer_gate = Arc::clone(&gate);
            let writer = std::thread::spawn(move || {
                let mut frontier = 0u64;
                for block in 0..256usize {
                    let frames = if block % 2 == 0 { 64 } else { 128 };
                    let mut stereo = [0.0f32; 128 * 2];
                    for frame in 0..frames {
                        let value = (frontier + frame as u64) as f32;
                        stereo[frame * 2] = value;
                        stereo[frame * 2 + 1] = value;
                    }
                    frontier += frames as u64;
                    writer_tap.write_stereo(&stereo[..frames * 2], frontier);
                    if block == 0 {
                        writer_gate.wait();
                    }
                    std::thread::yield_now();
                }
            });

            let mut analysis = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let mut snapshots = 0usize;
            gate.wait();
            // Bounded by time, not by tries. `copy_latest` declines while a
            // write is in flight, so a loaded machine can lose many tries in
            // a row. The claim is that a reader eventually gets a clean window.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while snapshots < 512 && std::time::Instant::now() < deadline {
                let Some(snapshot) = tap.copy_latest(&mut analysis) else {
                    std::thread::yield_now();
                    continue;
                };
                snapshots += 1;
                let available =
                    snapshot.end_frame.min(LIVE_ANALYSIS_WINDOW_SAMPLES as u64) as usize;
                let padding = LIVE_ANALYSIS_WINDOW_SAMPLES - available;
                assert!(analysis[..padding].iter().all(|sample| *sample == 0.0));
                for (offset, sample) in analysis[padding..].iter().enumerate() {
                    let expected = snapshot.end_frame - available as u64 + offset as u64;
                    assert_eq!(*sample, expected as f32);
                }
            }
            writer.join().expect("analysis writer");
            assert!(snapshots > 0, "concurrent reader never captured a snapshot");
        }
    }
    mod callback_gates {
        use super::*;

        const GATE_BENCH_CHUNK_CALLBACKS: usize = 8;

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum ReceiptBenchMode {
            Disabled,
            Empty,
            Window,
        }

        impl ReceiptBenchMode {
            fn label(self) -> &'static str {
                match self {
                    Self::Disabled => "no-consumer",
                    Self::Empty => "armed-empty-ledger",
                    Self::Window => "zero-event-window-per-chunk",
                }
            }
        }

        struct GateBenchState {
            shared: LiveShared,
            backend: LiveScalarBackend,
            meters: LiveCallbackMeters,
            gate: CallbackGate,
        }

        impl GateBenchState {
            fn new(frames: usize, callbacks: usize) -> Self {
                Self::with_capacity(frames, callbacks, 4)
            }

            fn with_capacity(frames: usize, callbacks: usize, voice_capacity: usize) -> Self {
                let shared = LiveShared::new(1);
                // Prepare only the fixture's capacity, not the full device's
                // thousands of voice/FX states.
                let mut backend = LiveScalarBackend::with_dispatch(
                    48_000,
                    voice_capacity,
                    DspDispatch::portable(),
                )
                .expect("portable backend");
                backend.set_input(Some(Arc::clone(&shared.input)));
                assert!(backend.dispatch().is_forced_portable());
                let duration_secs =
                    frames.checked_mul(callbacks).expect("frame count") as f32 / 48_000.0 + 1.0;
                assert!(shared.ring.push(AudioEvent {
                    onset_id: 1,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.5,
                    duration_secs,
                    ui_visuals: 0,
                    controls: crate::OscillatorControls {
                        envelope: crate::Envelope {
                            attack_secs: 0.001,
                            decay_secs: 0.0,
                            sustain: 1.0,
                            release_secs: 0.01,
                        },
                        ..Default::default()
                    },
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
                let meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
                let gate = CallbackGate::new();
                gate.activate().expect("activate before timing");
                Self {
                    shared,
                    backend,
                    meters,
                    gate,
                }
            }

            fn arm_receipts(&mut self, mode: ReceiptBenchMode) {
                if mode != ReceiptBenchMode::Disabled {
                    self.backend
                        .set_confirmations(self.shared.confirmations.clone());
                }
            }

            fn offer_receipt_chunk(
                &self,
                mode: ReceiptBenchMode,
                chunk: usize,
                frames: usize,
            ) -> Option<crate::confirmation::WindowOffer> {
                if mode != ReceiptBenchMode::Window {
                    return None;
                }
                let start = self.shared.frames.load(Ordering::Acquire);
                let end = start
                    .checked_add(
                        u64::try_from(
                            frames
                                .checked_mul(GATE_BENCH_CHUNK_CALLBACKS)
                                .expect("chunk frames"),
                        )
                        .expect("bounded frame count"),
                    )
                    .expect("window end");
                let token = u64::try_from(chunk)
                    .expect("chunk token")
                    .checked_add(1)
                    .expect("next token");
                let mut offer =
                    confirmation_offer(&self.shared.confirmations, token, 1, start, end, 0);
                offer.takeover_frame = 0;
                assert!(self.shared.confirmations.publish(offer));
                Some(offer)
            }

            fn collect_receipt_chunk(
                &self,
                offered: Option<crate::confirmation::WindowOffer>,
                frames: usize,
            ) {
                if let Some(offer) = offered {
                    let terminal = self
                        .shared
                        .confirmations
                        .pop_terminal()
                        .expect("offered chunk copied");
                    assert_eq!(terminal.key, offer.key);
                    assert_eq!(terminal.generation, offer.generation);
                    assert_eq!(terminal.takeover_frame, offer.takeover_frame);
                    assert_eq!(
                        terminal.outcome,
                        crate::confirmation::WindowOutcome::Confirmed
                    );
                    assert_eq!(terminal.copied_start_frame, offer.start_frame);
                    assert_eq!(
                        terminal.copied_end_frame,
                        offer.start_frame + frames.min(128) as u64
                    );
                }
                assert!(self.shared.confirmations.pop_terminal().is_none());
            }

            fn time_chunk(&mut self, output: &mut [f32], frames: usize, gated: bool) -> u128 {
                // Choose the arm outside timing. Both enter exactly one audio scope
                // and run the same metered callback body and host-buffer copy.
                let gate = std::hint::black_box(&self.gate);
                if gated {
                    let started = Instant::now();
                    for output in output.chunks_exact_mut(frames * 2) {
                        gate.render_or_silence(output, |output| {
                            write_live_output(
                                &mut self.backend,
                                &mut self.meters,
                                output,
                                2,
                                &self.shared,
                            );
                        });
                    }
                    started.elapsed().as_nanos()
                } else {
                    let started = Instant::now();
                    for output in output.chunks_exact_mut(frames * 2) {
                        tripwire::audio_scope(|| {
                            write_live_output(
                                &mut self.backend,
                                &mut self.meters,
                                output,
                                2,
                                &self.shared,
                            );
                        });
                    }
                    started.elapsed().as_nanos()
                }
            }

            fn check(&self, frames: usize, callbacks: usize) {
                self.check_counts(frames, callbacks, 1);
                assert_eq!(self.backend.pressure_observation().active_voices, 1);
            }

            fn check_counts(&self, frames: usize, callbacks: usize, accepted_events: usize) {
                let report = self.shared.report(1);
                assert_eq!(report.callbacks, callbacks as u64);
                assert_eq!(report.submitted_frames, (frames * callbacks) as u64);
                assert_eq!(report.accepted_events, accepted_events as u64);
                assert_eq!(report.ring_depth, 0);
                assert_eq!(report.ring_role_conflicts, 0);
                assert_eq!(report.ring_refusals, 0);
                assert_eq!(report.refused_voices, 0);
                assert_eq!(report.stale_events_filtered, 0);
                assert_eq!(report.late_events, 0);
                assert_eq!(report.callback_errors, 0);
                assert_eq!(report.callback_scope_misses, 0);
                assert!(!self.shared.failed.load(Ordering::Acquire));
                assert!(!self.shared.stopped.load(Ordering::Acquire));
                assert_eq!(self.backend.pressure_observation().voice_ceiling_drops, 0);
            }
        }

        fn validate_callback_chunk(
            a_output: &[f32],
            b_output: &[f32],
            chunk: usize,
            mut hash: Option<&mut u64>,
        ) -> f32 {
            assert_eq!(a_output.len(), b_output.len());
            let mut peak = 0.0f32;
            for (a, b) in a_output.iter().zip(b_output.iter()) {
                assert!(a.is_finite() && b.is_finite());
                assert_eq!(a.to_bits(), b.to_bits(), "PCM mismatch in chunk {chunk}");
                peak = peak.max(a.abs());
                if let Some(hash) = hash.as_deref_mut() {
                    for byte in a.to_bits().to_le_bytes() {
                        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            assert!(peak > 1e-6, "silent chunk {chunk}");
            peak
        }

        fn callback_gate_pair(
            frames: usize,
            gated_b: bool,
            reverse: bool,
            warmup_chunks: usize,
            measured_chunks: usize,
        ) -> ([u128; 2], u64, f32, u64) {
            callback_gate_receipt_pair(
                frames,
                [false, gated_b],
                [ReceiptBenchMode::Disabled; 2],
                reverse,
                warmup_chunks,
                measured_chunks,
            )
        }

        fn callback_gate_receipt_pair(
            frames: usize,
            gated: [bool; 2],
            receipts: [ReceiptBenchMode; 2],
            reverse: bool,
            warmup_chunks: usize,
            measured_chunks: usize,
        ) -> ([u128; 2], u64, f32, u64) {
            assert!(matches!(frames, 128 | 513));
            assert!(measured_chunks > 0);
            let chunks = warmup_chunks
                .checked_add(measured_chunks)
                .expect("chunk count");
            let callbacks = chunks
                .checked_mul(GATE_BENCH_CHUNK_CALLBACKS)
                .expect("callback count");
            let mut a = GateBenchState::new(frames, callbacks);
            let mut b = GateBenchState::new(frames, callbacks);
            a.arm_receipts(receipts[0]);
            b.arm_receipts(receipts[1]);
            // Only two prepared states and 65,664 bytes of PCM scratch coexist.
            // Validation is outside each eight-callback timer, not retained history.
            let mut buffers = [[0.0f32; GATE_BENCH_CHUNK_CALLBACKS * 513 * 2]; 2];
            let [a_output, b_output] = &mut buffers;
            let samples = GATE_BENCH_CHUNK_CALLBACKS * frames * 2;
            let (a_output, b_output) = (&mut a_output[..samples], &mut b_output[..samples]);
            let before = tripwire::Violations::capture();
            let scopes = tripwire::scope_entries();
            let mut elapsed = [0u128; 2];
            let mut hash = 0xcbf29ce484222325u64;
            let mut peak = 0.0f32;
            for chunk in 0..chunks {
                // Producer publication and collection never enter either timer.
                let a_offer = a.offer_receipt_chunk(receipts[0], chunk, frames);
                let b_offer = b.offer_receipt_chunk(receipts[1], chunk, frames);
                let (a_nanos, b_nanos) = if reverse {
                    let b_nanos = b.time_chunk(b_output, frames, gated[1]);
                    (a.time_chunk(a_output, frames, gated[0]), b_nanos)
                } else {
                    let a_nanos = a.time_chunk(a_output, frames, gated[0]);
                    (a_nanos, b.time_chunk(b_output, frames, gated[1]))
                };
                if receipts[0] != ReceiptBenchMode::Disabled {
                    a.collect_receipt_chunk(a_offer, frames);
                }
                if receipts[1] != ReceiptBenchMode::Disabled {
                    b.collect_receipt_chunk(b_offer, frames);
                }
                let chunk_peak = validate_callback_chunk(
                    a_output,
                    b_output,
                    chunk,
                    if chunk >= warmup_chunks {
                        Some(&mut hash)
                    } else {
                        None
                    },
                );
                if chunk >= warmup_chunks {
                    elapsed[0] = elapsed[0].checked_add(a_nanos).expect("elapsed time");
                    elapsed[1] = elapsed[1].checked_add(b_nanos).expect("elapsed time");
                    peak = peak.max(chunk_peak);
                }
            }
            assert!(tripwire::Violations::capture().since(before).clean());
            let observed_scopes = tripwire::scope_entries().wrapping_sub(scopes);
            // Other ordinary tests can enter this process-global scope counter.
            assert!(observed_scopes >= (callbacks * 2) as u64);
            assert!(elapsed.iter().all(|nanos| *nanos > 0));
            a.check(frames, callbacks);
            b.check(frames, callbacks);
            (elapsed, hash, peak, observed_scopes)
        }

        #[test]
        fn active_callback_gate_preserves_complete_pcm_and_allocation_contract() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            for frames in [128, 513] {
                callback_gate_pair(frames, true, false, 1, 2);
            }
        }

        #[test]
        fn callback_receipts_preserve_complete_pcm_and_allocation_contract() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            for frames in [128, 513] {
                for mode in [ReceiptBenchMode::Empty, ReceiptBenchMode::Window] {
                    callback_gate_receipt_pair(
                        frames,
                        [true; 2],
                        [ReceiptBenchMode::Disabled, mode],
                        false,
                        1,
                        2,
                    );
                }
            }
        }

        const DENSE_RECEIPT_WINDOWS: usize = 4;
        const DENSE_ONSETS_PER_CALLBACK: usize = 16;
        const DENSE_VOICE_CAPACITY: usize = 256;

        fn callback_dense_receipt_pair(
            frames: usize,
            converted_per_window: usize,
            tracked_b: bool,
            reverse: bool,
            warmup_chunks: usize,
        ) -> ([u128; 2], u64, f32, u64) {
            assert!(matches!(frames, 128 | 513));
            assert!(
                (1..=crate::confirmation::MAX_CONFIRMATION_ORDINALS)
                    .contains(&converted_per_window)
            );
            let onsets_per_chunk = DENSE_ONSETS_PER_CALLBACK * GATE_BENCH_CHUNK_CALLBACKS;
            let measured_onsets = DENSE_RECEIPT_WINDOWS * converted_per_window;
            assert!(measured_onsets.is_multiple_of(onsets_per_chunk));
            let measured_chunks = measured_onsets / onsets_per_chunk;
            let chunks = warmup_chunks
                .checked_add(measured_chunks)
                .expect("dense chunks");
            let callbacks = chunks * GATE_BENCH_CHUNK_CALLBACKS;
            let mut a = GateBenchState::with_capacity(frames, callbacks, DENSE_VOICE_CAPACITY);
            let mut b = GateBenchState::with_capacity(frames, callbacks, DENSE_VOICE_CAPACITY);
            if tracked_b {
                b.arm_receipts(ReceiptBenchMode::Empty);
            }
            let start = (warmup_chunks * GATE_BENCH_CHUNK_CALLBACKS * frames) as u64;
            let end = (callbacks * frames) as u64;
            let offers: [_; DENSE_RECEIPT_WINDOWS] = std::array::from_fn(|window| {
                let mut offer = confirmation_offer(
                    &b.shared.confirmations,
                    window as u64 + 1,
                    1,
                    start,
                    end,
                    converted_per_window as u32,
                );
                offer.takeover_frame = 0;
                offer
            });
            let mut buffers = [[0.0f32; GATE_BENCH_CHUNK_CALLBACKS * 513 * 2]; 2];
            let [a_output, b_output] = &mut buffers;
            let samples = GATE_BENCH_CHUNK_CALLBACKS * frames * 2;
            let (a_output, b_output) = (&mut a_output[..samples], &mut b_output[..samples]);
            let before = tripwire::Violations::capture();
            let scopes = tripwire::scope_entries();
            let mut elapsed = [0u128; 2];
            let mut hash = 0xcbf29ce484222325u64;
            let mut peak = 0.0f32;
            let mut confirmed = [false; DENSE_RECEIPT_WINDOWS];
            for chunk in 0..chunks {
                let measured = chunk >= warmup_chunks;
                if chunk == warmup_chunks && tracked_b {
                    for offer in offers {
                        assert!(b.shared.confirmations.publish(offer));
                    }
                }
                // One bounded producer refill, outside timing. Both arms receive
                // exactly the same onset order; only B's envelope can carry tags.
                // Short native voices keep active overlap below semantic polyphony,
                // and 128 queued onsets plus their tails fit the prepared 256 slots.
                for local in 0..onsets_per_chunk {
                    let global = chunk * onsets_per_chunk + local;
                    let within_window =
                        chunk.saturating_sub(warmup_chunks) * onsets_per_chunk + local;
                    let target = ((chunk * GATE_BENCH_CHUNK_CALLBACKS
                        + local / DENSE_ONSETS_PER_CALLBACK)
                        * frames
                        + (local % DENSE_ONSETS_PER_CALLBACK) * frames / DENSE_ONSETS_PER_CALLBACK)
                        as u64;
                    let mut queued = confirmation_event(
                        offers[within_window % DENSE_RECEIPT_WINDOWS],
                        (within_window / DENSE_RECEIPT_WINDOWS) as u32,
                        target,
                    );
                    queued.event.onset_id = global as u64 + 2;
                    queued.event.freq_hz = 220.0 + (global % 5) as f32 * 55.0;
                    queued.event.gain = 0.01;
                    queued.event.duration_secs = 0.0005;
                    queued.event.controls.envelope = crate::Envelope {
                        attack_secs: 0.0001,
                        decay_secs: 0.0,
                        sustain: 1.0,
                        release_secs: 0.0001,
                    };
                    assert!(a.shared.ring.push(queued.event));
                    if !tracked_b || !measured {
                        queued.confirmation = None;
                    }
                    assert!(b.shared.ring.push_queued(queued));
                }
                let (a_nanos, b_nanos) = if reverse {
                    let b_nanos = b.time_chunk(b_output, frames, true);
                    (a.time_chunk(a_output, frames, true), b_nanos)
                } else {
                    let a_nanos = a.time_chunk(a_output, frames, true);
                    (a_nanos, b.time_chunk(b_output, frames, true))
                };
                let chunk_peak = validate_callback_chunk(
                    a_output,
                    b_output,
                    chunk,
                    if measured { Some(&mut hash) } else { None },
                );
                if measured {
                    elapsed[0] = elapsed[0].checked_add(a_nanos).expect("dense elapsed");
                    elapsed[1] = elapsed[1].checked_add(b_nanos).expect("dense elapsed");
                    peak = peak.max(chunk_peak);
                }
                for _ in 0..DENSE_RECEIPT_WINDOWS {
                    let Some(terminal) = b.shared.confirmations.pop_terminal() else {
                        break;
                    };
                    assert!(tracked_b);
                    assert_eq!(
                        chunk + 1,
                        chunks,
                        "a complete dense window cannot finish early"
                    );
                    let slot = offers
                        .iter()
                        .position(|offer| offer.key == terminal.key)
                        .expect("offered key");
                    assert!(!confirmed[slot], "duplicate dense terminal");
                    confirmed[slot] = true;
                    assert_eq!(terminal.generation, 1);
                    assert_eq!(terminal.takeover_frame, 0);
                    assert_eq!(
                        terminal.outcome,
                        crate::confirmation::WindowOutcome::Confirmed
                    );
                    assert!(
                        terminal.copied_start_frame >= start && terminal.copied_end_frame <= end
                    );
                    assert!(terminal.copied_start_frame < terminal.copied_end_frame);
                }
                assert!(b.shared.confirmations.pop_terminal().is_none());
            }
            assert_eq!(confirmed, [tracked_b; DENSE_RECEIPT_WINDOWS]);
            assert!(tripwire::Violations::capture().since(before).clean());
            let observed_scopes = tripwire::scope_entries().wrapping_sub(scopes);
            assert!(observed_scopes >= (callbacks * 2) as u64);
            assert!(elapsed.iter().all(|nanos| *nanos > 0));
            for state in [&a, &b] {
                state.check_counts(frames, callbacks, callbacks * DENSE_ONSETS_PER_CALLBACK + 1);
                assert_eq!(
                    state
                        .backend
                        .pressure_observation()
                        .semantic_polyphony_fades,
                    0
                );
            }
            assert_eq!(
                a.backend.pressure_observation().active_voices,
                b.backend.pressure_observation().active_voices
            );
            (elapsed, hash, peak, observed_scopes)
        }

        #[test]
        fn callback_dense_receipts_preserve_pcm_and_confirm_every_onset() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            for frames in [128, 513] {
                callback_dense_receipt_pair(frames, 32, true, false, 1);
            }
        }

        /// Four full finite ledgers with actual interleaved onset admission,
        /// activation and host copy, compared with identical untracked audio.
        /// Run alone with `--release --exact --ignored --test-threads=1`.
        #[test]
        #[ignore = "manual release benchmark"]
        #[allow(
            clippy::assertions_on_constants,
            reason = "reject debug invocation at runtime, not compilation"
        )]
        fn callback_dense_consumer_confirmation_overhead_report() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(
                !cfg!(debug_assertions),
                "run this manual benchmark with --release"
            );
            assert!(tripwire::allocator_is_armed());
            const CONVERTED_PER_WINDOW: usize = 4096;
            const WARMUP_CHUNKS: usize = 4;
            const PAIRS: usize = 4;
            let callbacks =
                DENSE_RECEIPT_WINDOWS * CONVERTED_PER_WINDOW / DENSE_ONSETS_PER_CALLBACK;
            for frames in [128, 513] {
                let mut reference_hash = None;
                for tracked_b in [false, true] {
                    for repetition in 0..PAIRS {
                        let reverse = repetition % 2 == 1;
                        let (nanos, hash, peak, observed_scopes) = callback_dense_receipt_pair(
                            frames,
                            CONVERTED_PER_WINDOW,
                            tracked_b,
                            reverse,
                            WARMUP_CHUNKS,
                        );
                        assert_eq!(
                            observed_scopes,
                            ((callbacks + WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS) * 2) as u64
                        );
                        assert_eq!(*reference_hash.get_or_insert(hash), hash);
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1, "benchmark": "callback-dense-consumer-confirmation",
                                "measurement": "correctness-metadata-overhead",
                                "timing_scope": "eight-callback-chunks", "callback_path": "active-gate-full-callback",
                                "dispatch": "portable-engine-kernels", "sample_rate": 48_000,
                                "frames_per_callback": frames, "callbacks_per_arm": callbacks,
                                "warmup_callbacks": WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS,
                                "voice_capacity": DENSE_VOICE_CAPACITY, "sustained_bed_voices": 1,
                                "dense_onsets_per_callback": DENSE_ONSETS_PER_CALLBACK,
                                "measured_onsets_per_arm": callbacks * DENSE_ONSETS_PER_CALLBACK,
                                "validated_samples_per_arm": callbacks * frames * 2,
                                "window_count": if tracked_b { DENSE_RECEIPT_WINDOWS } else { 0 },
                                "converted_per_window": CONVERTED_PER_WINDOW,
                                "confirmed_receipts_per_b": if tracked_b { DENSE_RECEIPT_WINDOWS } else { 0 },
                                "comparison": if tracked_b { "untracked-tagged" } else { "untracked-untracked" },
                                "producer_publication_and_collection_timed": false,
                                "repetition": repetition, "block": repetition / 2, "order": if reverse { "ba" } else { "ab" },
                                "a_nanos": nanos[0], "b_nanos": nanos[1],
                                "a_nanos_per_callback": nanos[0] as f64 / callbacks as f64,
                                "b_nanos_per_callback": nanos[1] as f64 / callbacks as f64,
                                "validation_hash": format!("{hash:016x}"), "validation_peak": peak,
                            })
                        );
                    }
                }
            }
        }

        /// Correctness-metadata overhead for the complete gated callback, not a
        /// throughput improvement or device-pacing measurement. Publication,
        /// terminal collection, validation, construction and teardown are untimed.
        /// Run alone with `--release --exact --ignored --test-threads=1`.
        #[test]
        #[ignore = "manual release benchmark"]
        #[allow(
            clippy::assertions_on_constants,
            reason = "reject debug invocation at runtime, not compilation"
        )]
        fn callback_consumer_confirmation_overhead_report() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(
                !cfg!(debug_assertions),
                "run this manual benchmark with --release"
            );
            assert!(tripwire::allocator_is_armed());
            // Match the existing whole-callback gate baseline's musical interval
            // and repetition schedule, so pre-receipt builds remain comparable.
            const WARMUP_CHUNKS: usize = 64;
            const MEASURED_CHUNKS: usize = 512;
            const PAIRS: usize = 12;
            let callbacks = MEASURED_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS;
            for frames in [128, 513] {
                let mut reference_hash = None;
                for mode in [
                    ReceiptBenchMode::Disabled,
                    ReceiptBenchMode::Empty,
                    ReceiptBenchMode::Window,
                ] {
                    for repetition in 0..PAIRS {
                        // Consecutive fresh AB and BA pairs form balanced ABBA;
                        // Disabled/Disabled supplies the same-callback A/A control.
                        let reverse = repetition % 2 == 1;
                        let (nanos, hash, peak, observed_scopes) = callback_gate_receipt_pair(
                            frames,
                            [true; 2],
                            [ReceiptBenchMode::Disabled, mode],
                            reverse,
                            WARMUP_CHUNKS,
                            MEASURED_CHUNKS,
                        );
                        assert_eq!(
                            observed_scopes,
                            ((WARMUP_CHUNKS + MEASURED_CHUNKS) * GATE_BENCH_CHUNK_CALLBACKS * 2)
                                as u64
                        );
                        assert_eq!(
                            *reference_hash.get_or_insert(hash),
                            hash,
                            "receipt modes must render the same measured window"
                        );
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1,
                                "benchmark": "callback-consumer-confirmation",
                                "measurement": "correctness-metadata-overhead",
                                "timing_scope": "eight-callback-chunks",
                                "callback_path": "active-gate-full-callback",
                                "dispatch": "portable-engine-kernels",
                                "voices": 1, "voice_capacity": 4, "sample_rate": 48_000,
                                "frames_per_callback": frames,
                                "callbacks_per_arm": callbacks,
                                "warmup_callbacks": WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS,
                                "validated_samples_per_arm": callbacks * frames * 2,
                                "a_mode": ReceiptBenchMode::Disabled.label(), "b_mode": mode.label(),
                                "measured_offers_per_b": if mode == ReceiptBenchMode::Window { MEASURED_CHUNKS } else { 0 },
                                "total_confirmed_receipts_per_b": if mode == ReceiptBenchMode::Window { WARMUP_CHUNKS + MEASURED_CHUNKS } else { 0 },
                                "receipt_publication_and_collection_timed": false,
                                "repetition": repetition, "block": repetition / 2,
                                "order": if reverse { "ba" } else { "ab" },
                                "a_nanos": nanos[0], "b_nanos": nanos[1],
                                "a_nanos_per_callback": nanos[0] as f64 / callbacks as f64,
                                "b_nanos_per_callback": nanos[1] as f64 / callbacks as f64,
                                "validation_hash": format!("{hash:016x}"), "validation_peak": peak,
                            })
                        );
                    }
                }
            }
        }

        /// Active gate plus callback-body service time, not device pacing or
        /// per-callback percentiles. Both arms share the eight-callback validation
        /// cadence. Construction, activation and teardown are untimed.
        /// Run this test alone with `--exact --ignored --test-threads=1`.
        #[test]
        #[ignore = "manual release benchmark"]
        fn callback_active_gate_overhead_report() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            const WARMUP_CHUNKS: usize = 64;
            const MEASURED_CHUNKS: usize = 512;
            const PAIRS: usize = 12;
            let callbacks = MEASURED_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS;
            for frames in [128, 513] {
                let mut reference_hash = None;
                for gated_b in [false, true] {
                    for repetition in 0..PAIRS {
                        // Adjacent A/B and B/A pairs form a balanced ABBA block.
                        let reverse = repetition % 2 == 1;
                        let (nanos, hash, peak, observed_scopes) = callback_gate_pair(
                            frames,
                            gated_b,
                            reverse,
                            WARMUP_CHUNKS,
                            MEASURED_CHUNKS,
                        );
                        assert_eq!(
                            observed_scopes,
                            ((WARMUP_CHUNKS + MEASURED_CHUNKS) * GATE_BENCH_CHUNK_CALLBACKS * 2)
                                as u64
                        );
                        assert_eq!(
                            *reference_hash.get_or_insert(hash),
                            hash,
                            "fresh pairs must render the same measured window"
                        );
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "schema_version": 1, "benchmark": "callback-active-gate",
                                "timing_scope": "eight-callback-chunks", "dispatch": "portable-engine-kernels",
                                "voices": 1, "voice_capacity": 4, "sample_rate": 48_000,
                                "frames_per_callback": frames, "callbacks_per_arm": callbacks,
                                "warmup_callbacks": WARMUP_CHUNKS * GATE_BENCH_CHUNK_CALLBACKS,
                                "validated_samples_per_arm": callbacks * frames * 2,
                                "comparison": if gated_b { "direct-gated" } else { "direct-direct" },
                                "repetition": repetition, "block": repetition / 2,
                                "order": if reverse { "ba" } else { "ab" },
                                "a_nanos": nanos[0], "b_nanos": nanos[1],
                                "a_nanos_per_callback": nanos[0] as f64 / callbacks as f64,
                                "b_nanos_per_callback": nanos[1] as f64 / callbacks as f64,
                                "validation_hash": format!("{hash:016x}"), "validation_peak": peak,
                            })
                        );
                    }
                }
            }
        }

        fn time_callback_telemetry_arm(
            measure_load: bool,
            measure_pressure: bool,
            callbacks: usize,
        ) -> u128 {
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            for _ in 0..512 {
                tripwire::audio_scope(|| {
                    write_live_output_inner(
                        &mut backend,
                        &mut meters,
                        &mut output,
                        2,
                        &shared,
                        std::hint::black_box(measure_load),
                        std::hint::black_box(measure_pressure),
                    )
                });
            }
            let started = Instant::now();
            for _ in 0..callbacks {
                tripwire::audio_scope(|| {
                    write_live_output_inner(
                        &mut backend,
                        &mut meters,
                        &mut output,
                        2,
                        &shared,
                        std::hint::black_box(measure_load),
                        std::hint::black_box(measure_pressure),
                    )
                });
            }
            std::hint::black_box(output);
            started.elapsed().as_nanos()
        }

        #[test]
        #[ignore = "manual release benchmark"]
        fn callback_load_meter_overhead_report() {
            const CALLBACKS: usize = 20_000;
            for repetition in 0..15 {
                let unmetered_a = time_callback_telemetry_arm(false, false, CALLBACKS);
                let metered = time_callback_telemetry_arm(true, false, CALLBACKS);
                let unmetered_b = time_callback_telemetry_arm(false, false, CALLBACKS);
                eprintln!(
                    "{{\"benchmark\":\"callback-load-meter\",\"repetition\":{repetition},\"callbacks\":{CALLBACKS},\"unmetered_a_nanos\":{unmetered_a},\"metered_nanos\":{metered},\"unmetered_b_nanos\":{unmetered_b}}}"
                );
            }
        }

        #[test]
        #[ignore = "manual release benchmark"]
        fn callback_pressure_meter_overhead_report() {
            const CALLBACKS: usize = 20_000;
            for repetition in 0..15 {
                let load_only_a = time_callback_telemetry_arm(true, false, CALLBACKS);
                let with_pressure = time_callback_telemetry_arm(true, true, CALLBACKS);
                let load_only_b = time_callback_telemetry_arm(true, false, CALLBACKS);
                eprintln!(
                    "{{\"benchmark\":\"callback-pressure-meter\",\"repetition\":{repetition},\"callbacks\":{CALLBACKS},\"load_only_a_nanos\":{load_only_a},\"with_pressure_nanos\":{with_pressure},\"load_only_b_nanos\":{load_only_b}}}"
                );
            }
        }
    }
    mod callback_output {
        use super::*;

        #[test]
        fn callback_load_publication_is_calibrated_and_allocation_free() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            assert!(tripwire::allocator_is_armed());
            let before = tripwire::Violations::capture();
            for _ in 0..REALTIME_LOAD_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let violations = tripwire::Violations::capture().since(before);
            assert!(
                violations.clean(),
                "load telemetry violated callback allocation contract: {violations:?}"
            );

            let load = shared.report(1).realtime_load;
            assert_eq!(load.sample_rate_hz, 48_000);
            assert_eq!(load.last_callback_frames, 128);
            assert_eq!(load.last_callback_period_nanos, 2_666_666);
            assert_eq!(load.total_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
            assert_eq!(load.total_period_nanos, 42_666_656);
            assert!(load.max_callback_busy_nanos >= load.last_callback_busy_nanos);
            let pressure = shared.report(1).realtime_pressure;
            assert_eq!(pressure.total_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
            assert_eq!(pressure.window_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
        }

        #[test]
        fn stopping_an_active_pooled_voice_does_not_free_in_the_callback() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let controls = crate::OscillatorControls {
                stretch: Some(2.0),
                ..Default::default()
            };
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls,
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = test_meter();
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            let pressure = backend.pressure_observation();
            assert_eq!(pressure.active_voices, 1);
            assert_eq!(
                pressure.active_pool_leases[crate::pressure::RealtimePool::Stretch.index()],
                1
            );

            assert!(tripwire::allocator_is_armed());
            let before = tripwire::Violations::capture();
            shared.stopped.store(true, Ordering::Release);
            // The 10 ms ramp spans four 128-frame callbacks. The last one
            // resets the voices and returns the lease.
            for _ in 0..4 {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let violations = tripwire::Violations::capture().since(before);
            assert!(
                violations.clean(),
                "stop freed a pooled voice inside the callback: {violations:?}"
            );
            assert!(backend.stop_ramp_complete());
            let pressure = backend.pressure_observation();
            assert_eq!(pressure.active_voices, 0);
            assert_eq!(
                pressure.active_pool_leases[crate::pressure::RealtimePool::Stretch.index()],
                0
            );
        }

        #[test]
        fn callback_pressure_reports_semantic_fades_without_allocating() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            for onset_id in 0..130 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.1,
                    duration_secs: 1.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 256).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            let before = tripwire::Violations::capture();
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            assert!(
                tripwire::Violations::capture().since(before).clean(),
                "pressure telemetry allocated in the callback"
            );
            let pressure = shared.report(1).realtime_pressure;
            assert_eq!(pressure.active_voices, 130);
            assert_eq!(pressure.peak_active_voices, 130);
            assert_eq!(pressure.active_orbits, 1);
            assert_eq!(pressure.semantic_polyphony_fades, 2);
            assert_eq!(pressure.window_semantic_polyphony_fades, 2);
            assert_eq!(pressure.voice_ceiling_drops, 0);
        }

        #[test]
        fn callback_applies_polyphony_changes_without_allocating_or_cutting_voices() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            shared.max_polyphony.store(256, Ordering::Relaxed);
            for onset_id in 0..160 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.001,
                    duration_secs: 2.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 256).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 256];
            let before = tripwire::Violations::capture();
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let raised = shared.report(1).realtime_pressure;
            assert_eq!(raised.max_polyphony, 256);
            assert_eq!(raised.active_voices, 160);
            assert_eq!(raised.semantic_polyphony_fades, 0);
            shared.max_polyphony.store(64, Ordering::Relaxed);
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            assert!(
                tripwire::Violations::capture().since(before).clean(),
                "changing polyphony must not allocate in the callback"
            );
            let lowered = shared.report(1).realtime_pressure;
            assert_eq!(lowered.max_polyphony, 64);
            assert_eq!(
                lowered.active_voices, 160,
                "retiring voices must keep their fade"
            );
            assert_eq!(lowered.semantic_polyphony_fades, 96);
            assert_eq!(lowered.voice_ceiling_drops, 0);
            assert_eq!(
                LiveShared::new(1).max_polyphony.load(Ordering::Relaxed),
                crate::MAX_POLYPHONY
            );
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn device_polyphony_is_bounded_and_prepared_outputs_keep_it() {
            let mut output = ManualLiveOutput::new(48_000, 1).expect("manual output");
            assert_eq!(output.device().max_polyphony(), crate::MAX_POLYPHONY);
            output.device().set_max_polyphony(0);
            assert_eq!(output.device().max_polyphony(), 1);
            output.device().set_max_polyphony(usize::MAX);
            assert_eq!(
                output.device().max_polyphony(),
                crate::MAX_CONFIGURABLE_POLYPHONY
            );
            output.render(&mut [0.0; 256]);
            assert_eq!(
                output.backend.max_polyphony(),
                crate::MAX_CONFIGURABLE_POLYPHONY
            );
            let replacement = PreparedLiveOutput::backend(
                &output.device.shared,
                48_000,
                LiveOutputOptions::default().with_dispatch(DspDispatch::portable()),
                &[],
            )
            .expect("replacement backend");
            assert_eq!(
                replacement.max_polyphony(),
                crate::MAX_CONFIGURABLE_POLYPHONY
            );
        }

        #[test]
        fn callback_pressure_reports_exact_degradation_and_pool_misses() {
            let shared = LiveShared::new(1);
            let controls = crate::OscillatorControls {
                reverb: Some(crate::ReverbControls {
                    wet: 0.5,
                    size_secs: 0.1,
                    fade_secs: 0.1,
                    lp_start_hz: 15_000.0,
                    lp_end_hz: 1_000.0,
                    ir: None,
                }),
                ..crate::OscillatorControls::default()
            };
            for onset_id in 0..9 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.1,
                    duration_secs: 1.0,
                    ui_visuals: 0,
                    controls,
                    sample: None,
                    synth: Some(crate::SynthSource::ZzFx {
                        params: crate::zzfx::ZzfxParams {
                            delay: 0.1,
                            sustain: 0.1,
                            release: 0.2,
                            ..crate::zzfx::ZzfxParams::default()
                        },
                    }),
                    wavetable: None,
                    cut: None,
                }));
            }
            let mut backend = LiveScalarBackend::new(48_000, 16).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let mut output = [0.0f32; 128 * 2];
            for _ in 0..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
            }
            let pressure = shared.report(1).realtime_pressure;
            assert_eq!(
                pressure.active_pool_leases(crate::RealtimePool::ZzfxDelay),
                8
            );
            assert_eq!(pressure.pool_misses(crate::RealtimePool::ZzfxDelay), 1);
            assert_eq!(
                pressure.window_pool_misses(crate::RealtimePool::ZzfxDelay),
                1
            );
            assert_eq!(pressure.orbit_reverb_misses, 9);
            assert_eq!(pressure.window_orbit_reverb_misses, 9);

            let refused_shared = LiveShared::new(1);
            let event = |onset_id, target_frame| AudioEvent {
                onset_id,
                generation: 1,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.1,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            assert!(refused_shared.ring.push(event(0, 0)));
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut meters = LiveCallbackMeters::new(48_000, &refused_shared.realtime_load);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &refused_shared)
            });
            assert!(refused_shared.ring.push(event(1, 128)));
            for _ in 1..crate::pressure::PRESSURE_PUBLISH_INTERVAL {
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &refused_shared)
                });
            }
            let pressure = refused_shared.report(1).realtime_pressure;
            assert_eq!(pressure.voice_ceiling_drops, 1);
            assert_eq!(pressure.window_voice_ceiling_drops, 1);
            assert_eq!(pressure.refused_voices, 1);
            assert_eq!(pressure.window_refused_voices, 1);
        }

        #[test]
        fn live_voice_uses_the_installed_shared_orbit_reverb() {
            let shared = LiveShared::new(1);
            let installed = crate::reverb::ReverbParams {
                ir: None,
                size_secs: 0.1,
                fade_secs: 0.1,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
            };
            let channel = crate::assets::SampleChannel::new();
            let reverb = Box::into_raw(Box::new(crate::reverb::OrbitReverb::generate(
                48_000, installed,
            )));
            if let Err(rejected) = channel
                .reverb_installs
                .push(crate::assets::ReverbInstall { orbit: 0, reverb })
            {
                // SAFETY: the empty test ring refused before ownership moved.
                drop(unsafe { Box::from_raw(rejected.reverb) });
                panic!("reverb install was refused");
            }

            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            backend.drain_reverb_installs(&channel);
            let controls = crate::OscillatorControls {
                orbit: 0,
                reverb: Some(crate::ReverbControls {
                    wet: 1.0,
                    size_secs: 0.2,
                    fade_secs: 0.1,
                    lp_start_hz: 15_000.0,
                    lp_end_hz: 1_000.0,
                    ir: None,
                }),
                dry: Some(0.0),
                ..Default::default()
            };
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls,
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));

            let mut meters = test_meter();
            let mut output = [0.0f32; 128 * 2];
            let mut peak = 0.0f32;
            for _ in 0..8 {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared);
                peak = output
                    .iter()
                    .fold(peak, |current, sample| current.max(sample.abs()));
            }
            let pressure = backend.pressure_observation();
            assert_eq!(pressure.active_orbit_reverbs, 1);
            assert_eq!(pressure.orbit_reverb_misses, 0);
            assert!(peak > 0.0, "the shared reverb bus stayed silent");
        }

        #[test]
        fn live_reverb_batch_keeps_only_the_final_request_per_orbit() {
            let first = crate::reverb::ReverbParams {
                ir: None,
                size_secs: 0.1,
                fade_secs: 0.1,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
            };
            let final_request = crate::reverb::ReverbParams {
                size_secs: 0.2,
                ..first
            };
            let mut batch = LiveReverbBatch::default();

            batch.request_orbit(0, first);
            batch.request_orbit(0, final_request);
            batch.request_orbit(1, first);

            assert_eq!(batch.requested[0], Some(final_request));
            assert_eq!(batch.requested[1], Some(first));
            assert_eq!(batch.requested.iter().flatten().count(), 2);
        }

        #[test]
        fn callback_writer_is_non_silent_and_stops_at_the_exact_frame() {
            let mut backend = ScalarBackend::new();
            backend.init(48_000).unwrap();
            backend.note(OnsetEvent::new(0, 440.0, 0.8, 0.1));
            let written = AtomicU64::new(0);
            let mut first = [0.0f32; 128 * 2];
            write_output(&mut backend, &mut first, 2, 130, &written);
            assert_eq!(written.load(Ordering::Acquire), 128);
            assert!(first.iter().any(|sample| sample.abs() > 1e-6));

            let mut last = [1.0f32; 128 * 2];
            write_output(&mut backend, &mut last, 2, 130, &written);
            assert_eq!(written.load(Ordering::Acquire), 130);
            assert!(last[4..].iter().all(|sample| *sample == 0.0));
        }

        #[test]
        fn live_callback_report_counts_delivery_filtering_and_ring_pressure() {
            let shared = LiveShared::new(2);
            for event in [
                AudioEvent {
                    onset_id: 1,
                    generation: 1,
                    target_frame: 0,
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
                AudioEvent {
                    onset_id: 2,
                    generation: 2,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 440.0,
                    gain: 0.5,
                    duration_secs: 0.1,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                },
            ] {
                assert!(shared.ring.push(event));
            }
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            shared
                .buffer_playback_nanos
                .store(2_000_000, Ordering::Release);
            shared
                .playback_latency_nanos
                .store(3_000_000, Ordering::Release);
            shared
                .max_playback_latency_nanos
                .store(4_000_000, Ordering::Release);
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });

            let report = shared.report(77);
            assert_eq!(report.stream_id, 77);
            assert_eq!(report.submitted_frames, 128);
            assert_eq!(report.playhead_nanos, 0);
            assert_eq!(report.buffer_playback_nanos, 2_000_000);
            assert_eq!(report.playback_latency_nanos, 3_000_000);
            assert_eq!(report.max_playback_latency_nanos, 4_000_000);
            assert_eq!(report.generation, 2);
            assert_eq!(report.callbacks, 1);
            assert_eq!(report.accepted_events, 1);
            assert_eq!(report.stale_events_filtered, 1);
            assert_eq!(report.refused_voices, 0);
            assert_eq!(report.callback_errors, 0);
            assert_eq!(report.callback_scope_misses, 0);
            assert_eq!(report.callback_allocations, 0);
            assert_eq!(report.callback_frees, 0);
            assert!(!report.stop_acknowledged);
            assert_eq!(report.ring_refusals, 0);
            assert_eq!(report.ring_role_conflicts, 0);
            assert_eq!(report.ring_depth, 0);
            assert_eq!(report.ring_capacity, LIVE_RING_CAPACITY as u64);
            assert_eq!(report.ring_peak_depth, 2);
            assert_eq!(report.asset_queues.capacity, 64);
            assert_eq!(
                report.asset_queues,
                AssetQueuePressureSnapshot {
                    capacity: 64,
                    ..AssetQueuePressureSnapshot::default()
                }
            );
            assert_eq!(report.cutover_race_blocks, 0);
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
        }

        #[test]
        fn live_dsp_writer_records_missing_callback_scope() {
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut output = [0.0f32; 2];

            write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared);
            assert_eq!(
                shared.report(1).callback_scope_misses,
                1,
                "DSP work outside audio_scope was not exposed"
            );

            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 2, &shared)
            });
            assert_eq!(
                shared.report(1).callback_scope_misses,
                1,
                "a correctly scoped DSP call was misclassified"
            );
        }

        #[test]
        fn live_report_does_not_erase_tripwire_violations() {
            let report = apply_tripwire_report(
                LiveShared::new(1).report(1),
                tripwire::Violations {
                    allocs: 3,
                    frees: 2,
                },
            );
            assert_eq!(report.callback_allocations, 3);
            assert_eq!(report.callback_frees, 2);
        }

        /// A full voice pool refuses excess onsets and reports the refusal count.
        /// Exactly one event fits in this one-slot pool.
        #[test]
        fn refused_voices_are_counted_and_are_not_fatal() {
            // Generation must match the events, or they are filtered as stale
            // and nothing ever reaches the voice pool.
            let shared = LiveShared::new(1);
            for onset_id in 0..8u64 {
                assert!(shared.ring.push(AudioEvent {
                    onset_id,
                    generation: 1,
                    target_frame: 0,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.1,
                    duration_secs: 4.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: None,
                    wavetable: None,
                    cut: None,
                }));
            }
            // Capacity 1: the first onset takes the slot, the rest are refused.
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut output, 1, &shared)
            });

            let report = shared.report(1);
            assert!(
                report.refused_voices > 0,
                "a pool of 1 took all 8 simultaneous onsets: {report:?}"
            );
            assert_eq!(
                report.accepted_events, 1,
                "exactly one onset should have fit: {report:?}"
            );
        }
    }
    mod confirmation {
        use super::*;

        #[test]
        fn confirmation_mixed_conversion_waits_for_every_due_host_copy() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let mut offer = confirmation_offer(channel, 1, 7, 0, 256, 2);
            offer.intended = 5;
            offer.refused = 1;
            offer.skipped_loading = 1;
            offer.external = 1;
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(confirmation_event(offer, 0, 0)));
            assert!(shared.ring.push_queued(confirmation_event(offer, 1, 128)));
            let mut backend = confirmation_backend(&shared, 4);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert!(
                channel.pop_terminal().is_none(),
                "pending intake is not host consumption"
            );
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            let terminal = channel.pop_terminal().expect("both due blocks copied");
            assert_eq!(terminal.key, offer.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Confirmed
            );
            assert!(channel.pop_terminal().is_none());
        }

        /// A rewind's drop horizons refuse tracked onsets through the real
        /// callback, and the confirmation ledger hears it, all inside the
        /// callback scope with nothing allocated: an outgoing onset refused by a
        /// fired line arm, and one refused by an immediate rewind's own cut
        /// horizon (its takeover set past the onset, so only the cut can refuse
        /// it), each end their window Superseded.
        #[test]
        fn rewind_drop_horizons_supersede_tracked_onsets_without_allocating() {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let mut backend = confirmation_backend(&shared, 4);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            let mut callback = |backend: &mut LiveScalarBackend| {
                let before = tripwire::Violations::capture();
                tripwire::audio_scope(|| {
                    write_live_output(backend, &mut meters, &mut output, 2, &shared)
                });
                let delta = tripwire::Violations::capture().since(before);
                assert!(delta.clean(), "a rewind callback allocated: {delta:?}");
            };

            // The line arm at 256 with its drop bit, fired by the third callback.
            shared.line_arm.store((256 << 2) | 0b11, Ordering::Release);
            callback(&mut backend); // [0, 128)
            callback(&mut backend); // [128, 256)
            let ghost = confirmation_offer(channel, 1, 7, 512, 1_024, 1);
            assert!(channel.publish(ghost));
            assert!(shared.ring.push_queued(confirmation_event(ghost, 0, 512)));
            callback(&mut backend); // [256, 384): fires, refuses the ghost
            let terminal = channel.pop_terminal().expect("the refused ghost's window");
            assert_eq!(terminal.key, ghost.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Superseded
            );

            // An immediate rewind flips 7 -> 8 at 384 with its takeover far
            // ahead: the old onset at 1_024 is behind the takeover, ahead of the
            // cut.
            let old = confirmation_offer(channel, 2, 7, 1_024, 1_536, 1);
            assert!(channel.publish(old));
            shared.takeover_frame.store(8_192, Ordering::Release);
            shared
                .takeover_cut
                .store(TakeoverCut::AtFlip as u64, Ordering::Release);
            shared.line_arm.store(0, Ordering::Release);
            shared.generation.store(8, Ordering::Release);
            assert!(shared.ring.push_queued(confirmation_event(old, 0, 1_024)));
            callback(&mut backend); // [384, 512)
            let terminal = channel.pop_terminal().expect("the cut's refused onset");
            assert_eq!(terminal.key, old.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Superseded
            );
            assert!(channel.pop_terminal().is_none());
        }

        #[test]
        fn confirmation_pending_capacity_refusal_cannot_certify_partial_audio() {
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let offer = confirmation_offer(channel, 1, 7, 0, 128, 2);
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(confirmation_event(offer, 0, 0)));
            assert!(shared.ring.push_queued(confirmation_event(offer, 1, 0)));
            let mut backend = confirmation_backend(&shared, 1);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            assert_eq!(
                channel.pop_terminal().expect("refused window").outcome,
                crate::confirmation::WindowOutcome::AdmissionRefused
            );
        }

        #[test]
        fn confirmation_expired_onset_cannot_certify_a_late_silent_copy() {
            let shared = LiveShared::new(7);
            shared.frames.store(48_000, Ordering::Release);
            let channel = &shared.confirmations;
            let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
            let mut queued = confirmation_event(offer, 0, 0);
            queued.event.duration_secs = 0.001;
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(queued));
            let mut backend = confirmation_backend(&shared, 2);
            let mut meters = test_meter();
            let mut output = [0.5f32; 256];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert_eq!(shared.accepted_events.load(Ordering::Relaxed), 1);
            assert!(output.iter().all(|sample| *sample == 0.0));
            let terminal = channel.pop_terminal().expect("expired finite window");
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::CopyMissed
            );
            assert_eq!(terminal.copied_start_frame, 48_000);
        }

        #[test]
        fn confirmation_missing_sample_is_not_repaired_by_a_later_install() {
            let shared = LiveShared::new(7);
            let channel = &shared.confirmations;
            let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
            let mut queued = confirmation_event(offer, 0, 0);
            queued.event.sample = Some(crate::SampleControls {
                sample: crate::SampleId(1),
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: crate::SampleHold::Hap,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            assert!(channel.publish(offer));
            assert!(shared.ring.push_queued(queued));
            let mut backend = confirmation_backend(&shared, 2);
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            // Leave the terminal undrained while the asset becomes ready: later
            // installation must not revise the actual missing-at-activation result.
            let sample =
                crate::DecodedSample::from_parts(48_000, 1, vec![0.5; 256]).expect("sample");
            assert!(
                shared
                    .samples
                    .installs
                    .push(crate::assets::SampleInstall {
                        id: crate::SampleId(1),
                        sample: Box::into_raw(Box::new(sample)),
                    })
                    .is_ok()
            );
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert_eq!(
                channel
                    .pop_terminal()
                    .expect("missing asset receipt")
                    .outcome,
                crate::confirmation::WindowOutcome::MissingSample
            );
            assert!(channel.pop_terminal().is_none());
        }

        #[test]
        fn confirmation_requires_the_expected_body_when_a_sample_id_is_replaced() {
            for expected_known in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let old = crate::DecodedSample::from_parts(48_000, 1, vec![0.25; 1024])
                    .expect("old body");
                let replacement =
                    crate::DecodedSample::from_parts(48_000, 1, vec![0.5; 1024]).expect("new body");
                let replacement_identity = replacement.identity();
                assert!(
                    shared
                        .samples
                        .installs
                        .push(crate::assets::SampleInstall {
                            id: crate::SampleId(1),
                            sample: Box::into_raw(Box::new(old)),
                        })
                        .is_ok()
                );
                let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
                let mut queued = confirmation_event(offer, 0, 0);
                queued.event.sample = Some(crate::SampleControls {
                    sample: crate::SampleId(1),
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: crate::SampleHold::Hap,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                });
                queued.expected_sample_identity = expected_known.then_some(replacement_identity);
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(queued));
                let mut backend = confirmation_backend(&shared, 4);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(
                    output.iter().any(|sample| sample.abs() > 1e-6),
                    "identity refusal must not change playback policy"
                );
                // The newer publication/install can arrive after this callback's
                // entry drain. Its readiness cannot revise the old activation.
                assert!(
                    shared
                        .samples
                        .installs
                        .push(crate::assets::SampleInstall {
                            id: crate::SampleId(1),
                            sample: Box::into_raw(Box::new(replacement)),
                        })
                        .is_ok()
                );
                let next = confirmation_offer(channel, 2, 7, 128, 256, 1);
                let mut next_event = confirmation_event(next, 0, 128);
                next_event.event.sample = queued.event.sample;
                next_event.expected_sample_identity = Some(replacement_identity);
                assert!(channel.publish(next));
                assert!(shared.ring.push_queued(next_event));
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("old-body failure retained")
                        .outcome,
                    crate::confirmation::WindowOutcome::MissingSample
                );
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("required body installed at activation")
                        .outcome,
                    crate::confirmation::WindowOutcome::Confirmed
                );
                assert!(channel.pop_terminal().is_none());
                shared.samples.reclaim();
            }
        }

        #[test]
        fn confirmation_wavetable_requires_its_expected_installed_body() {
            for installed in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let table =
                    crate::DecodedSample::from_parts(48_000, 1, vec![0.5; 256]).expect("table");
                let expected_identity = table.identity();
                if installed {
                    assert!(
                        shared
                            .samples
                            .installs
                            .push(crate::assets::SampleInstall {
                                id: crate::SampleId(1),
                                sample: Box::into_raw(Box::new(table)),
                            })
                            .is_ok()
                    );
                }
                let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
                let mut queued = confirmation_event(offer, 0, 0);
                queued.expected_sample_identity = Some(expected_identity);
                queued.event.wavetable = Some(crate::WavetableControls {
                    table: crate::SampleId(1),
                    frame_len: 256,
                    voices: 1.0,
                    lfo_shape: 0,
                    phaserand: 0.0,
                    freqspread: 0.0,
                    panspread: 0.0,
                    position: 0.0,
                    pos_env_amount: 0.0,
                    pos_attack: 0.0,
                    pos_decay: 0.5,
                    pos_sustain: 0.0,
                    pos_release: 0.1,
                    lfo_depth: 0.0,
                    lfo_rate: 1.0,
                    lfo_skew: 0.5,
                    lfo_dc: 0.0,
                    warp: 0.0,
                    warp_mode: 0,
                    warp_env_amount: 0.0,
                    warp_attack: 0.0,
                    warp_decay: 0.5,
                    warp_sustain: 0.0,
                    warp_release: 0.1,
                    warp_lfo_depth: 0.0,
                    warp_lfo_rate: 1.0,
                    warp_lfo_skew: 0.5,
                    warp_lfo_dc: 0.0,
                    warp_lfo_shape: 0,
                });
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(queued));
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("wavetable activation evidence")
                        .outcome,
                    if installed {
                        crate::confirmation::WindowOutcome::Confirmed
                    } else {
                        crate::confirmation::WindowOutcome::MissingSample
                    }
                );
            }
        }

        #[test]
        fn confirmation_superseded_pending_and_stop_do_not_certify_silence() {
            for stop in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let offer = confirmation_offer(channel, 1, 7, 0, 256, 1);
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(confirmation_event(offer, 0, 128)));
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(channel.pop_terminal().is_none());
                if stop {
                    shared.stopped.store(true, Ordering::Release);
                } else {
                    shared.takeover_frame.store(128, Ordering::Release);
                    shared.generation.store(8, Ordering::Release);
                }
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                let terminal = channel
                    .pop_terminal()
                    .expect("uncopied pending onset ended");
                assert_eq!(
                    terminal.outcome,
                    if stop {
                        crate::confirmation::WindowOutcome::Cancelled
                    } else {
                        crate::confirmation::WindowOutcome::Superseded
                    }
                );
            }
        }

        #[test]
        fn confirmation_copied_audio_and_valid_silence_survive_a_later_generation() {
            for converted in [0, 1] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let offer = confirmation_offer(channel, 1, 7, 0, 128, converted);
                assert!(channel.publish(offer));
                if converted != 0 {
                    assert!(shared.ring.push_queued(confirmation_event(offer, 0, 0)));
                }
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(
                    output.iter().any(|sample| sample.abs() > 1e-6),
                    converted != 0
                );
                // The producer handles a later request before it drains B's
                // terminal. The already copied B remains valid rollback evidence.
                shared.takeover_frame.store(128, Ordering::Release);
                shared.generation.store(8, Ordering::Release);
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                let terminal = channel.pop_terminal().expect("delayed B completion");
                assert_eq!(terminal.key, offer.key);
                assert_eq!(terminal.generation, 7);
                assert_eq!(
                    terminal.outcome,
                    crate::confirmation::WindowOutcome::Confirmed
                );
                assert!(channel.pop_terminal().is_none());
            }
        }

        #[test]
        fn confirmation_intentional_mute_and_permitted_dry_fallback_qualify() {
            for muted in [false, true] {
                let shared = LiveShared::new(7);
                let channel = &shared.confirmations;
                let offer = confirmation_offer(channel, 1, 7, 0, 128, 1);
                let mut queued = confirmation_event(offer, 0, 0);
                if muted {
                    queued.expected_sample_identity = Some(1);
                    queued.event.sample = Some(crate::SampleControls {
                        sample: crate::BUNDLED_BD_SAMPLE_ID,
                        playback_rate: 1.0,
                        begin: 0.0,
                        end: 1.0,
                        hold: crate::SampleHold::Hap,
                        muted: true,
                        loop_secs: None,
                        envelope_peak: 1.0,
                        reversed: false,
                        nudge_secs: 0.0,
                        cut: None,
                    });
                } else {
                    queued.event.controls.reverb = Some(crate::ReverbControls {
                        wet: 0.5,
                        size_secs: 0.1,
                        fade_secs: 0.1,
                        lp_start_hz: 15_000.0,
                        lp_end_hz: 1_000.0,
                        ir: None,
                    });
                }
                assert!(channel.publish(offer));
                assert!(shared.ring.push_queued(queued));
                let mut backend = confirmation_backend(&shared, 2);
                let mut meters = test_meter();
                let mut output = [0.0f32; 256];
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert_eq!(output.iter().any(|sample| sample.abs() > 1e-6), !muted);
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("permitted disposition copied")
                        .outcome,
                    crate::confirmation::WindowOutcome::Confirmed
                );
            }
        }
    }
    mod host_cache {
        use super::*;
        use std::rc::Rc;

        #[test]
        fn missing_selector_keeps_the_same_connected_host_across_retries() {
            let connection = Rc::new(());
            let mut cached = Some(Rc::clone(&connection));
            let mut probes = 0;

            for _ in 0..16 {
                let request =
                    Err::<(), _>(DevicePlaybackError::Unavailable("missing input".into()));
                let result = finish_cached_host_request(&mut cached, request, |_| {
                    probes += 1;
                    false
                });

                assert!(
                    matches!(result, Err(DevicePlaybackError::Unavailable(message)) if message == "missing input")
                );
                assert!(Rc::ptr_eq(
                    cached.as_ref().expect("cached host"),
                    &connection
                ));
                assert_eq!(Rc::strong_count(&connection), 2);
            }
            assert_eq!(probes, 16);
        }

        #[test]
        fn failed_connection_is_released_without_replacing_the_request_error() {
            let connection = Rc::new(());
            let mut cached = Some(Rc::clone(&connection));
            let request = Err::<(), _>(DevicePlaybackError::Unavailable(
                "device request failed".into(),
            ));

            let result = finish_cached_host_request(&mut cached, request, |_| true);

            assert!(
                matches!(result, Err(DevicePlaybackError::Unavailable(message)) if message == "device request failed")
            );
            assert!(cached.is_none(), "the next request must create a new host");
            assert_eq!(Rc::strong_count(&connection), 1);
        }

        #[test]
        fn successful_request_keeps_the_host_without_a_health_probe() {
            let connection = Rc::new(());
            let mut cached = Some(Rc::clone(&connection));

            let result = finish_cached_host_request(&mut cached, Ok(42), |_| {
                panic!("a successful request does not need a health probe")
            });

            assert_eq!(result.expect("successful request"), 42);
            assert!(Rc::ptr_eq(
                cached.as_ref().expect("cached host"),
                &connection
            ));
            assert_eq!(Rc::strong_count(&connection), 2);
        }
    }
    mod immediate_piano {
        use super::*;

        #[test]
        fn immediate_piano_bypasses_score_lookahead_and_preserves_score_pcm() {
            assert_immediate_piano_mix(false);
        }

        #[test]
        fn immediate_piano_shares_a_sample_with_the_score_without_sharing_voices() {
            assert_immediate_piano_mix(true);
        }

        #[test]
        fn immediate_piano_is_independent_of_score_orbit_controls() {
            let _tripwire_guard = tripwire_assertion_guard();
            for channels in [1, 2] {
                for control in ["mute", "duck", "djf", "channels", "output pair"] {
                    let shared = LiveShared::new(1);
                    let expected_shared = LiveShared::new(1);
                    // Keep both devices in explicit multi-pair mode, even when
                    // the score itself stays on the main pair.
                    shared.orbit_pairs[2].store(1, Ordering::Relaxed);
                    expected_shared.orbit_pairs[2].store(1, Ordering::Relaxed);
                    let mut backend = LiveScalarBackend::new(48_000, 8).unwrap();
                    let mut expected_backend = LiveScalarBackend::new(48_000, 8).unwrap();
                    for target in [&mut backend, &mut expected_backend] {
                        let pcm = crate::DecodedSample::from_parts(
                            48_000,
                            channels,
                            vec![0.25; 48_000 * usize::from(channels)],
                        )
                        .unwrap();
                        assert!(
                            target
                                .install_prepared_sample(crate::SampleId(1), Box::new(pcm))
                                .is_ok()
                        );
                    }
                    let mut score = AudioEvent {
                        onset_id: 1,
                        generation: 1,
                        target_frame: 0,
                        onset_lead: 0.0,
                        freq_hz: 220.0,
                        gain: 0.0,
                        duration_secs: 1.0,
                        ui_visuals: 0,
                        controls: crate::OscillatorControls {
                            orbit: 1,
                            ..Default::default()
                        },
                        sample: Some(crate::SampleControls {
                            sample: crate::SampleId(1),
                            playback_rate: 1.0,
                            begin: 0.0,
                            end: 1.0,
                            hold: crate::SampleHold::Slice,
                            muted: false,
                            loop_secs: None,
                            envelope_peak: 1.0,
                            reversed: false,
                            nudge_secs: 0.0,
                            cut: None,
                        }),
                        synth: None,
                        wavetable: None,
                        cut: None,
                    };
                    assert!(expected_shared.ring.push(score));
                    let mut piano = score;
                    piano.controls.piano = true;
                    piano.gain = 0.1;
                    match control {
                        "mute" => shared.orbit_gains[1].store(0.0f32.to_bits(), Ordering::Relaxed),
                        "duck" => {
                            score.controls.duck = Some(crate::DuckControls {
                                targets: [
                                    Some(crate::DuckTarget {
                                        orbit: 1,
                                        onset_secs: 0.0,
                                        attack_secs: 1.0,
                                        depth: 1.0,
                                    }),
                                    None,
                                    None,
                                    None,
                                ],
                            });
                        }
                        "djf" => score.controls.djf = Some(0.9),
                        "channels" => score.controls.channels = Some([2, 0]),
                        "output pair" => shared.orbit_pairs[1].store(1, Ordering::Relaxed),
                        _ => unreachable!(),
                    }
                    assert!(shared.ring.push(score));
                    let mut output = [0.0f32; 512];
                    let mut expected = [0.0f32; 512];
                    let mut meter = test_meter();
                    let mut expected_meter = test_meter();
                    write_live_output(&mut backend, &mut meter, &mut output, 4, &shared);
                    write_live_output(
                        &mut expected_backend,
                        &mut expected_meter,
                        &mut expected,
                        4,
                        &expected_shared,
                    );
                    assert!(shared.immediate.push(piano));
                    assert!(expected_shared.immediate.push(piano));
                    let before = tripwire::Violations::capture();
                    tripwire::audio_scope(|| {
                        write_live_output(&mut backend, &mut meter, &mut output, 4, &shared);
                        write_live_output(
                            &mut expected_backend,
                            &mut expected_meter,
                            &mut expected,
                            4,
                            &expected_shared,
                        );
                    });
                    assert!(tripwire::Violations::capture().since(before).clean());
                    assert!(expected.iter().any(|sample| sample.abs() > 0.001));
                    assert_eq!(
                        output, expected,
                        "{channels}-channel piano inherited score {control}"
                    );
                }
            }
        }

        fn assert_immediate_piano_mix(sampled: bool) {
            let _tripwire_guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let expected_shared = LiveShared::new(1);
            let event = |frequency, cut, piano| AudioEvent {
                onset_id: u64::MAX,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: frequency,
                gain: 0.1,
                duration_secs: f32::INFINITY,
                ui_visuals: 0,
                controls: crate::OscillatorControls {
                    piano,
                    ..Default::default()
                },
                sample: sampled.then_some(crate::SampleControls {
                    sample: crate::SampleId(1),
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: crate::SampleHold::Slice,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                }),
                synth: None,
                wavetable: None,
                cut,
            };
            let score = event(220.0, None, false);
            assert!(shared.ring.push(score));
            assert!(expected_shared.ring.push(score));
            let mut future = event(110.0, None, false);
            future.target_frame = 480_000;
            assert!(shared.ring.push(future));
            let mut backend = LiveScalarBackend::new(48_000, 8).unwrap();
            let mut expected_backend = LiveScalarBackend::new(48_000, 8).unwrap();
            if sampled {
                for target in [&mut backend, &mut expected_backend] {
                    let pcm =
                        crate::DecodedSample::from_parts(48_000, 1, vec![0.25; 48_000]).unwrap();
                    assert!(
                        target
                            .install_prepared_sample(crate::SampleId(1), Box::new(pcm))
                            .is_ok()
                    );
                }
            }
            let mut output = [0.0f32; 256];
            let mut expected = [0.0f32; 256];
            let mut meter = test_meter();
            let mut expected_meter = test_meter();
            // Put a far-future score event in the consumer's held slot first.
            write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
            write_live_output(
                &mut expected_backend,
                &mut expected_meter,
                &mut expected,
                2,
                &expected_shared,
            );
            let mut first = event(440.0, Some(1.0), true);
            first.target_frame = u64::MAX; // direct admission must ignore this
            assert!(shared.immediate.push(first));
            assert!(shared.immediate.push(event(660.0, Some(2.0), true)));
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
                write_live_output(
                    &mut expected_backend,
                    &mut expected_meter,
                    &mut expected,
                    2,
                    &expected_shared,
                );
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_ne!(output, expected, "piano sounds on the very next callback");
            assert_eq!(backend.pressure_observation().active_voices, 3);
            let mut release = event(440.0, Some(1.0), true);
            release.controls.choke_only = true;
            assert!(shared.immediate.push(release));
            for _ in 0..16 {
                write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
                write_live_output(
                    &mut expected_backend,
                    &mut expected_meter,
                    &mut expected,
                    2,
                    &expected_shared,
                );
            }
            assert_eq!(backend.pressure_observation().active_voices, 2);
            release.cut = Some(2.0);
            assert!(shared.immediate.push(release));
            for _ in 0..16 {
                write_live_output(&mut backend, &mut meter, &mut output, 2, &shared);
                write_live_output(
                    &mut expected_backend,
                    &mut expected_meter,
                    &mut expected,
                    2,
                    &expected_shared,
                );
            }
            assert_eq!(
                output, expected,
                "releasing piano leaves score PCM unchanged"
            );
            assert_eq!(backend.pressure_observation().active_voices, 1);
            assert!(output.iter().any(|sample| sample.abs() > 0.001));
        }
    }
    mod inventory {
        use super::*;
        use std::cell::Cell;
        use std::hash::{Hash, Hasher};
        use std::rc::Rc;

        #[derive(Debug, Default)]
        struct ConfigProbes {
            input: Cell<usize>,
            output: Cell<usize>,
        }

        #[derive(Clone, Debug)]
        struct ListedDevice {
            id: &'static str,
            name: &'static str,
            input: bool,
            output: bool,
            config_available: bool,
            probes: Rc<ConfigProbes>,
        }

        impl ListedDevice {
            fn new(id: &'static str, name: &'static str, input: bool, output: bool) -> Self {
                Self {
                    id,
                    name,
                    input,
                    output,
                    config_available: true,
                    probes: Rc::default(),
                }
            }

            fn listed_id(&self) -> String {
                self.id().expect("fake device id").to_string()
            }

            fn default_config(
                &self,
                input: bool,
            ) -> Result<cpal::SupportedStreamConfig, cpal::Error> {
                let count = if input {
                    &self.probes.input
                } else {
                    &self.probes.output
                };
                count.set(count.get() + 1);
                if !self.config_available {
                    return Err(cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable));
                }
                Ok(cpal::SupportedStreamConfig::new(
                    if input { 1 } else { 2 },
                    if input { 44_100 } else { 48_000 },
                    SupportedBufferSize::Unknown,
                    SampleFormat::F32,
                ))
            }

            fn assert_probes(&self, input: usize, output: usize) {
                assert_eq!(self.probes.input.get(), input, "{} input probes", self.id);
                assert_eq!(
                    self.probes.output.get(),
                    output,
                    "{} output probes",
                    self.id
                );
            }
        }

        impl PartialEq for ListedDevice {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }

        impl Eq for ListedDevice {}

        impl Hash for ListedDevice {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }

        impl std::fmt::Display for ListedDevice {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.name)
            }
        }

        impl DeviceTrait for ListedDevice {
            type SupportedInputConfigs = std::vec::IntoIter<cpal::SupportedStreamConfigRange>;
            type SupportedOutputConfigs = std::vec::IntoIter<cpal::SupportedStreamConfigRange>;
            type Stream = cpal::Stream;

            fn description(&self) -> Result<cpal::DeviceDescription, cpal::Error> {
                Ok(cpal::DeviceDescriptionBuilder::new(self.name).build())
            }

            fn id(&self) -> Result<cpal::DeviceId, cpal::Error> {
                Ok(cpal::DeviceId::new(cpal::ALL_HOSTS[0], self.id))
            }

            // ALSA determines direction from device metadata during enumeration.
            fn supports_input(&self) -> bool {
                self.input
            }

            fn supports_output(&self) -> bool {
                self.output
            }

            fn supported_input_configs(&self) -> Result<Self::SupportedInputConfigs, cpal::Error> {
                panic!("listing must not request input formats from the fake host")
            }

            fn supported_output_configs(
                &self,
            ) -> Result<Self::SupportedOutputConfigs, cpal::Error> {
                panic!("listing must not request output formats from the fake host")
            }

            fn default_input_config(&self) -> Result<cpal::SupportedStreamConfig, cpal::Error> {
                self.default_config(true)
            }

            fn default_output_config(&self) -> Result<cpal::SupportedStreamConfig, cpal::Error> {
                self.default_config(false)
            }

            fn build_input_stream_raw<D, E>(
                &self,
                _config: cpal::StreamConfig,
                _sample_format: SampleFormat,
                _data_callback: D,
                _error_callback: E,
                _timeout: Option<Duration>,
            ) -> Result<Self::Stream, cpal::Error>
            where
                D: FnMut(&cpal::Data, &cpal::InputCallbackInfo) + Send + 'static,
                E: FnMut(cpal::Error) + Send + 'static,
            {
                panic!("listing must not open an input stream")
            }

            fn build_output_stream_raw<D, E>(
                &self,
                _config: cpal::StreamConfig,
                _sample_format: SampleFormat,
                _data_callback: D,
                _error_callback: E,
                _timeout: Option<Duration>,
            ) -> Result<Self::Stream, cpal::Error>
            where
                D: FnMut(&mut cpal::Data, &cpal::OutputCallbackInfo) + Send + 'static,
                E: FnMut(cpal::Error) + Send + 'static,
            {
                panic!("listing must not open an output stream")
            }
        }

        #[derive(Default)]
        struct InventoryHost {
            devices: Vec<ListedDevice>,
            default_input: Option<&'static str>,
            default_output: Option<&'static str>,
            unavailable: bool,
        }

        impl HostTrait for InventoryHost {
            type Devices = std::vec::IntoIter<ListedDevice>;
            type Device = ListedDevice;

            fn is_available() -> bool {
                true
            }

            fn devices(&self) -> Result<Self::Devices, cpal::Error> {
                if self.unavailable {
                    return Err(cpal::Error::new(cpal::ErrorKind::HostUnavailable));
                }
                Ok(self.devices.clone().into_iter())
            }

            fn default_input_device(&self) -> Option<Self::Device> {
                self.devices
                    .iter()
                    .find(|device| Some(device.id) == self.default_input)
                    .cloned()
            }

            fn default_output_device(&self) -> Option<Self::Device> {
                self.devices
                    .iter()
                    .find(|device| Some(device.id) == self.default_output)
                    .cloned()
            }
        }

        #[test]
        fn repeated_inventory_keeps_metadata_without_default_config_probes() {
            let duplex = ListedDevice::new("duplex", "Interface", true, true);
            let output = ListedDevice::new("output", "Interface", false, true);
            let input = ListedDevice::new("input", " ", true, false);
            let host = InventoryHost {
                devices: vec![duplex.clone(), output.clone(), input.clone()],
                default_input: Some(input.id),
                default_output: Some(duplex.id),
                ..Default::default()
            };

            for _ in 0..3 {
                let (outputs, inputs) =
                    list_audio_devices(&host, AudioDeviceDetails::Metadata).unwrap();
                assert_eq!(outputs.len(), 2);
                assert_eq!(inputs.len(), 2);
                assert_eq!(outputs[0].id, duplex.listed_id());
                assert_eq!(outputs[1].id, output.listed_id());
                assert_eq!(outputs[0].name, "Interface");
                assert_eq!(
                    outputs[1].name,
                    format!("Interface ({})", output.listed_id())
                );
                assert!(outputs[0].is_default);
                assert!(!outputs[1].is_default);
                assert_eq!(inputs[0].id, duplex.listed_id());
                assert_eq!(inputs[0].name, "Interface");
                assert!(!inputs[0].is_default);
                assert_eq!(inputs[1].id, input.listed_id());
                assert_eq!(inputs[1].name, input.listed_id());
                assert!(inputs[1].is_default);
                for device in outputs.iter().chain(&inputs) {
                    assert_eq!(device.sample_rate, None);
                    assert_eq!(device.channels, None);
                }
            }
            duplex.assert_probes(0, 0);
            output.assert_probes(0, 0);
            input.assert_probes(0, 0);
        }

        #[test]
        fn detailed_listing_reads_each_direction_and_keeps_unreadable_devices() {
            let duplex = ListedDevice::new("duplex", "Interface", true, true);
            let mut unavailable = ListedDevice::new("unavailable", "Busy output", false, true);
            unavailable.config_available = false;
            let host = InventoryHost {
                devices: vec![duplex.clone(), unavailable.clone()],
                default_input: Some(duplex.id),
                default_output: Some(duplex.id),
                ..Default::default()
            };

            let (outputs, inputs) =
                list_audio_devices(&host, AudioDeviceDetails::DefaultConfig).unwrap();
            assert_eq!(outputs.len(), 2);
            assert_eq!(inputs.len(), 1);
            assert_eq!(outputs[0].sample_rate, Some(48_000));
            assert_eq!(outputs[0].channels, Some(2));
            assert!(outputs[0].is_default);
            assert_eq!(inputs[0].sample_rate, Some(44_100));
            assert_eq!(inputs[0].channels, Some(1));
            assert!(inputs[0].is_default);
            assert_eq!(outputs[1].id, unavailable.listed_id());
            assert_eq!(outputs[1].name, "Busy output");
            assert_eq!(outputs[1].sample_rate, None);
            assert_eq!(outputs[1].channels, None);
            duplex.assert_probes(1, 1);
            unavailable.assert_probes(0, 1);
        }

        #[test]
        fn inventory_refresh_observes_hotplug_and_default_changes() {
            let old = ListedDevice::new("old", "Old output", false, true);
            let new = ListedDevice::new("new", "New interface", true, true);
            let mut host = InventoryHost {
                devices: vec![old.clone()],
                default_output: Some(old.id),
                ..Default::default()
            };
            let (outputs, inputs) =
                list_audio_devices(&host, AudioDeviceDetails::Metadata).unwrap();
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].id, old.listed_id());
            assert!(outputs[0].is_default);
            assert!(inputs.is_empty());

            host.devices = vec![new.clone()];
            host.default_input = Some(new.id);
            host.default_output = Some(new.id);
            let (outputs, inputs) =
                list_audio_devices(&host, AudioDeviceDetails::Metadata).unwrap();
            assert_eq!(outputs.len(), 1);
            assert_eq!(inputs.len(), 1);
            assert_eq!(outputs[0].id, new.listed_id());
            assert_eq!(inputs[0].id, new.listed_id());
            assert!(outputs[0].is_default);
            assert!(inputs[0].is_default);
            old.assert_probes(0, 0);
            new.assert_probes(0, 0);
        }

        #[test]
        fn inventory_reports_host_enumeration_failure() {
            let host = InventoryHost {
                unavailable: true,
                ..Default::default()
            };
            assert!(matches!(
                list_audio_devices(&host, AudioDeviceDetails::Metadata),
                Err(DevicePlaybackError::Unavailable(_))
            ));
        }
    }
    mod output_activation {
        use super::*;

        #[cfg(feature = "test-support")]
        #[test]
        fn manual_output_renders_queued_audio_and_acknowledges_stop() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let mut manual = ManualLiveOutput::new(48_000, 7).expect("manual output");
            let device = manual.device();
            assert!(device.is_silent());
            assert_eq!(device.clock_frames(), 0);
            assert!(device.push(AudioEvent {
                onset_id: 1,
                generation: 7,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            assert_eq!(device.report().callbacks, 0);
            assert_eq!(device.report().ring_depth, 1);

            let mut output = [0.0f32; 256 * 2];
            let before = tripwire::Violations::capture();
            manual.render(&mut output);
            assert!(tripwire::Violations::capture().since(before).clean());
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            let report = manual.device().report();
            assert_eq!(report.callbacks, 1);
            assert_eq!(report.submitted_frames, 256);
            assert_eq!(report.accepted_events, 1);
            assert_eq!(report.ring_depth, 0);
            assert_eq!(report.ring_role_conflicts, 0);
            assert_eq!(report.callback_scope_misses, 0);

            manual.device().stop();
            assert!(!manual.device().report().stop_acknowledged);
            let before = tripwire::Violations::capture();
            for buffer in 0..3 {
                manual.render(&mut output);
                // The first 256 frames sit inside the 480-frame ramp.
                assert_eq!(manual.device().report().stop_acknowledged, buffer > 0);
            }
            assert!(tripwire::Violations::capture().since(before).clean());
            let report = manual.device().report();
            assert!(report.stop_acknowledged);
            assert_eq!(report.callbacks, 4);
            assert_eq!(report.submitted_frames, 1024);
            assert_eq!(report.accepted_events, 1);
            assert!(output.iter().all(|sample| *sample == 0.0));
        }

        #[cfg(feature = "test-support")]
        #[test]
        fn tripwire_rejects_prior_violation_without_erasing_it_and_fresh_output_arms() {
            let _tripwire_guard = tripwire_assertion_guard();
            let mut manual = ManualLiveOutput::new(48_000, 1).expect("manual output");
            let device = &mut manual.device;
            device.arm_callback_tripwire_checked().expect("first arm");
            device
                .arm_callback_tripwire_checked()
                .expect("repeat clean arm");
            assert_eq!(device.report().callback_allocations, 0);
            assert_eq!(device.report().callback_frees, 0);

            tripwire::audio_scope(|| {
                let allocation = vec![1_u8; 8];
                std::hint::black_box(allocation);
            });
            let report = device.report();
            assert_eq!(report.callback_allocations, 1);
            assert_eq!(report.callback_frees, 1);
            assert_eq!(
                device.arm_callback_tripwire_checked(),
                Err(CallbackTripwireArmError::PriorViolation {
                    allocations: 1,
                    frees: 1,
                    scope_misses: 0,
                })
            );
            assert_eq!(device.report().callback_allocations, 1);
            assert_eq!(device.report().callback_frees, 1);

            drop(manual);
            let mut fresh = ManualLiveOutput::new(48_000, 2).expect("fresh output");
            fresh
                .device
                .arm_callback_tripwire_checked()
                .expect("fresh reporting window");
            assert_eq!(fresh.device().report().callback_allocations, 0);
            assert_eq!(fresh.device().report().callback_frees, 0);
        }

        #[test]
        fn pending_callbacks_write_silence_in_every_host_format() {
            fn check<T>()
            where
                T: SizedSample + FromSample<f32> + PartialEq + std::fmt::Debug,
            {
                assert!(
                    sample_format_from_cpal(T::FORMAT).is_some(),
                    "{}",
                    T::FORMAT
                );
                let gate = CallbackGate::new();
                let mut output = [T::from_sample(0.5); 7];
                gate.render_or_silence(&mut output, |_| panic!("pending callback entered DSP"));
                assert_eq!(output, [T::from_sample(0.0); 7]);
            }
            check::<f32>();
            check::<f64>();
            check::<i8>();
            check::<i16>();
            check::<I24>();
            check::<i32>();
            check::<i64>();
            check::<u8>();
            check::<u16>();
            check::<U24>();
            check::<u32>();
            check::<u64>();
        }

        #[test]
        fn pending_callbacks_leave_queues_and_shared_state_for_activation() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let dispatch = DspDispatch::portable();
            let mut device = LiveScalarDevice::start_silent_with_options(
                48_000,
                7,
                LiveOutputOptions::default().with_dispatch(dispatch),
            )
            .expect("silent output");
            device.silent.take().expect("callback").stop();
            // SAFETY: the prior callback is joined before this thread becomes the
            // consumer, so asset inspection below never races a silent callback.
            unsafe { device.shared.ring.release_consumer() };
            device.shared.input.set_channels(1);
            device.shared.input.write(&[0.25; 4096], 1);
            device.set_master_gain(0.5);
            device.set_analysis_enabled(true);
            device
                .install_sample(
                    crate::sample::SampleId(0),
                    crate::sample::DecodedSample::from_parts(48_000, 1, vec![0.1; 256])
                        .expect("sample"),
                )
                .expect("sample install");
            let params = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            assert!(device.push(AudioEvent {
                onset_id: 1,
                generation: 7,
                target_frame: device.clock_frames(),
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: Some(crate::SynthSource::Input { channel: 0 }),
                wavetable: None,
                cut: None,
            }));
            let shared = &device.shared;
            let mut backend = PreparedLiveOutput::backend(
                shared,
                48_000,
                LiveOutputOptions::default().with_dispatch(dispatch),
                &[],
            )
            .expect("backend");
            let mut meters = LiveCallbackMeters::new(48_000, &shared.realtime_load);
            let gate = CallbackGate::new();
            let before = shared.report(1);
            assert_eq!(before.ring_depth, 1);
            assert_eq!(before.asset_queues.sample_installs, 1);
            assert_eq!(before.asset_queues.orbit_reverb_installs, 1);
            assert_eq!(before.asset_queues.fx_reverb_installs, 1);
            let origin = shared.playback_origin_nanos.load(Ordering::Acquire);
            let violations = tripwire::Violations::capture();
            let mut output = [0.5f32; 256];
            // A host may call more than once during build and again during play.
            for _ in 0..3 {
                gate.render_or_silence(&mut output, |output| {
                    shared.playback_origin_nanos.store(123, Ordering::Release);
                    write_live_output(&mut backend, &mut meters, output, 2, shared);
                });
                assert_eq!(output, [0.0; 256]);
                assert_eq!(shared.report(1), before);
                assert_eq!(shared.playback_origin_nanos.load(Ordering::Acquire), origin);
            }
            gate.activate().expect("activate once");
            gate.render_or_silence(&mut output, |output| {
                write_live_output(&mut backend, &mut meters, output, 2, shared);
            });
            assert!(tripwire::Violations::capture().since(violations).clean());
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(output.iter().any(|sample| sample.abs() > 1e-6));
            let after = shared.report(1);
            assert_eq!(after.submitted_frames, before.submitted_frames + 128);
            assert_eq!(after.accepted_events, before.accepted_events + 1);
            assert_eq!(after.ring_depth, 0);
            assert_eq!(after.ring_role_conflicts, 0);
            assert_eq!(after.asset_queues.sample_installs, 0);
            assert_eq!(after.asset_queues.orbit_reverb_installs, 0);
            assert_eq!(after.asset_queues.fx_reverb_installs, 0);
            assert!(gate.activate().is_err(), "activation cannot be repeated");
        }

        #[test]
        fn callback_errors_before_activation_do_not_fail_the_running_output() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let shared = LiveShared::new(7);
            let pending = CallbackGate::new();
            let before = shared.report(1);
            let violations = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                pending.report_error(&shared.failed, &shared.callback_errors);
                pending.report_error(&shared.failed, &shared.callback_errors);
            });
            assert!(tripwire::Violations::capture().since(violations).clean());
            assert!(!shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.report(1), before);
            assert!(pending.activate().is_err());
            let mut output = [1.0f32; 8];
            pending.render_or_silence(&mut output, |_| panic!("failed candidate rendered"));
            assert_eq!(output, [0.0; 8]);
            let mut started = None;
            assert_eq!(
                silent_elapsed(&mut started, &pending, || panic!(
                    "failed gate read the clock"
                )),
                None
            );
            assert!(started.is_none());

            let active = CallbackGate::new();
            active.activate().expect("active output");
            active.report_error(&shared.failed, &shared.callback_errors);
            active.report_error(&shared.failed, &shared.callback_errors);
            assert!(
                active.is_active(),
                "active errors keep the normal stop path"
            );
            assert!(shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.callback_errors.load(Ordering::Acquire), 2);
        }

        #[test]
        fn silent_pacing_starts_at_activation_not_preparation() {
            let gate = CallbackGate::new();
            let prepared_at = Instant::now();
            let mut started = None;
            assert_eq!(
                silent_elapsed(&mut started, &gate, || panic!(
                    "pending gate read the clock"
                )),
                None
            );
            let activated_at = prepared_at + Duration::from_secs(30);
            assert_eq!(
                silent_elapsed(&mut started, &gate, || panic!(
                    "pending gate read the clock"
                )),
                None
            );
            assert!(started.is_none());
            gate.activate().expect("activate");
            assert_eq!(
                silent_elapsed(&mut started, &gate, || activated_at),
                Some(Duration::ZERO)
            );
            assert_eq!(started, Some(activated_at));
            assert_eq!(
                silent_elapsed(&mut started, &gate, || {
                    activated_at + Duration::from_millis(10)
                }),
                Some(Duration::from_millis(10)),
            );
        }

        #[test]
        fn prepared_silent_drop_joins_without_stopping_shared_playback() {
            let shared = LiveShared::new(7);
            shared.frames.store(48_000, Ordering::Release);
            shared.input.set_channels(1);
            for fail in [false, true] {
                let before = shared.report(1);
                let input_owners = Arc::strong_count(&shared.input);
                let prepared = PreparedLiveOutput::silent(
                    shared.clone(),
                    48_000,
                    LiveOutputOptions::default().with_dispatch(DspDispatch::portable()),
                    &[],
                )
                .expect("prepared output");
                let gate = Arc::clone(&prepared.gate);
                prepared.start_gated().expect("gated start");
                if fail {
                    gate.report_error(&shared.failed, &shared.callback_errors);
                    assert!(prepared.activate().is_err());
                } else {
                    drop(prepared);
                }
                // The callback retains a gate reference until its thread exits.
                assert_eq!(Arc::strong_count(&gate), 1, "candidate thread was joined");
                assert_eq!(Arc::strong_count(&shared.input), input_owners);
                assert_eq!(shared.report(1), before);
                assert!(!shared.stopped.load(Ordering::Acquire));
                assert_eq!(shared.input.channels(), 1);
            }
        }

        #[test]
        fn prepared_silent_activation_transfers_the_thread_to_its_handle() {
            let shared = LiveShared::new(7);
            let prepared = PreparedLiveOutput::silent(
                shared.clone(),
                48_000,
                LiveOutputOptions::default(),
                &[],
            )
            .expect("prepared output");
            let gate = Arc::clone(&prepared.gate);
            prepared.start_gated().expect("gated start");
            assert_eq!(shared.clock_frames(), 0);
            let handle = prepared.activate().expect("activate");
            let deadline = Instant::now() + Duration::from_secs(3);
            while shared.callbacks.load(Ordering::Acquire) == 0 {
                assert!(Instant::now() < deadline, "activated callback never ran");
                std::thread::sleep(Duration::from_millis(2));
            }
            drop(handle);
            assert_eq!(
                Arc::strong_count(&gate),
                1,
                "active handle joined its thread"
            );
            assert!(shared.clock_frames() >= u64::from(SILENT_BUFFER_FRAMES));
            assert_eq!(shared.ring.role_conflicts.load(Ordering::Acquire), 0);
            assert!(!shared.stopped.load(Ordering::Acquire));
        }

        /// A held silent clock stays put however long the test looks at it,
        /// and a released one resumes where it stopped: no burst of blocks for
        /// the time spent held.
        ///
        /// The burst is bounded without racing the silent thread. Its clock
        /// only ever counts wall time since its gate opened, less the time it
        /// spent held, and delivers nothing it has not counted. The gate opens
        /// after `opened` (taken once the output is prepared, so preparation
        /// does not blunt the check), and the thread's held span covers the one
        /// measured here from the hold's acknowledgement to just before the
        /// release. So however late this thread runs, the frozen clock is never
        /// past the unheld wall time from `opened` to now, where resuming from
        /// the old origin puts it a whole hold ahead.
        #[cfg(feature = "test-support")]
        #[test]
        fn a_held_silent_clock_freezes_and_resumes_without_a_catch_up_burst() {
            const RATE: u32 = 48_000;
            const HOLD: Duration = Duration::from_millis(100);
            let shared = LiveShared::new(7);
            let options = LiveOutputOptions::default();
            let prepared = PreparedLiveOutput::silent(shared.clone(), RATE, options, &[])
                .expect("prepared silent output");
            let opened = Instant::now();
            let device =
                LiveScalarDevice::start_prepared(shared, options, prepared).expect("silent output");
            let deadline = Instant::now() + Duration::from_secs(5);
            while device.shared.clock_frames() == 0 {
                assert!(Instant::now() < deadline, "the silent output never started");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(device.hold_silent_clock_for_test(true));
            let held_from = Instant::now();
            let held_at = device.shared.clock_frames();
            std::thread::sleep(HOLD);
            assert_eq!(
                device.shared.clock_frames(),
                held_at,
                "a held clock does not move"
            );
            let held = held_from.elapsed();

            assert!(device.hold_silent_clock_for_test(false));
            let deadline = Instant::now() + Duration::from_secs(5);
            while device.shared.clock_frames() == held_at {
                assert!(Instant::now() < deadline, "a released clock never resumed");
                std::thread::sleep(Duration::from_micros(200));
            }
            assert!(device.hold_silent_clock_for_test(true));
            let frozen = device.shared.clock_frames();
            let unheld = opened.elapsed().saturating_sub(held);
            let pace = (unheld.as_secs_f64() * f64::from(RATE)) as u64;
            // One frame of slack for the float products on either side.
            assert!(
                frozen <= pace + 1,
                "the released clock resumed with a catch-up burst: {frozen} frames against \
             {pace} frames of unheld wall time ({held:?} held)"
            );
            assert!(device.hold_silent_clock_for_test(false));
        }

        #[test]
        fn silent_output_reports_only_facts_it_actually_has() {
            let device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            assert!(!device.dispatch().is_forced_portable());
            let facts = device.audio_facts();
            let output = facts.output();

            assert_eq!(output.host(), &AudioHost::Silent);
            assert_eq!(output.host().id(), None);
            assert_eq!(output.device_id(), SILENT_OUTPUT_NAME);
            assert_eq!(output.sample_rate_hz(), 48_000);
            assert_eq!(output.channels(), 2);
            assert_eq!(output.sample_format(), None);
            assert_eq!(output.buffer_preference(), AudioBufferPreference::Auto);
            assert_eq!(output.requested_buffer_frames(), SILENT_BUFFER_FRAMES);
            assert_eq!(output.reported_buffer_frames(), Some(SILENT_BUFFER_FRAMES));
            assert!(!output.host_timestamps_available());
            assert_eq!(facts.input(), None);
        }
    }
    mod output_recycle {
        use super::*;

        #[test]
        fn silent_recycle_reinstalls_unchanged_reverb_parameters() {
            fn take_prepared_reverbs(
                device: &LiveScalarDevice,
            ) -> (
                Box<crate::reverb::OrbitReverb>,
                Box<crate::reverb::OrbitReverb>,
            ) {
                let install = device
                    .shared
                    .samples
                    .reverb_installs
                    .pop()
                    .expect("prepared orbit reverb");
                assert_eq!(install.orbit, 0);
                // SAFETY: the sole callback has been joined before this helper
                // runs. Popping transfers the allocation to this thread.
                let orbit = unsafe { Box::from_raw(install.reverb) };
                let install = device
                    .shared
                    .samples
                    .fx_reverb_installs
                    .pop()
                    .expect("prepared stage reverb");
                // SAFETY: as above, this is the popped allocation's only owner.
                let stage = unsafe { Box::from_raw(install.reverb) };
                (orbit, stage)
            }

            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            let params = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            let (orbit, stage) = take_prepared_reverbs(&device);
            assert_eq!(orbit.params(), params);
            assert_eq!(stage.params(), params);
            drop((orbit, stage));

            // Empty queues do not invalidate the producer fingerprints. Repeated
            // requests must remain cache hits until the backend is replaced.
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            assert_eq!(device.shared.samples.reverb_installs.len(), 0);
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), 0);

            device.recycle_output().expect("recycle silent output");
            device.silent.take().expect("replacement callback").stop();
            device.ensure_reverb(0, params);
            device.ensure_fx_reverb(params);
            let (orbit, stage) = take_prepared_reverbs(&device);
            assert_eq!(orbit.params(), params);
            assert_eq!(stage.params(), params);
            assert_eq!(device.shared.samples.reverb_installs.len(), 0);
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), 0);
        }

        /// An output's reverbs are counted where the producer ships them. The
        /// same room is the reverb already there, a new room on an orbit takes
        /// the old one's place rather than adding to it, and a recycled backend
        /// holds none until asked again. The backend is measured once, before
        /// the callback owns it.
        #[test]
        fn an_outputs_memory_counts_each_orbit_reverb_once() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let opened = device.memory();
            assert!(opened.backend > 0, "{opened:?}");
            assert_eq!((opened.reverbs, opened.input, opened.record), (0, 0, 0));
            let small = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            let large = crate::reverb::ReverbParams {
                size_secs: 0.5,
                ..small
            };

            device.ensure_reverb(0, small);
            let one = device.memory().reverbs;
            assert!(one > 0);
            device.ensure_reverb(0, small);
            assert_eq!(device.memory().reverbs, one, "the same room");
            device.ensure_reverb(0, large);
            let larger = device.memory().reverbs;
            assert!(
                larger > one,
                "replaced by the larger room: {larger} against {one}"
            );
            device.ensure_reverb(1, small);
            assert_eq!(device.memory().reverbs, larger + one, "another orbit's own");

            device.recycle_output().expect("recycle silent output");
            let recycled = device.memory();
            assert_eq!(recycled.reverbs, 0, "a fresh backend has none yet");
            assert_eq!(recycled.backend, opened.backend, "the same backend again");
        }

        /// More than eight room settings remain usable across repeated cache cycles.
        #[test]
        fn nine_distinct_stage_reverbs_repeat_after_cache_eviction() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            let room = |index: usize| crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0 + index as f32,
                ir: None,
            };
            let mut installed = Vec::new();
            for _ in 0..3 {
                for index in 0..9 {
                    device.ensure_fx_reverb(room(index));
                }
                assert_eq!(device.shared.samples.fx_reverb_installs.len(), 9);
                for (index, install) in device
                    .shared
                    .samples
                    .fx_reverb_installs
                    .drain_available()
                    .enumerate()
                {
                    // SAFETY: the callback is stopped. This thread owns the popped box.
                    let reverb = unsafe { Box::from_raw(install.reverb) };
                    assert_eq!(reverb.params(), room(index));
                    installed.push(reverb);
                }
            }
            assert_eq!(installed.len(), 27);
            assert_eq!(device.report().asset_queues.fx_reverb_refusals, 0);
        }

        #[test]
        fn returning_an_old_room_does_not_invalidate_its_replacement() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            let room = |index: usize| crate::reverb::ReverbParams {
                size_secs: 0.001,
                fade_secs: 0.0,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0 + index as f32,
                ir: None,
            };
            device.ensure_fx_reverb(room(0));
            let old = device
                .shared
                .samples
                .fx_reverb_installs
                .pop()
                .expect("old room");
            for index in 1..=8 {
                device.ensure_fx_reverb(room(index));
            }
            let other_rooms: Vec<_> = device
                .shared
                .samples
                .fx_reverb_installs
                .drain_available()
                .map(|install| {
                    // SAFETY: the callback is stopped. This thread owns each popped box.
                    unsafe { Box::from_raw(install.reverb) }
                })
                .collect();
            device.ensure_fx_reverb(room(0));
            let replacement = device
                .shared
                .samples
                .fx_reverb_installs
                .pop()
                .expect("replacement room");
            assert_ne!(old.reverb, replacement.reverb);

            assert!(
                device
                    .shared
                    .samples
                    .fx_reverb_returns
                    .push(crate::assets::ReturnedFxReverb(old.reverb))
                    .is_ok()
            );
            device.reclaim_assets();
            device.ensure_fx_reverb(room(0));
            assert_eq!(
                device.shared.samples.fx_reverb_installs.len(),
                0,
                "the replacement remains cached"
            );

            assert!(
                device
                    .shared
                    .samples
                    .fx_reverb_returns
                    .push(crate::assets::ReturnedFxReverb(replacement.reverb))
                    .is_ok()
            );
            device.reclaim_assets();
            device.ensure_fx_reverb(room(0));
            assert_eq!(
                device.shared.samples.fx_reverb_installs.len(),
                1,
                "returning the replacement invalidates its cache entry"
            );
            drop(other_rooms);
        }

        #[test]
        fn a_room_above_the_byte_budget_is_refused_once_and_not_generated_again() {
            let mut device = LiveScalarDevice::start_silent(192_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            let oversized = crate::reverb::ReverbParams {
                size_secs: 6.0,
                fade_secs: 0.0,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_fx_reverb(oversized);
            device.ensure_fx_reverb(oversized);
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), 0);
            assert_eq!(device.report().asset_queues.fx_reverb_refusals, 1);
        }

        #[test]
        fn a_full_stage_install_ring_counts_a_refusal_and_allows_a_retry() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            let room = |index: usize| crate::reverb::ReverbParams {
                size_secs: 0.001,
                fade_secs: 0.0,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0 + index as f32,
                ir: None,
            };
            let capacity = device.shared.samples.fx_reverb_installs.capacity();
            for index in 0..=capacity {
                device.ensure_fx_reverb(room(index));
            }
            assert_eq!(device.shared.samples.fx_reverb_installs.len(), capacity);
            assert_eq!(device.report().asset_queues.fx_reverb_refusals, 1);
            let returned = device
                .shared
                .samples
                .fx_reverb_installs
                .pop()
                .expect("queued room");
            assert!(
                device
                    .shared
                    .samples
                    .fx_reverb_returns
                    .push(crate::assets::ReturnedFxReverb(returned.reverb))
                    .is_ok()
            );
            device.ensure_fx_reverb(room(capacity));
            assert_eq!(
                device.shared.samples.fx_reverb_installs.len(),
                capacity,
                "the refused parameters can be retried"
            );
            assert_eq!(device.report().asset_queues.fx_reverb_refusals, 1);
        }

        /// When the byte budget fills, the callback retires an idle room and admits
        /// the next request without freeing large IR buffers on the audio thread.
        #[test]
        fn a_stage_reverb_replaces_an_idle_room_at_the_byte_budget() {
            let device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let room = |lp_end_hz| crate::reverb::ReverbParams {
                size_secs: 6.0,
                fade_secs: 0.1,
                lp_start_hz: 15_000.0,
                lp_end_hz,
                ir: None,
            };
            device.ensure_fx_reverb(room(1_000.0));
            let each = device.memory().reverbs;
            let budget = crate::scalar::MAX_FX_REVERB_BYTES;
            assert!(
                each > 0 && 2 * each <= budget && 3 * each > budget,
                "two rooms fit the budget and a third does not: {each} each"
            );
            device.ensure_fx_reverb(room(2_000.0));
            assert_eq!(device.memory().reverbs, 2 * each, "both kept");
            device.ensure_fx_reverb(room(3_000.0));
            assert_eq!(
                device.memory().reverbs,
                2 * each,
                "the idle room is displaced without exceeding the budget"
            );

            // The callback hands an idle old room back, while accepting the third.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while device.report().asset_queues.fx_reverb_returns == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the callback never retired an old room"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(device.report().asset_queues.fx_reverb_refusals, 0);
        }

        /// The callback count is what the studio settles a retiring id
        /// against: it climbs one per completed block, and a recycle keeps the
        /// shared counter, so a count read before it is still comparable after.
        #[test]
        fn callbacks_count_completed_blocks_and_survive_a_recycle() {
            fn wait_past(device: &LiveScalarDevice, count: u64) -> u64 {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let now = device.callbacks();
                    if now > count {
                        return now;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the silent output stopped at {now} callbacks"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }

            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let first = wait_past(&device, 0);
            let second = wait_past(&device, first);
            assert!(second > first, "one more block, one more callback");

            device.recycle_output().expect("recycle silent output");
            let after = wait_past(&device, second);
            assert!(
                after > second,
                "the count carried over the recycle: {second} then {after}"
            );
        }

        #[test]
        fn silent_recycle_keeps_input_audio_routed() {
            fn wait_for_input(device: &LiveScalarDevice, sign: f32) -> LiveAnalysisSnapshot {
                let start = device.clock_frames();
                assert!(device.push(AudioEvent {
                    onset_id: start,
                    generation: device.generation(),
                    target_frame: start,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.5,
                    duration_secs: 10.0,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: Some(crate::SynthSource::Input { channel: 0 }),
                    wavetable: None,
                    cut: None,
                }));
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut output = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
                loop {
                    if let Some(snapshot) = device.copy_analysis_window(&mut output) {
                        assert!(output.iter().all(|sample| sample.is_finite()));
                        if snapshot.end_frame > start
                            && output.iter().any(|sample| sample * sign > 1e-6)
                        {
                            return snapshot;
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "input stayed silent: rendered {} frames after enqueue",
                        device.clock_frames().saturating_sub(start),
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }

            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.shared.input.set_channels(1);
            device.shared.input.write(&[0.25; 4096], 1);
            device.set_analysis_enabled(true);
            let before = wait_for_input(&device, 1.0);

            device.recycle_output().expect("recycle silent output");
            // New input with the opposite sign cannot be mistaken for PCM from
            // the discarded backend, even if an old analysis window survived.
            device.shared.input.write(&[-0.25; 4096], 1);
            let after = wait_for_input(&device, -1.0);
            assert_ne!(after.epoch, before.epoch);
            assert_eq!(device.input_channels(), 1);
        }

        #[test]
        fn silent_selector_retains_prepared_output_options() {
            for dispatch in [DspDispatch::automatic(), DspDispatch::portable()] {
                let preference = AudioBufferPreference::Frames(512);
                let options = LiveOutputOptions::default()
                    .with_dispatch(dispatch)
                    .with_buffer_preference(preference);
                let device = LiveScalarDevice::start_output_with_options(
                    Some(SILENT_OUTPUT_NAME),
                    7,
                    options,
                )
                .expect("selected silent output");
                assert_eq!(device.audio_facts().output().host(), &AudioHost::Silent);
                assert_eq!(device.output_options().buffer_preference(), preference);
                assert_eq!(
                    device.audio_facts().output().buffer_preference(),
                    preference
                );
                assert_eq!(device.requested_buffer_frames(), 512);
                let selected = device.output_options().dispatch();
                assert_eq!(selected.is_forced_portable(), dispatch.is_forced_portable());
                assert_eq!(
                    selected.convolution_kernel_kind(),
                    dispatch.convolution_kernel_kind()
                );
                assert_eq!(
                    selected.supersaw_kernel_kind(),
                    dispatch.supersaw_kernel_kind()
                );
                assert_eq!(
                    selected.wavetable_kernel_kind(),
                    dispatch.wavetable_kernel_kind()
                );
            }
        }

        #[test]
        fn explicit_silent_buffer_survives_recycle() {
            let preference = AudioBufferPreference::Frames(513);
            let options = LiveOutputOptions::default()
                .with_buffer_preference(preference)
                .with_dispatch(DspDispatch::portable());
            let mut device = LiveScalarDevice::start_silent_with_options(44_100, 7, options)
                .expect("silent output with explicit buffer");
            let mut previous_publication = 0;
            for (recycled, sample_rate) in [(false, 44_100), (true, SILENT_SAMPLE_RATE)] {
                if recycled {
                    device.recycle_output().expect("recycle silent output");
                }
                assert_eq!(device.buffer_preference(), preference);
                assert_eq!(device.output_options().buffer_preference(), preference);
                assert!(device.dispatch().is_forced_portable());
                assert_eq!(device.sample_rate(), sample_rate);
                assert_eq!(device.requested_buffer_frames(), 513);
                assert_eq!(device.reported_buffer_frames(), Some(513));
                let facts = device.audio_facts();
                assert_eq!(facts.output().buffer_preference(), preference);
                assert_eq!(facts.output().requested_buffer_frames(), 513);
                assert_eq!(facts.output().reported_buffer_frames(), Some(513));

                // Load telemetry publishes every 16 callbacks. Recycle publishes
                // an empty reset first; the changed rate also excludes an old
                // callback's late publication from proving replacement delivery.
                let deadline = Instant::now() + Duration::from_secs(3);
                let load = loop {
                    let load = device.report().realtime_load;
                    if load.publication > previous_publication
                        && load.sample_rate_hz == sample_rate
                        && load.total_callbacks >= REALTIME_LOAD_PUBLISH_INTERVAL
                    {
                        break load;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "no fresh callback telemetry after recycled={recycled}: {load:?}"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                };
                assert_eq!(load.last_callback_frames, 513);
                let period_nanos = 513 * 1_000_000_000 / u64::from(sample_rate);
                assert_eq!(load.last_callback_period_nanos, period_nanos);
                assert_eq!(load.total_period_nanos, load.total_callbacks * period_nanos);
                previous_publication = load.publication;
            }
        }

        #[test]
        fn rejected_output_discovery_preserves_queued_state() {
            let _tripwire_guard = tripwire_assertion_guard();
            let mut device = LiveScalarDevice::start_silent(44_100, 7).expect("silent output");
            // Quiesce the callback without dropping its owner so queue and telemetry
            // comparisons cannot race normal playback progress.
            device.silent.as_mut().expect("silent callback").stop();
            // SAFETY: `stop` joined the silent callback, the ring's only consumer.
            unsafe { device.shared.ring.release_consumer() };
            let close = Arc::clone(&device.silent.as_ref().unwrap().close);
            device.shared.frames.store(44_100, Ordering::Release);
            device.set_generation(7, 88_200, TakeoverCut::None);
            device.set_master_gain(0.5);
            device
                .shared
                .record
                .set_enabled(true)
                .expect("legacy recording");
            let event = AudioEvent {
                onset_id: 42,
                generation: 7,
                target_frame: 88_200,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 1.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            assert!(device.push(event));
            let report = device.report();
            let facts = device.audio_facts();
            let mut discoveries = 0;
            let error = device
                .recycle_output_to_with(
                    Some("unavailable-output"),
                    |selector| {
                        discoveries += 1;
                        assert_eq!(selector, Some("unavailable-output"));
                        Err(DevicePlaybackError::Unavailable(
                            "discovery rejected".into(),
                        ))
                    },
                    LiveScalarDevice::prepare_recycled_output,
                )
                .expect_err("discovery must fail");
            assert_eq!(error.to_string(), "discovery rejected");
            assert_eq!(discoveries, 1);
            assert!(!device.shared.stopped.load(Ordering::Acquire));
            assert!(Arc::ptr_eq(
                &device.silent.as_ref().expect("retained output").close,
                &close
            ));
            assert_eq!(device.audio_facts(), facts);
            assert_eq!(device.report(), report);
            assert_eq!(device.shared.takeover_frame.load(Ordering::Acquire), 88_200);
            assert!(device.shared.record.is_enabled());
            assert_eq!(
                device.shared.ring.pop().map(|event| event.onset_id),
                Some(42)
            );
            assert!(device.shared.ring.is_empty());
            device.check_health().expect("discovery preserves health");
        }

        #[test]
        fn rejected_output_discovery_keeps_the_live_clock_running() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            let callback = device
                .silent
                .as_ref()
                .unwrap()
                .join
                .as_ref()
                .unwrap()
                .thread()
                .id();
            device
                .recycle_output_to_with(
                    Some("unavailable-output"),
                    |_| {
                        Err(DevicePlaybackError::Unavailable(
                            "discovery rejected".into(),
                        ))
                    },
                    LiveScalarDevice::prepare_recycled_output,
                )
                .expect_err("discovery must fail");
            assert!(!device.shared.stopped.load(Ordering::Acquire));
            assert_eq!(
                device
                    .silent
                    .as_ref()
                    .expect("retained output")
                    .join
                    .as_ref()
                    .unwrap()
                    .thread()
                    .id(),
                callback
            );
            let before = device.clock_frames();
            let deadline = Instant::now() + Duration::from_secs(3);
            while device.clock_frames() <= before {
                assert!(
                    Instant::now() < deadline,
                    "retained output stopped advancing"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(device.generation(), 7);
            device.check_health().expect("discovery preserves health");
        }

        fn assert_recycled_sample_is_copied(
            device: &LiveScalarDevice,
            id: crate::SampleId,
            sample: &crate::DecodedSample,
        ) {
            device
                .set_recording_enabled(true)
                .expect("record copied PCM");
            let target = device.clock_frames() + u64::from(device.sample_rate()) / 4;
            let generation = device.generation() + 1;
            let channel = &device.shared.confirmations;
            let offer = confirmation_offer(channel, 1, generation, target, target + 128, 1);
            let mut event = confirmation_event(offer, 0, target);
            event.event.gain = 1.0;
            event.event.sample = Some(crate::SampleControls {
                sample: id,
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: crate::SampleHold::Hap,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            event.expected_sample_identity = Some(sample.identity());
            assert!(channel.publish(offer));
            device.set_generation(generation, target, TakeoverCut::None);
            assert!(device.push(event));

            let mut pcm = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            let terminal = loop {
                device.drain_recording(&mut pcm);
                if let Some(terminal) = channel.pop_terminal() {
                    break terminal;
                }
                assert!(
                    Instant::now() < deadline,
                    "seeded callback did not copy its window"
                );
                std::thread::sleep(Duration::from_millis(2));
            };
            device.drain_recording(&mut pcm);
            assert_eq!(terminal.key, offer.key);
            assert_eq!(
                terminal.outcome,
                crate::confirmation::WindowOutcome::Confirmed
            );
            assert!(pcm.iter().all(|value| value.is_finite()));
            assert!(
                pcm.iter().any(|value| value.abs() > 0.125),
                "new body must sound"
            );
            let queues = device.report().asset_queues;
            assert_eq!(queues.sample_installs, 0);
            assert_eq!(queues.sample_returns, 0);
            assert_eq!(queues.leaked, 0);
        }

        #[test]
        fn seeded_recycle_supersedes_pending_installs_and_uninstalls() {
            for pending_uninstall in [false, true] {
                let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
                // Keep the old handle but join its callback so both batons are
                // deterministically still pending when the real recycle begins.
                device.silent.as_mut().expect("old callback").stop();
                let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
                let id = crate::SampleId(9);
                let old = crate::DecodedSample::from_parts(48_000, 1, vec![0.125; 4096])
                    .expect("old sample");
                let samples = [(
                    id,
                    crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 4096])
                        .expect("new sample"),
                )];
                device.install_sample(id, old).expect("queued old body");
                if pending_uninstall {
                    assert!(device.uninstall_sample(id));
                }
                assert_eq!(
                    device.report().asset_queues.sample_installs,
                    if pending_uninstall { 2 } else { 1 }
                );

                device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &samples, || false)
                    .expect("seeded recycle");
                assert_eq!(Arc::strong_count(&old_close), 1, "old owner released");
                device.check_health().expect("replacement healthy");
                assert_recycled_sample_is_copied(&device, id, &samples[0].1);
            }
        }

        #[test]
        fn invalid_seeded_recycle_preserves_the_old_output_and_queued_batons() {
            let sample =
                crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 32]).expect("sample");
            for invalid in [
                vec![(
                    crate::SampleId(crate::sample::SAMPLE_BANK_CAPACITY as u32),
                    sample.clone(),
                )],
                vec![(crate::SampleId(9), sample.clone()); crate::sample::SAMPLE_BANK_CAPACITY + 1],
            ] {
                let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
                device.silent.as_mut().expect("old callback").stop();
                let close = Arc::clone(&device.silent.as_ref().unwrap().close);
                device
                    .install_sample(crate::SampleId(9), sample.clone())
                    .expect("install baton");
                assert!(device.uninstall_sample(crate::SampleId(10)));
                let report = device.report();
                let facts = device.audio_facts();
                let epoch = device.confirmations().epoch();

                let error = device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &invalid, || {
                        panic!("invalid seed reached recycle")
                    })
                    .expect_err("invalid snapshot must fail before teardown");
                assert!(matches!(error, DevicePlaybackError::ResourceLimit(_)));
                assert!(Arc::ptr_eq(
                    &close,
                    &device.silent.as_ref().expect("old output").close
                ));
                assert!(!device.shared.stopped.load(Ordering::Acquire));
                assert_eq!(device.audio_facts(), facts);
                assert_eq!(device.report(), report);
                assert_eq!(device.confirmations().epoch(), epoch);
                device.check_health().expect("old output unchanged");
                let install = device
                    .shared
                    .samples
                    .installs
                    .pop()
                    .expect("preserved install");
                assert_eq!(install.id, crate::SampleId(9));
                // SAFETY: the callback is joined; this pop transfers unique ownership.
                let queued = unsafe { Box::from_raw(install.sample) };
                assert_eq!(queued.identity(), sample.identity());
                let uninstall = device
                    .shared
                    .samples
                    .installs
                    .pop()
                    .expect("preserved uninstall");
                assert_eq!(uninstall.id, crate::SampleId(10));
                assert!(uninstall.sample.is_null());
                assert_eq!(device.shared.samples.installs.len(), 0);
            }
        }

        #[test]
        fn recycle_preparation_failure_marks_output_unhealthy_and_can_recover() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.check_health().expect("initial output is healthy");
            let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
            let samples = [(
                crate::SampleId(9),
                crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 4096]).expect("seed"),
            )];
            let identity = samples[0].1.identity();
            let pcm = samples[0].1.pcm().as_ptr();
            let mut refused_health = [false; 2];
            let mut preparations = 0;
            for refused in &mut refused_health {
                let error = device
                    .recycle_output_to_with(
                        None,
                        |_| panic!("silent output needs no discovery"),
                        |device, opened| {
                            preparations += 1;
                            assert!(opened.is_none());
                            assert!(device.stream.is_none() && device.silent.is_none());
                            assert!(device.shared.stopped.load(Ordering::Acquire));
                            assert!(old_close.load(Ordering::Acquire));
                            assert_eq!(Arc::strong_count(&old_close), 1, "old thread was joined");
                            let prepared =
                                device.prepare_recycled_output_with_samples(opened, &samples)?;
                            let OutputHandle::Silent(silent) = &prepared.handle else {
                                panic!("expected seeded candidate");
                            };
                            let close = Arc::clone(&silent.close);
                            drop(prepared);
                            assert!(close.load(Ordering::Acquire));
                            assert_eq!(Arc::strong_count(&close), 1, "failed candidate joined");
                            Err(DevicePlaybackError::Unavailable(
                                "preparation rejected".into(),
                            ))
                        },
                    )
                    .expect_err("preparation must fail");
                assert_eq!(error.to_string(), "preparation rejected");
                assert!(device.stream.is_none() && device.silent.is_none());
                assert!(device.shared.stopped.load(Ordering::Acquire));
                assert_eq!(samples[0].1.identity(), identity);
                assert_eq!(samples[0].1.pcm().as_ptr(), pcm);
                assert_eq!(samples[0].1.stereo_at(0.0), Some((0.75, 0.75)));
                *refused = device.check_health().is_err();
            }
            assert_eq!(preparations, 2);

            // The old thread is joined before this sole replacement is prepared.
            device
                .recycle_output_to_with_samples(None, &samples, || false)
                .expect("successful seeded silent retry");
            assert!(device.stream.is_none() && device.silent.is_some());
            assert!(!device.shared.stopped.load(Ordering::Acquire));
            device
                .check_health()
                .expect("successful retry clears failure");
            let before = device.clock_frames();
            let deadline = Instant::now() + Duration::from_secs(3);
            while device.clock_frames() <= before {
                assert!(
                    Instant::now() < deadline,
                    "replacement output stopped advancing"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(device.generation(), 7);
            device.check_health().expect("replacement remains healthy");
            assert_recycled_sample_is_copied(&device, samples[0].0, &samples[0].1);
            assert_eq!(
                refused_health, [true; 2],
                "handleless retries must fail health"
            );
        }

        #[test]
        fn recycle_activation_failure_marks_output_unhealthy() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.check_health().expect("initial output is healthy");
            let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
            let mut candidate_close = None;
            let mut preparations = 0;
            let error = device
                .recycle_output_to_with(
                    None,
                    |_| panic!("silent output needs no discovery"),
                    |device, opened| {
                        preparations += 1;
                        assert!(opened.is_none());
                        assert!(device.stream.is_none() && device.silent.is_none());
                        assert!(old_close.load(Ordering::Acquire));
                        assert_eq!(Arc::strong_count(&old_close), 1, "old thread was joined");
                        let prepared = device.prepare_recycled_output(opened)?;
                        let OutputHandle::Silent(silent) = &prepared.handle else {
                            panic!("expected pending silent output");
                        };
                        candidate_close = Some(Arc::clone(&silent.close));
                        assert_eq!(prepared.gate.0.load(Ordering::Acquire), CALLBACK_PENDING);
                        prepared
                            .gate
                            .report_error(&device.shared.failed, &device.shared.callback_errors);
                        assert_eq!(prepared.gate.0.load(Ordering::Acquire), CALLBACK_FAILED);
                        Ok(prepared)
                    },
                )
                .expect_err("failed pending gate must reject activation");
            assert_eq!(preparations, 1);
            assert_eq!(
                error.to_string(),
                "live output failed before activation or was already activated"
            );
            assert!(device.stream.is_none() && device.silent.is_none());
            assert!(device.shared.stopped.load(Ordering::Acquire));
            let candidate_close = candidate_close.expect("candidate was prepared");
            assert!(candidate_close.load(Ordering::Acquire));
            assert_eq!(
                Arc::strong_count(&candidate_close),
                1,
                "candidate thread was joined"
            );
            assert_eq!(device.shared.callback_errors.load(Ordering::Acquire), 0);
            assert!(
                device.check_health().is_err(),
                "handleless activation must fail health"
            );
        }

        #[test]
        fn silent_recycle_does_not_discover_a_native_output() {
            let mut device = LiveScalarDevice::start_silent(44_100, 7).expect("silent output");
            for selector in [None, Some(SILENT_OUTPUT_NAME)] {
                device
                    .recycle_output_to_with(
                        selector,
                        |_| panic!("silent output needs no discovery"),
                        LiveScalarDevice::prepare_recycled_output,
                    )
                    .expect("silent recycle");
                assert_eq!(device.name(), SILENT_OUTPUT_NAME);
                assert_eq!(device.sample_rate(), SILENT_SAMPLE_RATE);
                assert_eq!(device.generation(), 7);
            }
        }

        #[test]
        fn cancelled_recycle_never_activates_the_pending_output() {
            use std::cell::Cell;
            let samples = [(
                crate::SampleId(9),
                crate::DecodedSample::from_parts(48_000, 1, vec![0.75; 4096]).expect("seed"),
            )];
            let identity = samples[0].1.identity();
            let pcm = samples[0].1.pcm().as_ptr();
            for cancel_at in 1..=5 {
                let mut device = LiveScalarDevice::start_silent(44_100, 7).expect("silent output");
                let old_close = Arc::clone(&device.silent.as_ref().unwrap().close);
                let facts = device.audio_facts();
                let checks = Cell::new(0);
                let mut candidate_close = None;
                let error = device
                    .recycle_output_to_with_cancel(
                        Some(SILENT_OUTPUT_NAME),
                        |_| panic!("silent output needs no discovery"),
                        |device, opened| {
                            let prepared =
                                device.prepare_recycled_output_with_samples(opened, &samples)?;
                            let OutputHandle::Silent(silent) = &prepared.handle else {
                                panic!("expected pending silent output");
                            };
                            candidate_close = Some(Arc::clone(&silent.close));
                            assert_eq!(prepared.gate.0.load(Ordering::Acquire), CALLBACK_PENDING);
                            Ok(prepared)
                        },
                        || {
                            checks.set(checks.get() + 1);
                            checks.get() == cancel_at
                        },
                    )
                    .expect_err("cancel output replacement");
                assert!(matches!(error, DevicePlaybackError::Cancelled));
                assert_eq!(checks.get(), cancel_at);
                assert_eq!(samples[0].1.identity(), identity);
                assert_eq!(samples[0].1.pcm().as_ptr(), pcm);
                assert_eq!(samples[0].1.stereo_at(0.0), Some((0.75, 0.75)));
                assert_eq!(device.audio_facts(), facts, "no candidate facts committed");
                if cancel_at <= 2 {
                    assert!(device.has_output());
                    assert!(!old_close.load(Ordering::Acquire));
                    device.check_health().expect("old output untouched");
                    assert!(candidate_close.is_none());
                } else {
                    assert!(!device.has_output());
                    assert!(old_close.load(Ordering::Acquire));
                    assert_eq!(Arc::strong_count(&old_close), 1, "old callback joined");
                    assert!(device.check_health().is_err());
                    if cancel_at == 3 {
                        assert!(
                            candidate_close.is_none(),
                            "Stop after teardown skips preparation"
                        );
                    } else {
                        let close = candidate_close.expect("candidate prepared");
                        assert!(close.load(Ordering::Acquire));
                        assert_eq!(Arc::strong_count(&close), 1, "pending callback joined");
                    }
                }
            }
        }

        #[test]
        fn portable_selection_survives_silent_recycle_and_asset_preparation() {
            let options = LiveOutputOptions::default().with_dispatch(DspDispatch::portable());
            let mut device = LiveScalarDevice::start_silent_with_options(48_000, 7, options)
                .expect("portable silent output");
            device.recycle_output().expect("recycle silent output");
            assert!(device.dispatch().is_forced_portable());
            assert!(device.output_options().dispatch().is_forced_portable());

            // Joining the sole callback leaves the test as the only asset-ring
            // consumer, so it can inspect the prepared installs without a race.
            device.silent.take().expect("silent callback").stop();
            let params = crate::reverb::ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            device.ensure_reverb(0, params);
            let install = device
                .shared
                .samples
                .reverb_installs
                .pop()
                .expect("prepared orbit reverb");
            // SAFETY: the callback is joined and popping transferred the unique
            // allocation to this thread. No other owner can read or free it.
            let reverb = unsafe { Box::from_raw(install.reverb) };
            assert!(reverb.dispatch().is_forced_portable());
            assert_eq!(
                reverb.dispatch().convolution_kernel_kind(),
                crate::ConvolutionKernelKind::Portable,
            );

            device.ensure_fx_reverb(params);
            let install = device
                .shared
                .samples
                .fx_reverb_installs
                .pop()
                .expect("prepared stage reverb");
            // SAFETY: as above, this thread now exclusively owns the popped box.
            let reverb = unsafe { Box::from_raw(install.reverb) };
            assert!(reverb.dispatch().is_forced_portable());
            assert_eq!(
                reverb.dispatch().convolution_kernel_kind(),
                crate::ConvolutionKernelKind::Portable,
            );
        }

        #[test]
        fn frame_rebase_preserves_time_in_both_rate_directions() {
            assert_eq!(rebase_frame_rate(48_000, 48_000, 44_100), 44_100);
            assert_eq!(rebase_frame_rate(24_000, 48_000, 44_100), 22_050);
            assert_eq!(rebase_frame_rate(44_100, 44_100, 48_000), 48_000);
            assert_eq!(rebase_frame_rate(22_050, 44_100, 48_000), 24_000);

            for (frame, old_rate, new_rate) in [
                (1, 48_000, 44_100),
                (1, 44_100, 48_000),
                (98_765, 96_000, 44_100),
                (98_765, 44_100, 96_000),
            ] {
                let rebased = rebase_frame_rate(frame, old_rate, new_rate);
                assert!(
                    u128::from(rebased) * u128::from(old_rate)
                        >= u128::from(frame) * u128::from(new_rate),
                    "recycle moved the clock backwards: {frame}@{old_rate} -> {rebased}@{new_rate}"
                );
                assert!(
                    u128::from(rebased.saturating_sub(1)) * u128::from(old_rate)
                        < u128::from(frame) * u128::from(new_rate),
                    "recycle rounded forward by more than one new-rate frame"
                );
            }
        }

        #[test]
        fn recycled_stream_rebases_takeover_and_resets_stream_local_clock_state() {
            let shared = LiveShared::new(7);
            shared.frames.store(48_000, Ordering::Release);
            shared
                .rendering_until_frame
                .store(49_000, Ordering::Release);
            shared.takeover_frame.store(24_000, Ordering::Release);
            shared.playback_origin_nanos.store(123, Ordering::Release);
            shared.buffer_playback_nanos.store(456, Ordering::Release);
            shared.playback_latency_nanos.store(789, Ordering::Release);
            shared.last_callback_nanos.store(999, Ordering::Release);

            shared.prepare_recycled_stream(48_000, 44_100);
            assert_eq!(shared.clock_frames(), 44_100);
            assert_eq!(shared.render_frontier_frames(), 44_100);
            assert_eq!(shared.takeover_frame.load(Ordering::Acquire), 22_050);
            assert_eq!(
                shared.playback_origin_nanos.load(Ordering::Acquire),
                UNSET_STREAM_INSTANT_NANOS
            );
            assert_eq!(shared.buffer_playback_nanos.load(Ordering::Acquire), 0);
            assert_eq!(shared.playback_latency_nanos.load(Ordering::Acquire), 0);
            assert_eq!(shared.last_callback_nanos.load(Ordering::Acquire), 0);

            shared.prepare_recycled_stream(44_100, 48_000);
            assert_eq!(shared.clock_frames(), 48_000);
            assert_eq!(shared.render_frontier_frames(), 48_000);
            assert_eq!(shared.takeover_frame.load(Ordering::Acquire), 24_000);
            assert_eq!(shared.generation.load(Ordering::Acquire), 7);
        }

        /// A countdown can span an output change: a quantised launch arms its
        /// line, then the output is recycled onto a device at another rate
        /// before the line. The armed frame moves with the clock, its flag bits
        /// kept; an empty word and the withdrawal marker stay as they are.
        #[test]
        fn recycled_stream_rebases_an_armed_line_and_keeps_its_flags() {
            let shared = LiveShared::new(7);
            for drop_bits in [0b01u64, 0b11] {
                shared
                    .line_arm
                    .store((44_100 << 2) | drop_bits, Ordering::Release);
                shared.prepare_recycled_stream(44_100, 48_000);
                let word = shared.line_arm.load(Ordering::Acquire);
                assert_eq!(word >> 2, 48_000, "the line is one second in either domain");
                assert_eq!(word & 0b11, drop_bits);
            }
            for unarmed in [0, crate::LINE_ARM_WITHDRAWN] {
                shared.line_arm.store(unarmed, Ordering::Release);
                shared.prepare_recycled_stream(48_000, 44_100);
                assert_eq!(shared.line_arm.load(Ordering::Acquire), unarmed);
            }
        }

        #[test]
        fn recycled_stream_discards_only_rate_specific_pending_reverbs() {
            use crate::assets::{
                FxReverbInstall, ReturnedFxReverb, ReturnedReverb, ReturnedSample,
            };
            use crate::assets::{ReverbInstall, SampleInstall};
            use crate::reverb::{OrbitReverb, ReverbParams};
            use crate::sample::{DecodedSample, SampleId};

            fn enqueue<T>(value: T, send: impl FnOnce(*mut T) -> bool) -> *mut T {
                let pointer = Box::into_raw(Box::new(value));
                if !send(pointer) {
                    // SAFETY: a refused enqueue leaves this allocation here.
                    drop(unsafe { Box::from_raw(pointer) });
                    panic!("fresh asset queue refused an entry");
                }
                pointer
            }

            let params = ReverbParams {
                size_secs: 0.025,
                fade_secs: 0.01,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            };
            for new_rate in [48_000, 44_100] {
                // No callback owns these queues: preparation runs after the old
                // stream joins and before the replacement gate opens.
                let shared = LiveShared::new(7);
                let channel = &shared.samples;
                let orbit = || OrbitReverb::generate(48_000, params);
                let stage = || OrbitReverb::generate_streaming(48_000, params);
                let pending_orbits: [_; 2] = std::array::from_fn(|index| {
                    enqueue(orbit(), |reverb| {
                        channel
                            .reverb_installs
                            .push(ReverbInstall {
                                orbit: index as u8,
                                reverb,
                            })
                            .is_ok()
                    })
                });
                let pending_stages: [_; 2] = std::array::from_fn(|_| {
                    enqueue(stage(), |reverb| {
                        channel
                            .fx_reverb_installs
                            .push(FxReverbInstall { reverb })
                            .is_ok()
                    })
                });
                let decoded = DecodedSample::from_parts(96_000, 1, vec![0.25, 0.5]).unwrap();
                let sample = enqueue(decoded.clone(), |sample| {
                    channel
                        .installs
                        .push(SampleInstall {
                            id: SampleId(8),
                            sample,
                        })
                        .is_ok()
                });
                let returned_sample = enqueue(decoded.clone(), |sample| {
                    channel.returns.push(ReturnedSample(sample)).is_ok()
                });
                let returned_orbit = enqueue(orbit(), |reverb| {
                    channel.reverb_returns.push(ReturnedReverb(reverb)).is_ok()
                });
                let returned_stage = enqueue(stage(), |reverb| {
                    channel
                        .fx_reverb_returns
                        .push(ReturnedFxReverb(reverb))
                        .is_ok()
                });
                assert_eq!(channel.reverb_installs.len(), 2);
                assert_eq!(channel.fx_reverb_installs.len(), 2);

                shared.prepare_recycled_stream(48_000, new_rate);

                // Decoded PCM carries its own rate; returns already belong to the
                // producer and must not be consumed by stream preparation.
                assert_eq!(channel.installs.len(), 1);
                assert_eq!(channel.returns.len(), 1);
                assert_eq!(channel.reverb_returns.len(), 1);
                assert_eq!(channel.fx_reverb_returns.len(), 1);
                let installed = channel.installs.pop().unwrap();
                // SAFETY: the pop transfers the queue's unique allocation here.
                let installed_pcm = unsafe { Box::from_raw(installed.sample) };
                assert_eq!(installed.id, SampleId(8));
                assert_eq!(installed.sample, sample);
                assert_eq!(*installed_pcm, decoded);
                let returned = channel.returns.pop().unwrap();
                // SAFETY: this returned allocation has been popped exactly once.
                let returned_pcm = unsafe { Box::from_raw(returned.0) };
                assert_eq!(returned.0, returned_sample);
                assert_eq!(*returned_pcm, decoded);
                let returned = channel.reverb_returns.pop().unwrap();
                // SAFETY: this is the sole owner after popping the return.
                let orbit_return = unsafe { Box::from_raw(returned.0) };
                assert_eq!(returned.0, returned_orbit);
                let returned = channel.fx_reverb_returns.pop().unwrap();
                // SAFETY: this is a distinct, once-popped stage allocation.
                let stage_return = unsafe { Box::from_raw(returned.0) };
                assert_eq!(returned.0, returned_stage);
                drop((orbit_return, stage_return));

                if new_rate == 48_000 {
                    for (index, expected) in pending_orbits.into_iter().enumerate() {
                        let install = channel.reverb_installs.pop().unwrap();
                        // SAFETY: same-rate preparation left the queued owner.
                        let reverb = unsafe { Box::from_raw(install.reverb) };
                        assert_eq!(install.orbit, index as u8);
                        assert_eq!(install.reverb, expected);
                        drop(reverb);
                    }
                    for expected in pending_stages {
                        let install = channel.fx_reverb_installs.pop().unwrap();
                        // SAFETY: as above, the pop transfers unique ownership.
                        let reverb = unsafe { Box::from_raw(install.reverb) };
                        assert_eq!(install.reverb, expected);
                        drop(reverb);
                    }
                }
                assert_eq!(channel.reverb_installs.len(), 0);
                assert_eq!(channel.fx_reverb_installs.len(), 0);
                assert_eq!(channel.leaked.load(Ordering::Relaxed), 0);
                assert_eq!(shared.generation.load(Ordering::Acquire), 7);
            }
        }
    }
    mod output_selection {
        use super::*;

        #[test]
        fn invalid_buffer_is_rejected_before_output_discovery() {
            let invalid = AudioBufferPreference::Frames(0);
            let options = LiveOutputOptions::default().with_buffer_preference(invalid);
            let expected = invalid
                .validate()
                .expect_err("invalid preference")
                .to_string();
            let error = LiveScalarDevice::start_output_with_options(
                Some("missing-output-for-buffer-validation"),
                1,
                options,
            )
            .err()
            .expect("invalid preference must not open output");
            assert_eq!(error.to_string(), expected);
            let error = LiveScalarDevice::start_silent_with_options(48_000, 1, options)
                .err()
                .expect("invalid preference must not prepare silent output");
            assert_eq!(error.to_string(), expected);
        }

        #[test]
        fn live_environment_controls_use_the_product_namespace() {
            assert_eq!(MAX_LIVE_LATENCY_ENV, "RUSTEL_MAX_LIVE_LATENCY_MS");
            assert_eq!(LIVE_BUFFER_FRAMES_ENV, "RUSTEL_LIVE_BUFFER_FRAMES");
        }

        #[test]
        fn live_buffer_request_stays_inside_the_advertised_device_range() {
            // The platform default is environment-dependent: WSL's forwarded
            // sink underruns at 256 frames, so the selector prefers 2048 there.
            let platform_default = if std::env::var_os("WSL_DISTRO_NAME").is_some()
                || std::env::var_os("WSL_INTEROP").is_some()
            {
                2048
            } else {
                TARGET_LIVE_BUFFER_FRAMES
            };
            let env_override = std::env::var(LIVE_BUFFER_FRAMES_ENV)
                .ok()
                .and_then(|frames| frames.parse::<u32>().ok())
                .filter(|frames| {
                    (AudioBufferPreference::MIN_FRAMES..=AudioBufferPreference::MAX_FRAMES)
                        .contains(frames)
                });
            let target = env_override.unwrap_or(platform_default);
            let select = |range: &SupportedBufferSize| {
                select_live_buffer_frames(range, AudioBufferPreference::Auto)
                    .expect("supported buffer range")
            };
            assert_eq!(select(&SupportedBufferSize::Unknown), target);
            assert_eq!(
                select(&SupportedBufferSize::Range { min: 64, max: 1024 }),
                target.clamp(64, 1024)
            );
            assert_eq!(
                select(&SupportedBufferSize::Range {
                    min: 512,
                    max: 2048
                }),
                target.clamp(512, 2048)
            );
            assert_eq!(
                select(&SupportedBufferSize::Range { min: 32, max: 128 }),
                target.clamp(32, 128)
            );
        }

        #[test]
        fn explicit_buffer_selection_ignores_automatic_defaults() {
            assert_eq!(
                select_live_buffer_frames(
                    &SupportedBufferSize::Unknown,
                    AudioBufferPreference::Frames(96)
                )
                .expect("explicit buffer"),
                96
            );
            assert_eq!(
                select_live_buffer_frames(
                    &SupportedBufferSize::Range {
                        min: 256,
                        max: 1024
                    },
                    AudioBufferPreference::Frames(96),
                )
                .expect("clamped buffer"),
                256
            );
        }

        /// WASAPI reports its shared period as `min == max`. Treat it as a minimum:
        /// larger requests pass through and smaller requests use the period.
        /// Other hosts keep fixed sizes and real ranges as hard constraints.
        /// This test needs no device and runs on every platform.
        #[test]
        fn a_fixed_shared_mode_period_is_a_floor_not_a_ceiling() {
            use AudioBufferPreference::Frames;
            let period = SupportedBufferSize::Range { min: 480, max: 480 };
            let floor = live_buffer_range(&period, true);
            assert_eq!(Frames(2_048).resolve(256, floor), Ok(2_048));
            assert_eq!(Frames(16_384).resolve(256, floor), Ok(16_384));
            assert_eq!(Frames(32).resolve(256, floor), Ok(480), "the period floor");
            assert_eq!(AudioBufferPreference::Auto.resolve(128, floor), Ok(480));
            let hard = live_buffer_range(&period, false);
            assert_eq!(Frames(2_048).resolve(256, hard), Ok(480));
            for fixed_period_is_floor in [false, true] {
                let ranged = SupportedBufferSize::Range {
                    min: 64,
                    max: 1_024,
                };
                assert_eq!(
                    Frames(2_048).resolve(256, live_buffer_range(&ranged, fixed_period_is_floor)),
                    Ok(1_024)
                );
                assert_eq!(
                    live_buffer_range(&SupportedBufferSize::Unknown, fixed_period_is_floor),
                    AudioBufferRange::Unknown
                );
            }
            assert_eq!(fixed_device_period(&period), Some(480));
            assert_eq!(
                fixed_device_period(&SupportedBufferSize::Range {
                    min: 64,
                    max: 1_024
                }),
                None
            );
            // The live selector applies the floor on Windows, where WASAPI is
            // the host, and keeps the hard constraint elsewhere.
            assert_eq!(
                select_live_buffer_frames(&period, Frames(2_048)).expect("fixed period"),
                if cfg!(windows) { 2_048 } else { 480 }
            );
        }

        #[test]
        fn listed_device_match_prefers_id_then_exact_friendly_then_disambiguated() {
            let candidates = [
                ("id-a".to_owned(), "Speakers".to_owned(), 0u8),
                ("id-b".to_owned(), "Speakers".to_owned(), 1u8),
                ("id-c".to_owned(), "Microphone".to_owned(), 2u8),
                ("id-d".to_owned(), "Mic".to_owned(), 3u8),
            ];
            assert_eq!(match_listed_device(&candidates, "id-b"), Some(1));
            assert_eq!(match_listed_device(&candidates, "ID-B"), Some(1));
            assert_eq!(match_listed_device(&candidates, "Speakers"), Some(0));
            assert_eq!(match_listed_device(&candidates, "Speakers (id-b)"), Some(1));
            assert_eq!(match_listed_device(&candidates, "microphone"), Some(2));
            // Case-insensitive exact "mic" must beat the earlier substring hit
            // inside "Microphone".
            assert_eq!(match_listed_device(&candidates, "mic"), Some(3));
            assert_eq!(match_listed_device(&candidates, "MIC"), Some(3));
            assert_eq!(match_listed_device(&candidates, "phone"), Some(2));
            assert_eq!(match_listed_device(&candidates, "missing"), None);
        }
    }
    mod record_capture {
        use super::*;

        #[test]
        fn the_record_tap_hands_over_whole_blocks_and_drops_rather_than_waits() {
            let tap = RecordTap::new();
            tap.write_stereo(&[0.5, -0.5]);
            let mut out = Vec::new();
            assert_eq!(tap.drain(&mut out), 0, "disabled: nothing queued");
            tap.set_enabled(true).expect("legacy recording");
            tap.write_stereo(&[0.25, -0.25, 1.0, -1.0]);
            assert_eq!(tap.drain(&mut out), 4);
            assert_eq!(out, [0.25, -0.25, 1.0, -1.0]);
            // Fill it to the brim, then one more block is dropped whole.
            let block = vec![0.1f32; 4096];
            let mut written = 0;
            while written + block.len() <= RECORD_RING_CAPACITY {
                tap.write_stereo(&block);
                written += block.len();
            }
            tap.write_stereo(&block);
            assert_eq!(tap.dropped_frames(), (block.len() / 2) as u64);
            out.clear();
            assert_eq!(tap.drain(&mut out), written);
            // Re-enabling starts from now and forgets the count.
            tap.set_enabled(false).expect("close legacy recording");
            tap.write_stereo(&block);
            tap.set_enabled(true).expect("restart legacy recording");
            assert_eq!(tap.dropped_frames(), 0);
            out.clear();
            assert_eq!(tap.drain(&mut out), 0);
        }

        #[test]
        fn record_capture_close_waits_for_admitted_pcm() {
            let tap = Arc::new(RecordTap::new());
            let mut capture = tap.capture().expect("capture");
            let admission = tap.admit(tap.state.load(Ordering::Relaxed)).unwrap();
            capture.request_close().unwrap();
            assert!(!tap.is_enabled());
            assert!(!capture.is_closed(), "admitted PCM has not published yet");
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            admission.tap.write_admitted(&[0.25, -0.5]);
            drop(admission);
            assert!(capture.is_closed());
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(2));
            assert_eq!(samples, [0.25, -0.5]);
            assert_eq!(capture.drain(&mut samples), Ok(0));
            assert_eq!(capture.dropped_frames(), Ok(0));
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            drop(capture);

            // Dropping the reader also cannot reset an admitted writer's epoch.
            let capture = tap.capture().unwrap();
            let admission = tap.admit(tap.state.load(Ordering::Relaxed)).unwrap();
            drop(capture);
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            drop(admission);
            let mut next = tap.capture().unwrap();
            assert_eq!(next.drain(&mut samples), Ok(0));
        }

        #[test]
        fn record_capture_close_waits_for_admitted_loss() {
            let tap = Arc::new(RecordTap::new());
            let mut capture = tap.capture().unwrap();
            let full = vec![0.25; RECORD_RING_CAPACITY];
            tap.write_stereo(&full);
            let admission = tap.admit(tap.state.load(Ordering::Relaxed)).unwrap();
            capture.request_close().unwrap();
            assert!(!capture.is_closed(), "admitted loss has not published yet");
            admission.tap.write_admitted(&[0.5, -0.5]);
            drop(admission);
            assert!(capture.is_closed());
            assert_eq!(capture.dropped_frames(), Ok(1));
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(RECORD_RING_CAPACITY));
            assert_eq!(samples, full);
            tap.write_stereo(&[0.75, -0.75]);
            assert_eq!(capture.drain(&mut samples), Ok(0));
            assert_eq!(capture.dropped_frames(), Ok(1));
        }

        #[test]
        fn record_capture_rejects_stale_admission_and_handle_operations() {
            let tap = Arc::new(RecordTap::new());
            let capture = tap.capture().unwrap();
            let observed = tap.state.load(Ordering::Relaxed);
            let epoch = capture.epoch;
            capture.request_close().unwrap();
            assert!(capture.is_closed());
            drop(capture);
            let mut next = tap.capture().unwrap();
            assert_ne!(next.epoch, epoch);
            assert!(tap.admit(observed).is_none());
            tap.write_stereo(&[0.75, -0.75]);

            let mut stale = RecordCapture {
                tap: Arc::clone(&tap),
                epoch,
            };
            let mut samples = Vec::new();
            assert_eq!(stale.request_close(), Err(RecordCaptureError::StaleCapture));
            assert!(!stale.is_closed());
            assert_eq!(
                stale.drain(&mut samples),
                Err(RecordCaptureError::StaleCapture)
            );
            assert_eq!(
                stale.dropped_frames(),
                Err(RecordCaptureError::StaleCapture)
            );
            assert!(samples.is_empty());
            drop(stale);
            assert!(tap.is_enabled());
            assert_eq!(next.drain(&mut samples), Ok(2));
            assert_eq!(samples, [0.75, -0.75]);
        }

        #[test]
        fn record_capture_rejects_busy_and_exhausted_epochs_without_resetting_pcm() {
            let tap = Arc::new(RecordTap::new());
            tap.set_enabled(true).unwrap();
            tap.write_stereo(&[0.25, -0.25]);
            let legacy_epoch = tap.state.load(Ordering::Acquire);
            tap.set_enabled(true).unwrap();
            assert_eq!(tap.state.load(Ordering::Acquire), legacy_epoch);
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            let mut samples = Vec::new();
            assert_eq!(tap.drain(&mut samples), 2);
            assert_eq!(samples, [0.25, -0.25]);
            tap.write_stereo(&[0.5, -0.5]);
            tap.set_enabled(false).unwrap();
            tap.state
                .store(MAX_RECORD_EPOCH << RECORD_EPOCH_SHIFT, Ordering::Release);
            let read = tap.read.load(Ordering::Acquire);
            let written = tap.written.load(Ordering::Acquire);
            assert_eq!(
                tap.capture().unwrap_err(),
                RecordCaptureError::EpochExhausted
            );
            assert_eq!(
                tap.set_enabled(true),
                Err(RecordCaptureError::EpochExhausted)
            );
            assert_eq!(tap.read.load(Ordering::Acquire), read);
            assert_eq!(tap.written.load(Ordering::Acquire), written);
            samples.clear();
            assert_eq!(tap.drain(&mut samples), 2);
            assert_eq!(samples, [0.5, -0.5]);
        }

        #[test]
        fn record_capture_legacy_controls_cannot_steal_claimed_pcm() {
            let tap = Arc::new(RecordTap::new());
            let mut capture = tap.capture().unwrap();
            tap.write_stereo(&[0.25, -0.25]);
            assert_eq!(tap.set_enabled(true), Err(RecordCaptureError::Busy));
            assert_eq!(tap.set_enabled(false), Err(RecordCaptureError::Busy));
            let mut samples = Vec::new();
            assert_eq!(tap.drain(&mut samples), 0);
            assert!(samples.is_empty());
            capture.request_close().unwrap();
            assert!(capture.is_closed());
            assert_eq!(tap.capture().unwrap_err(), RecordCaptureError::Busy);
            assert_eq!(capture.drain(&mut samples), Ok(2));
            assert_eq!(samples, [0.25, -0.25]);
        }

        #[test]
        fn record_capture_rate_change_closes_without_resetting_tail() {
            let shared = LiveShared::new(7);
            let mut capture = shared.record.capture().unwrap();
            shared.record.write_stereo(&[0.25, -0.25]);
            shared.prepare_recycled_stream(48_000, 48_000);
            assert!(!capture.is_closed());
            shared.record.write_stereo(&[0.5, -0.5]);
            shared.prepare_recycled_stream(48_000, 44_100);
            assert!(capture.is_closed());
            shared.record.write_stereo(&[0.75, -0.75]);
            drop(shared);
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(4));
            assert_eq!(samples, [0.25, -0.25, 0.5, -0.5]);
            assert_eq!(capture.dropped_frames(), Ok(0));
            assert_eq!(capture.drain(&mut samples), Ok(0));
        }

        #[test]
        fn record_capture_callback_accept_drop_and_closed_paths_do_not_allocate() {
            let _tripwire_guard = tripwire_assertion_guard();
            assert!(tripwire::allocator_is_armed());
            let shared = LiveShared::new(7);
            let mut capture = shared.record.capture().unwrap();
            let mut backend = LiveScalarBackend::new(48_000, 4).unwrap();
            let mut meters = test_meter();
            let mut output = [0.0f32; 256];
            let owners = Arc::strong_count(&shared.record);
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            let rest = vec![0.0; RECORD_RING_CAPACITY - output.len()];
            shared.record.write_stereo(&rest);
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_eq!(capture.dropped_frames(), Ok(128));
            capture.request_close().unwrap();
            assert!(capture.is_closed());
            let before = tripwire::Violations::capture();
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
            });
            assert!(tripwire::Violations::capture().since(before).clean());
            assert_eq!(Arc::strong_count(&shared.record), owners);
            assert_eq!(shared.report(1).callback_scope_misses, 0);
            assert_eq!(shared.clock_frames(), 384);
            let mut samples = Vec::new();
            assert_eq!(capture.drain(&mut samples), Ok(RECORD_RING_CAPACITY));
            assert!(samples.iter().all(|sample| *sample == 0.0));
            assert_eq!(capture.dropped_frames(), Ok(128));
        }
    }
    mod reload_and_clock {
        use super::*;

        /// The reload takeover contract: a save's new generation re-queries
        /// from the published takeover frame, so old-generation events BEFORE it
        /// (the already-scheduled horizon) still play, and only events at/after
        /// it are filtered. Dropping the earlier ones cuts the music by one
        /// continuity margin on every save.
        /// A reload lets voices ring; an audition swap must not.
        ///
        /// Previewing a snippet installs a source the way a save does, and a
        /// save deliberately leaves sounding voices alone - cutting them would
        /// click on every edit. For an audition that is the bug: a four-second
        /// break played on under the snippet that replaced it. `cut_sounding`
        /// is the verb for that case, and this is what it buys.
        #[test]
        fn cut_sounding_silences_what_a_reload_would_have_let_ring() {
            let long_note = AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 4.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            let render = |cut_at: Option<u64>| -> f32 {
                let shared = LiveShared::new(1);
                assert!(shared.ring.push(long_note));
                let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
                let mut output = [0.0f32; 128 * 2];
                let mut start = 0u64;
                let mut tail = 0.0f32;
                while start < 24_000 {
                    if Some(start) == cut_at {
                        backend.cut_sounding_voices(start);
                    }
                    output.fill(0.0);
                    backend.process_block_with(
                        &mut output,
                        128,
                        start,
                        &shared.ring,
                        shared.flip_atomics(),
                        &shared.stopped,
                    );
                    // A tenth of a second past the cut, well beyond its ramp.
                    if start >= 16_800 {
                        tail = tail.max(
                            output
                                .iter()
                                .fold(0.0f32, |peak, sample| peak.max(sample.abs())),
                        );
                    }
                    start += 128;
                }
                tail
            };
            let ringing = render(None);
            let cut = render(Some(12_032));
            assert!(ringing > 1e-3, "the note never sounded: {ringing}");
            assert!(
                cut < 1e-5,
                "the note was still sounding a tenth of a second after the cut: {cut}"
            );
        }

        #[test]
        fn live_reload_preserves_an_already_active_sbd_graph() {
            let render = |reload: bool, resend: bool| {
                let shared = LiveShared::new(1);
                let hit = |onset_id: u64, generation: u64| AudioEvent {
                    onset_id,
                    generation,
                    target_frame: 4_800,
                    onset_lead: 0.0,
                    freq_hz: 55.0,
                    gain: 0.5,
                    duration_secs: 0.3,
                    ui_visuals: 0,
                    controls: Default::default(),
                    sample: None,
                    synth: Some(crate::SynthSource::Sbd {
                        decay_secs: 0.2,
                        pdecay_secs: 0.3,
                        penv_semitones: 36.0,
                        stop_secs: 0.21,
                    }),
                    wavetable: None,
                    cut: None,
                };
                assert!(shared.ring.push(hit(1, 1)));
                let mut backend =
                    LiveScalarBackend::with_dispatch(48_000, 4, DspDispatch::portable())
                        .expect("live scalar");
                let mut meters = test_meter();
                let mut pcm = vec![0.0f32; 12_288 * 2];
                for (index, output) in pcm.as_chunks_mut::<{ 128 * 2 }>().0.iter_mut().enumerate() {
                    if reload && index == 1 {
                        // Existing low-latency edits can take over in 80 ms,
                        // inside SBD's 100 ms graph lead. An already activated
                        // graph, like an already sounding ordinary voice, is
                        // not pending work for retire_pending_from to discard.
                        shared.takeover_frame.store(128 + 3_840, Ordering::Release);
                        shared.generation.store(2, Ordering::Release);
                        // The new generation queries from the takeover and
                        // sends the same onset again. The copy is dropped.
                        if resend {
                            assert!(shared.ring.push(hit(2, 2)));
                        }
                    }
                    tripwire::audio_scope(|| {
                        write_live_output(&mut backend, &mut meters, output, 2, &shared)
                    });
                }
                assert_eq!(shared.refused_voices.load(Ordering::Acquire), 0);
                assert_eq!(shared.late_events.load(Ordering::Acquire), 0);
                assert!(pcm[4_800 * 2..].iter().any(|sample| sample.abs() > 1e-3));
                pcm
            };
            assert_eq!(render(false, false), render(true, false));
            assert_eq!(
                render(false, false),
                render(true, true),
                "the onset sounds once"
            );
        }

        #[test]
        fn reload_keeps_old_generation_events_before_the_takeover_frame() {
            let event = |onset_id: u64, generation: u64, target_frame: u64| AudioEvent {
                onset_id,
                generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            let shared = LiveShared::new(1);
            // Old-generation horizon: one onset soon, one past the takeover.
            assert!(shared.ring.push(event(1, 1, 1_000)));
            assert!(shared.ring.push(event(2, 1, 60_000)));
            // The reload: takeover is published before the generation flip, then
            // the replacement generation supplies onsets from the takeover on.
            shared.takeover_frame.store(50_000, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            assert!(shared.ring.push(event(3, 2, 60_100)));

            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut output = [0.0f32; 128 * 2];
            let mut kept_audible = false;
            let mut accepted = 0usize;
            let mut stale = 0usize;
            let mut start = 0u64;
            while start < 61_000 {
                output.fill(0.0);
                let report = backend.process_block_with(
                    &mut output,
                    128,
                    start,
                    &shared.ring,
                    shared.flip_atomics(),
                    &shared.stopped,
                );
                accepted += report.accepted;
                stale += report.stale;
                if (960..1_100).contains(&start) && output.iter().any(|sample| sample.abs() > 1e-6)
                {
                    kept_audible = true;
                }
                start += 128;
            }
            assert_eq!(
                accepted, 2,
                "pre-takeover old onset and the replacement onset must both play"
            );
            assert_eq!(stale, 1, "only the post-takeover old onset is replaced");
            assert!(kept_audible, "the kept pre-takeover onset never sounded");
        }

        /// The callback reads each handshake word from its own atomic. The line
        /// arm and the cut intent are both `AtomicU64`s published by the same
        /// thread; wired into each other's place, a line arm reads as no cut and
        /// a cut reads as a line armed at frame 0 with no drop horizon. Driven
        /// through `write_live_output`, the production wiring: a line armed
        /// with its drop bit refuses an outgoing event past it, and an
        /// immediate rewind's cut refuses an outgoing event its takeover frame
        /// alone would have let through.
        #[test]
        fn the_callback_reads_the_line_arm_and_the_cut_from_their_own_words() {
            let event = |onset_id: u64, generation: u64, target_frame: u64| AudioEvent {
                onset_id,
                generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.5,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            };
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut meters = test_meter();
            let mut output = [0.0f32; 128 * 2];
            let mut callback = |backend: &mut LiveScalarBackend| {
                tripwire::audio_scope(|| {
                    write_live_output(backend, &mut meters, &mut output, 2, &shared)
                });
            };
            let stale = || shared.stale_events.load(Ordering::Acquire);

            // The line at 256, armed with its drop bit (arm_line_cut's word).
            shared.line_arm.store((256 << 2) | 0b11, Ordering::Release);
            callback(&mut backend); // [0, 128)
            callback(&mut backend); // [128, 256)
            assert!(shared.ring.push(event(1, 1, 4_000)));
            callback(&mut backend); // [256, 384): fires, refuses the event
            assert_eq!(
                stale(),
                1,
                "the armed line refuses the outgoing event past it"
            );

            // An immediate rewind (set_generation's order), its takeover past
            // the outgoing event: only the cut's own horizon refuses it.
            shared.takeover_frame.store(8_192, Ordering::Release);
            shared
                .takeover_cut
                .store(TakeoverCut::AtFlip as u64, Ordering::Release);
            shared.line_arm.store(0, Ordering::Release);
            shared.generation.store(2, Ordering::Release);
            assert!(shared.ring.push(event(2, 1, 6_000)));
            callback(&mut backend); // [384, 512)
            assert_eq!(stale(), 2, "the rewind's cut refuses the outgoing event");
        }

        /// A published flip supersedes a pre-armed line: `set_generation`
        /// leaves no arm behind, even an edit's flip that asks for no cut of its
        /// own, so a launch's stale line can never cut a later generation that
        /// never asked for one. It leaves zero, not the withdrawn marker: the
        /// consumer tells "the flip took over" from "the launch was withdrawn"
        /// by exactly that word.
        #[test]
        fn a_published_flip_leaves_no_line_arm() {
            let device = LiveScalarDevice::start_silent(48_000, 6).expect("silent output");
            device.arm_line_cut(1_000, true);
            assert!(device.line_cut_armed());
            assert_eq!(device.armed_line_word(), (1_000 << 2) | 0b11);
            assert_eq!(device.armed_line_frame(), Some(1_000));
            device.set_generation(7, 2_000, TakeoverCut::None);
            assert!(!device.line_cut_armed(), "the flip supersedes the arm");
            assert_eq!(device.armed_line_word(), 0, "and leaves no word behind");
            assert_eq!(device.armed_line_frame(), None);

            device.arm_line_cut(3_000, false);
            device.clear_line_arm();
            assert!(!device.line_cut_armed());
            assert_eq!(device.armed_line_word(), crate::LINE_ARM_WITHDRAWN);
            assert_eq!(device.armed_line_frame(), None);
            device.set_generation(8, 4_000, TakeoverCut::None);
            assert_eq!(
                device.armed_line_word(),
                0,
                "a flip after a withdrawal is a flip, not a withdrawal"
            );
        }

        #[test]
        fn live_reload_margin_does_not_recount_downstream_playback_latency() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            // Join the callback before injecting the two observations. Both
            // describe exactly the same completed render frontier; only PCM
            // already handed downstream takes longer to reach the listener.
            device.silent.take().expect("silent callback").stop();
            device.shared.frames.store(48_000, Ordering::Release);

            for requested_frames in [256, 2048] {
                device.requested_buffer_frames = requested_frames;
                device
                    .shared
                    .playback_latency_nanos
                    .store(75_000_000, Ordering::Release);
                let shallow_margin = device.continuity_margin_seconds();
                let shallow_clock = device.clock_nanos();
                device
                    .shared
                    .playback_latency_nanos
                    .store(800_000_000, Ordering::Release);
                let deep_margin = device.continuity_margin_seconds();

                assert_eq!(device.clock_nanos(), shallow_clock);
                assert_eq!(
                    deep_margin, shallow_margin,
                    "downstream delay moved the edit boundary without any additional rendering"
                );
                assert_eq!(deep_margin, f64::from(requested_frames) / 48_000.0);
            }
        }

        #[test]
        fn live_reload_margin_tracks_in_flight_work_without_certifying_it() {
            let mut device = LiveScalarDevice::start_silent(48_000, 7).expect("silent output");
            device.silent.take().expect("silent callback").stop();
            device.requested_buffer_frames = 256;
            for sample_rate in [44_100, 48_000, 96_000] {
                device.sample_rate = sample_rate;
                let startup_lead = device.schedule_lead_seconds();
                let completed = u64::from(sample_rate);
                device.shared.frames.store(completed, Ordering::Release);
                device
                    .shared
                    .rendering_until_frame
                    .store(completed + 1_024, Ordering::Release);

                assert_eq!(device.clock_nanos(), 1_000_000_000);
                assert_eq!(device.report().submitted_frames, completed);
                assert_eq!(
                    device.render_frontier_seconds(),
                    (completed + 1_024) as f64 / f64::from(sample_rate)
                );
                assert_eq!(
                    device.continuity_margin_seconds(),
                    1_280.0 / f64::from(sample_rate)
                );

                // Once copied, the reservation is no longer extra work ahead
                // of the completed clock. Do not charge for it a second time.
                device
                    .shared
                    .frames
                    .store(completed + 1_024, Ordering::Release);
                assert_eq!(
                    device.continuity_margin_seconds(),
                    256.0 / f64::from(sample_rate)
                );
                assert_eq!(device.schedule_lead_seconds(), startup_lead);
            }
        }

        #[test]
        fn live_render_frontier_advances_per_chunk_inside_an_oversized_callback() {
            // Observe the real writer at its existing sample-conversion seam;
            // no production-only test hooks, clocks, or callback allocations.
            thread_local! {
                static OBSERVED: std::cell::RefCell<Option<LiveShared>> = const {
                    std::cell::RefCell::new(None)
                };
            }
            #[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
            struct ObservedSample {
                value: f32,
                completed: u64,
                frontier: u64,
            }
            impl Sample for ObservedSample {
                type Signed = f32;
                type Float = f32;
                const EQUILIBRIUM: Self = Self {
                    value: 0.0,
                    completed: 0,
                    frontier: 0,
                };
            }
            impl FromSample<f32> for ObservedSample {
                fn from_sample_(value: f32) -> Self {
                    OBSERVED.with(|slot| {
                        let shared = slot.borrow();
                        let shared = shared.as_ref().expect("writer under observation");
                        Self {
                            value,
                            completed: shared.clock_frames(),
                            frontier: shared.render_frontier_frames(),
                        }
                    })
                }
            }
            impl FromSample<ObservedSample> for f32 {
                fn from_sample_(value: ObservedSample) -> Self {
                    value.value
                }
            }

            let _guard = tripwire_assertion_guard();
            let shared = LiveShared::new(1);
            let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
            let mut meters = test_meter();
            OBSERVED.with(|slot| *slot.borrow_mut() = Some(shared.clone()));
            for frames in [1_025, 2_048] {
                let start = shared.clock_frames();
                let mut output = vec![ObservedSample::EQUILIBRIUM; frames * 2];
                let before = tripwire::Violations::capture();
                tripwire::audio_scope(|| {
                    write_live_output(&mut backend, &mut meters, &mut output, 2, &shared)
                });
                assert!(tripwire::Violations::capture().since(before).clean());
                for (frame, pair) in output.as_chunks::<2>().0.iter().enumerate() {
                    let expected_frontier = start + ((frame / 128 + 1) * 128).min(frames) as u64;
                    for sample in pair {
                        assert_eq!(sample.completed, start, "partial copy was certified");
                        assert_eq!(sample.frontier, expected_frontier);
                    }
                }
                assert_eq!(shared.clock_frames(), start + frames as u64);
                assert_eq!(shared.render_frontier_frames(), shared.clock_frames());
            }
            OBSERVED.with(|slot| *slot.borrow_mut() = None);
        }

        #[test]
        fn live_dsp_advances_by_submitted_frames_not_playback_timestamps() {
            let shared = LiveShared::new(1);
            assert!(shared.ring.push(AudioEvent {
                onset_id: 1,
                generation: 1,
                target_frame: 128,
                onset_lead: 0.0,
                freq_hz: 220.0,
                gain: 0.8,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                synth: None,
                wavetable: None,
                cut: None,
            }));
            let mut backend = LiveScalarBackend::new(48_000, 4).expect("live scalar");
            let mut first = [0.0f32; 128 * 2];
            let mut second = [0.0f32; 128 * 2];
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut first, 2, &shared)
            });
            tripwire::audio_scope(|| {
                write_live_output(&mut backend, &mut test_meter(), &mut second, 2, &shared)
            });
            assert_eq!(shared.report(1).submitted_frames, 256);
            let first_peak = first
                .iter()
                .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
            let second_peak = second
                .iter()
                .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
            assert!(
                first_peak < 1e-6,
                "onset at frame 128 leaked into the first callback: {first_peak}"
            );
            assert!(
                second_peak > 1e-4,
                "monotonic start frames never reached the onset: {second_peak}"
            );
        }

        #[test]
        fn scheduler_clock_uses_current_playhead_not_future_buffer_position() {
            let origin = 4_000_000_000;
            let shared = LiveShared::new(1);
            shared
                .playback_origin_nanos
                .store(origin, Ordering::Release);
            shared
                .buffer_playback_nanos
                .store(12_000_000_000, Ordering::Release);
            let now = StreamInstant::from_nanos(origin + 8_000_000_000);

            assert_eq!(shared.playhead_at(now), 8_000_000_000);
            assert_ne!(
                shared.playhead_at(now),
                shared.buffer_playback_nanos.load(Ordering::Acquire)
            );
            assert_eq!(shared.playhead_at(StreamInstant::from_nanos(origin - 1)), 0);
        }
    }
    #[cfg(feature = "test-support")]
    mod sample_resampling {
        use super::*;
        use crate::{DecodedSample, SampleControls, SampleHold, SampleId, SampleResamplingMode};

        const SAMPLE_ID: SampleId = SampleId(9);
        const SOURCE_RATE: u32 = 24_000;
        const OUTPUT_RATE: u32 = 48_000;

        fn sample() -> DecodedSample {
            let pcm = (0..SOURCE_RATE * 8)
                .map(|frame| if frame % 2 == 0 { 0.0 } else { 0.2 })
                .collect();
            DecodedSample::from_parts(SOURCE_RATE, 1, pcm).expect("local sample")
        }

        fn event(generation: u64, target_frame: u64) -> AudioEvent {
            AudioEvent {
                onset_id: 1,
                generation,
                target_frame,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 1.0,
                duration_secs: 8.0,
                ui_visuals: 0,
                controls: Default::default(),
                sample: Some(SampleControls {
                    sample: SAMPLE_ID,
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: SampleHold::Hap,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                }),
                synth: None,
                wavetable: None,
                cut: None,
            }
        }

        #[test]
        fn manual_callback_uses_the_selected_sample_resampling_mode() {
            let _tripwire_guard = tripwire_assertion_guard();
            let samples = [(SAMPLE_ID, sample())];
            let render = |mode: Option<SampleResamplingMode>| {
                let mut output = match mode {
                    Some(mode) => ManualLiveOutput::new_with_samples_and_options(
                        OUTPUT_RATE,
                        7,
                        &samples,
                        LiveOutputOptions::default().with_sample_resampling_mode(mode),
                    ),
                    None => ManualLiveOutput::new_with_samples(OUTPUT_RATE, 7, &samples),
                }
                .expect("manual output");
                output.device().set_limiter(None);
                assert!(output.device().push(event(7, 0)));
                let mut pcm = [0.0; 1024 * 2];
                let before = tripwire::Violations::capture();
                output.render(&mut pcm);
                assert!(tripwire::Violations::capture().since(before).clean());
                assert!(pcm.iter().all(|sample| sample.is_finite()));
                assert!(pcm.iter().any(|sample| sample.abs() > 1e-4));
                let report = output.device().report();
                assert_eq!(report.accepted_events, 1);
                assert_eq!(report.ring_role_conflicts, 0);
                assert_eq!(report.callback_scope_misses, 0);
                pcm
            };

            assert_eq!(
                LiveOutputOptions::default().sample_resampling_mode(),
                SampleResamplingMode::Linear
            );
            let linear = render(Some(SampleResamplingMode::Linear));
            let raw = render(Some(SampleResamplingMode::Raw));
            assert_eq!(
                render(None),
                linear,
                "default output keeps linear interpolation"
            );
            assert!(
                linear
                    .iter()
                    .zip(raw.iter())
                    .any(|(a, b)| (a - b).abs() > 1e-4),
                "fractional source positions must change callback PCM"
            );
        }

        fn assert_silent_pcm(device: &LiveScalarDevice, mode: SampleResamplingMode) {
            assert_eq!(device.sample_rate(), OUTPUT_RATE);
            assert_eq!(device.output_options().sample_resampling_mode(), mode);
            let accepted = device.report().accepted_events;
            let target_frame =
                device.clock_frames() + u64::from(device.requested_buffer_frames()) * 3;
            assert!(device.push(event(device.generation(), target_frame)));
            let mut pcm = [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES];
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(snapshot) = device.copy_analysis_window(&mut pcm)
                    && snapshot.end_frame >= target_frame + LIVE_ANALYSIS_WINDOW_SAMPLES as u64
                    && device.report().accepted_events > accepted
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "silent output did not render the sample"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            let tail = &pcm[pcm.len() - 256..];
            assert!(tail.iter().all(|sample| sample.is_finite()));
            assert!(tail.iter().any(|sample| sample.abs() > 1e-4));
            // At half-rate playback, Raw repeats each zero twice. Linear inserts a
            // nonzero midpoint, so only one in four output frames stays zero.
            let zeroes = tail.iter().filter(|sample| **sample == 0.0).count();
            assert_eq!(
                zeroes,
                match mode {
                    SampleResamplingMode::Raw => 128,
                    SampleResamplingMode::Linear => 64,
                },
                "silent callback must use {mode:?} interpolation"
            );
        }

        #[test]
        fn silent_output_retains_sample_resampling_through_replacement_and_retry() {
            let _tripwire_guard = tripwire_assertion_guard();
            let samples = [(SAMPLE_ID, sample())];
            for mode in [SampleResamplingMode::Raw, SampleResamplingMode::Linear] {
                let options = LiveOutputOptions::default()
                    .with_buffer_preference(AudioBufferPreference::Frames(128))
                    .with_sample_resampling_mode(mode);
                let mut device = LiveScalarDevice::start_output_with_options(
                    Some(SILENT_OUTPUT_NAME),
                    7,
                    options,
                )
                .expect("silent selector");
                device.set_analysis_enabled(true);
                device
                    .install_sample(SAMPLE_ID, samples[0].1.clone())
                    .expect("install sample");
                assert_silent_pcm(&device, mode);

                device.set_buffer_preference(AudioBufferPreference::Frames(257));
                assert_eq!(device.output_options().sample_resampling_mode(), mode);
                device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &samples, || false)
                    .expect("replace silent output");
                assert_eq!(device.requested_buffer_frames(), 257);
                assert_silent_pcm(&device, mode);

                assert!(device.fail_output_replacement_for_test().is_err());
                assert_eq!(device.output_options().sample_resampling_mode(), mode);
                device
                    .recycle_output_to_with_samples(Some(SILENT_OUTPUT_NAME), &samples, || false)
                    .expect("retry silent output");
                assert_eq!(device.requested_buffer_frames(), 257);
                assert_silent_pcm(&device, mode);
            }
        }
    }
    mod stream_errors {
        use super::*;

        fn live_errors(gate: &Arc<CallbackGate>, shared: &LiveShared) -> impl FnMut(cpal::Error) {
            live_error_callback(
                Arc::clone(gate),
                shared.failed.clone(),
                shared.callback_errors.clone(),
            )
        }

        #[test]
        fn an_xrun_leaves_every_stream_running() {
            let xrun = || cpal::Error::with_message(cpal::ErrorKind::Xrun, "overload");

            let failed = Arc::new(AtomicBool::new(false));
            flag_stream_failure(Arc::clone(&failed))(xrun());
            assert!(!failed.load(Ordering::Acquire));

            let shared = LiveShared::new(7);
            let active = Arc::new(CallbackGate::new());
            active.activate().expect("active output");
            live_errors(&active, &shared)(xrun());
            assert!(active.is_active());
            let pending = Arc::new(CallbackGate::new());
            live_errors(&pending, &shared)(xrun());
            assert!(!shared.failed.load(Ordering::Acquire));
            assert_eq!(shared.callback_errors.load(Ordering::Acquire), 0);
            pending
                .activate()
                .expect("an xrun before activation leaves the candidate pending");
        }

        #[test]
        fn a_lost_device_still_fails_the_stream() {
            for kind in [
                cpal::ErrorKind::DeviceNotAvailable,
                cpal::ErrorKind::StreamInvalidated,
            ] {
                let failed = Arc::new(AtomicBool::new(false));
                flag_stream_failure(Arc::clone(&failed))(kind.into());
                assert!(failed.load(Ordering::Acquire), "{kind:?}");

                let shared = LiveShared::new(7);
                let active = Arc::new(CallbackGate::new());
                active.activate().expect("active output");
                live_errors(&active, &shared)(kind.into());
                assert!(shared.failed.load(Ordering::Acquire), "{kind:?}");
                assert_eq!(shared.callback_errors.load(Ordering::Acquire), 1);
            }
        }
    }
}
