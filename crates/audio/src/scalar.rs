//! Scalar synthesis, sample playback and effects for offline and live audio.

mod effects;
mod inserts;
mod kabelsalat;
mod modulation;
mod orbit;
mod render;
mod voice;

use crate::DspDispatch;
use crate::backend::{
    AudioBackend, BusMod, CompactKabelControls, CompressorControls, EnvMod, Envelope, FmControls,
    FxStage, LfoMod, MAX_VOICE_MODS, OnsetEvent, OscillatorControls, PartialsControls,
    PhaserControls, PitchEnvControls, ShapeControls, TremoloControls, VibratoControls,
    VowelControls, Waveform,
};
use crate::sample::{
    DecodedSample, SampleBank, SampleId, SampleResamplingMode, bundled_sample, stop_secs_for,
};
use crate::wavetable_kernel::WavetableKernel;
use effects::apply_fx_stage;
use effects::bandpass_coefs;
use effects::notch_coefs;
use effects::reserve_fx_reverb_returns;
use effects::voice_fx_pre_distort;
use inserts::claim_insert_bus;
use inserts::deliver;
use inserts::displace_insert;
pub(crate) use inserts::effect_stage_wakes;
pub(crate) use inserts::fade_effect_stage;
use inserts::order_insert_stages;
pub(crate) use inserts::ordered_stages;
pub(crate) use inserts::render_leaving_insert;
pub(crate) use inserts::settle_insert_block;
use kabelsalat::kabel_available;
pub(crate) use kabelsalat::kabel_controls;
#[cfg(feature = "device-audio")]
pub(crate) use kabelsalat::kabel_graph_bytes;
use kabelsalat::lease_kabel;
use modulation::env_mod_value;
use modulation::first_open_quantum;
use modulation::js_clamp;
use modulation::lfo_phase0;
use modulation::lfo_quantum_alive;
use modulation::lfo_quantum_open;
use modulation::lfo_shape;
use modulation::lfo_waveshape;
use modulation::lfo_waveshape_f64;
use modulation::nan_param_default;
use modulation::wrap_phase;
use orbit::stereo_pan;
pub(crate) use render::enable_denormal_flush;
use std::sync::Arc;
use voice::begin_frame;
use voice::fm_advance;
use voice::note_end_alive;
use voice::nudged_start;
use voice::oscillator_sample;
use voice::retire_voice_resources;
use voice::sbd_graph_lead_frames;
use voice::sbd_noise_gain;
use voice::sbd_saturation;
use voice::sbd_saturation_curve;
use voice::seeded_phase;
use voice::supersaw_detune_ratio;
use voice::supersaw_spread_geometry;
pub(crate) use voice::{saw_blep, saw_step};

use crate::biquad::FilterChain;
use crate::periodic_wave::{PeriodicWaveTables, TableSelection, prepared_tables};
#[cfg(feature = "device-audio")]
use crate::pressure::RealtimePressureObservation;
use crate::pressure::{REALTIME_POOL_COUNT, RealtimePool};

/// One shared feedback delay per orbit: a stereo one-second line. The send
/// taps the chain after the panner, and each triggering voice resets its time
/// and feedback.
#[derive(Clone, Debug, Default)]
struct OrbitDelay {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
    time_frames: usize,
    feedback: f32,
    active: bool,
    /// Stereo cells whose stored energy can still produce an audible echo.
    /// Maintaining this as the ring is overwritten avoids scanning a full
    /// one-second line in the realtime callback.
    energized: usize,
    block: OrbitDelayBlock,
}

/// Native and instrument sends join before the delay advances.
#[derive(Clone, Debug)]
struct OrbitDelayBlock {
    input: [[f32; crate::reverb::REVERB_BLOCK]; 2],
    time_mod: [f32; crate::reverb::REVERB_BLOCK],
    feedback_mod: [f32; crate::reverb::REVERB_BLOCK],
}

/// One `.FX()` stage's inline feedback delay.
///
/// An `.FX()` stage carries a feedback delay of its own instead of the
/// orbit send, so this is per voice, not per orbit. Pooled and allocated at
/// init like the compressor:
/// a 1-second stereo line is 384 KiB at 48 kHz and must never be allocated in
/// the callback.
#[derive(Clone, Debug)]
struct FxDelayLine {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
}

/// How much memory `.FX()` stage reverbs may hold between them.
///
/// Bounded in BYTES, not instances, because a convolver's cost is set by its
/// roomsize and a count-based cap has no idea what it is holding: measured
/// resident cost is 2.7 MiB at roomsize 0.5, 7.0 MiB at the default 2, and
/// 24.8 MiB at 6, so sixteen of them is 43 MiB or 397 MiB depending entirely
/// on the pattern.
///
/// CPU is the lesser limit - one stereo reverb costs 18 us per 128-frame
/// block against a 2667 us budget, so about 37 could run inside a sane 25%
/// share - which is why this bound is the memory one.
///
/// A voice that finds none plays dry and is counted, exactly like an orbit
/// whose reverb has not arrived. Refusing is the same never-die trade as
/// the voice and queue ceilings; an unbounded convolver-per-note policy
/// would hand one pattern hundreds of MiB.
pub(crate) const MAX_FX_REVERB_BYTES: usize = 64 * 1024 * 1024;

/// Maximum resident native worklet storage, including graphs currently playing.
pub const MAX_KABEL_BYTES: usize = 64 * 1024 * 1024;
/// Bounds callback activation, return slots and graph search work.
pub const MAX_KABEL_GRAPHS: usize = 128;

#[derive(Debug)]
struct KabelState {
    graph: Box<rustel_kabel::Graph>,
    start_secs: f64,
    gate_end_secs: f64,
    end_secs: f64,
    time: f64,
}

// Each stereo reverb accounts for at least one partition, matching history,
// work and accumulator per channel, even for an empty IR. Scratch and tails
// only increase that minimum. Reserve return slots for every box the existing
// byte budget can admit, including boxes currently leased to voices.
const MIN_FX_REVERB_BYTES: usize = 2
    * 4
    * (2 * crate::reverb::REVERB_BLOCK)
    * std::mem::size_of::<rustfft::num_complex::Complex<f32>>();
const MAX_INSTALLED_FX_REVERBS: usize = MAX_FX_REVERB_BYTES / MIN_FX_REVERB_BYTES;

/// Orbit indices in common use; sends beyond this refuse loudly
/// at resolve time rather than aliasing into a wrong bus.
pub const MAX_ORBITS: usize = 16;
/// An insert with no input and no output for this long sleeps. The time is
/// long, so the late echo of a delay effect still sounds.
const INSERT_IDLE_SECONDS: u32 = 30;
/// A chain the score no longer has leaves after this long with no sound.
const INSERT_QUIET_SECONDS: u32 = 1;
/// An insert output below this level is silence: about -120 dB.
const INSERT_SILENCE: f32 = 1e-6;
/// An insert output with this many silent frames in a row makes no sound:
/// a sound that goes through zero is silent for 1 frame or 2.
const INSERT_QUIET_FRAMES: u16 = 64;
/// The values sliders moved that the notes in the queue take: see
/// [`ScalarBackend::set_insert_value`].
const MAX_INSERT_VALUES: usize = 64;
/// Frames of per-orbit mix kept for routing: the largest block a host asks
/// for in one go.
pub const ORBIT_MIX_FRAMES: usize = 4096;
pub const MAX_UI_AUDIO_VISUALS: usize = 64;
pub const UI_VISUAL_MIX_FRAMES: usize = crate::reverb::REVERB_BLOCK;

/// Concurrent-voice ceiling. The per-frame mix is O(voices × frames), so an
/// unbounded list lets a pathological score (`chop(32).slow(0.001)` is
/// ~266k simultaneous voices) starve the audio callback - silence, the
/// worst live failure. An uncapped graph just dies under that; the
/// never-die contract prefers dropping the newest onset and counting it,
/// exactly like the live ring refusing when full.
pub const MAX_ACTIVE_VOICES: usize = 512;
/// One limiter for every voice that can sound at once. A voice that asks
/// for `.limit()` and does not get one plays unlimited, and the documented
/// advice, `all(x => x.limit(...))`, asks for one per voice in the set.
///
/// Each in-line limiter holds a few hundred bytes, so the whole pool is
/// small.
const LIMITER_POOL: usize = MAX_ACTIVE_VOICES;

/// Ceiling on events waiting for their onset frame. `process_block` scans
/// this list EVERY callback, so an unbounded queue turns one heavy score
/// into an audio thread that can no longer meet its deadline - frames stop
/// advancing while every other part looks healthy. Dropping the newest
/// admission keeps callback work bounded, matching the live ring's policy.
pub const MAX_PENDING_EVENTS: usize = 8192;

/// The subset of event controls still consulted after a voice is activated.
///
/// [`OscillatorControls`] is the fixed event-boundary record, so it also owns
/// one-shot setup data such as routing, sends, filter configuration and rare
/// `.FX()` stages. The callback turns those into prepared voice, orbit and
/// pooled stage state once. Retaining them afterward would inflate the stride
/// walked for every voice and every sample. Keep the exhaustive projection
/// below: a newly added event control must be consciously classified as
/// render-time or activation-only.
#[derive(Clone, Copy, Debug)]
struct VoiceRenderControls {
    noise: f32,
    waveform: Waveform,
    envelope: Envelope,
    worklet_begin_secs: f32,
    lfo_end_secs: f32,
    filter_lfo_end_secs: f32,
    modulator_release_secs: f32,
    postgain: f32,
    distort: Option<crate::distortion::DistortControls>,
    stretch: Option<f32>,
    fm: Option<FmControls>,
    lfos: [Option<LfoMod>; MAX_VOICE_MODS],
    bus_mods: [Option<BusMod>; MAX_VOICE_MODS],
    bus: Option<u8>,
    busgain: f32,
    envs: [Option<EnvMod>; MAX_VOICE_MODS],
    phaser: Option<PhaserControls>,
    tremolo: Option<TremoloControls>,
    vowel: Option<VowelControls>,
    coarse: Option<f32>,
    crush: Option<f32>,
    shape: Option<ShapeControls>,
    vibrato: Option<VibratoControls>,
    pitch_env: Option<PitchEnvControls>,
    compressor: Option<CompressorControls>,
    partials: Option<PartialsControls>,
}

/// Not `Clone`: a voice leases pooled state (a compressor, a delay line, a
/// reverb with its impulse response) that exists once and is returned when the
/// voice is dropped. Copying one would duplicate the lease.
#[derive(Debug)]
struct Voice {
    /// Only these directly bound controls can move on a sustained voice.
    live_gain: crate::live_control::Ramp,
    live_cutoff: crate::live_control::Ramp,
    live_resonance: crate::live_control::Ramp,
    /// The direct FM route's index; see [`VoiceSource`] for the routes it
    /// rides. Inert while the voice carries no such route.
    live_fm_index: crate::live_control::Ramp,
    /// Frame at which the connected graph begins producing output. This is
    /// normally the source onset. SBD is the exception: its graph begins one
    /// scheduler lead earlier because its asymmetric WaveShaper curve produces
    /// a tiny non-zero value before the oscillator starts.
    graph_start_frame: u64,
    start_frame: u64,
    generation: u64,
    /// The generation whose takeover kept this voice and plays its onset
    /// again. `None` once the voice stands in for that copy. See
    /// [`ScalarBackend::keep_started_onsets`].
    replayed_by: Option<u64>,
    /// The onset is one frame before the takeover frame, so the copy is on
    /// the next frame and nowhere else. Read only with `replayed_by` set.
    replayed_on_next_frame: bool,
    ui_visuals: u64,
    preview_epoch: u64,
    piano: bool,
    /// Sub-sample onset placement: how far the source has already advanced at
    /// `start_frame` (see OnsetEvent::onset_lead). Rounding this away costs up
    /// to half a sample per onset, which `crush` then magnifies into whole
    /// quantisation steps.
    onset_lead: f32,
    /// The distortion curve at the unmodulated shape, prepared at onset.
    distort_curve: Option<crate::distortion::DistortCurve>,
    /// Wet send into the orbit delay (0 = no send).
    delay_wet: f32,
    /// Wet send into the orbit reverb (0 = no send).
    reverb_wet: f32,
    /// The dry path feeds the orbit insert, not the orbit.
    through_insert: bool,
    insert_bus: Option<usize>,
    dry_gain: f32,
    kabel: Option<KabelState>,
    stretch: Option<Box<[crate::stretch::Stretch; 2]>>,
    orbit: usize,
    freq_hz: f32,
    duration_secs: f32,
    /// The hap's own duration, which for a sample is NOT `duration_secs`: a
    /// sample held for its slice sounds on past the note. Modulators end on
    /// the note, so this is the length they live for.
    hap_duration_secs: f32,
    controls: VoiceRenderControls,
    /// Whether any modulator contributes through [`ModAdds`]. Most voices do
    /// not, so they share one immutable zero frame instead of clearing the
    /// full accumulator for every rendered sample.
    has_param_modulators: bool,
    source: VoiceSource,
    amplitude_scale: f32,
    gate_value: f32,
    left_gain: f32,
    right_gain: f32,
    /// StereoPanner x in [-1, 1] for STEREO sources (2·pan − 1; 0 when the
    /// pan control is absent).
    pan_x: f32,
    filters: FilterChain,
    has_filters: bool,
    /// Whether vowel, coarse, crush, or shape has real work to perform.
    /// Ordinary voices skip that whole routine instead of checking four empty
    /// controls for every rendered channel and sample.
    has_pre_distort_fx: bool,
    /// Second-channel filter state, present only for stereo sources - the
    /// biquad chain processes each channel independently.
    filters_right: Option<FilterChain>,
    stop_secs: f32,
    /// The bank slot this voice reads was emptied under it - an uninstall
    /// landed - so it renders silence for the rest of the block and retires
    /// at the block's end. Set on the audio thread; the id may then go to
    /// another sound, and a voice still reading it would play that.
    source_gone: bool,
    /// Free-running LFO modulator phases,
    /// one per `controls.lfos` slot, advanced once per rendered frame.
    /// f64: see [`lfo_phase0`].
    mod_lfo_phases: [f64; crate::backend::MAX_VOICE_MODS],
    /// The detune ratio a sample plays this block at.
    ///
    /// A buffer source's `detune` and `playbackRate` are k-rate: the browser
    /// reads them once per 128-frame quantum, from the value the automation
    /// holds at the end of that quantum, and plays the whole block at the
    /// resulting rate. An oscillator's are a-rate and do move per sample,
    /// which is why only samples need this.
    sample_pitch_hold: Option<f32>,
    /// Phaser LFO phase. `None` until the first sample past the onset
    /// quantum: the LFO seeds on its first run past the begin gate, with
    /// the frequency it reads there. A modulated rate therefore changes the
    /// seed, not just the advance, so this cannot be computed when the
    /// voice is built.
    /// f64: see [`lfo_phase0`].
    phaser_phase: Option<f64>,
    /// Notch biquad state, [mono/left, right].
    phaser_notch: [NotchState; 2],
    /// Tremolo LFO phase, seeded on first use like [`Self::phaser_phase`].
    /// f64: see [`lfo_phase0`].
    tremolo_phase: Option<f64>,
    /// Modulation of other modulators, latched per quantum: both the LFO
    /// and envelope modulators read their params once per quantum.
    mod_param_hold: ModParamAdds,
    /// The transient shaper's followers, when the voice has one.
    transient: Option<TransientState>,
    /// The main stage's gain, held back until after the `.FX()` stages.
    fx_post_gain: f32,
    /// One cold configuration and mutable state bundle per `.FX()` stage. A
    /// stage is a complete effects pass, so it cannot share the main chain's
    /// filters - running two lowpasses through one biquad would be a different
    /// filter, not two. The box is leased from a pool prepared before
    /// playback, never allocated by the audio callback.
    fx_stage_state: Option<Box<FxStageStates>>,
    /// f64: see [`lfo_phase0`].
    vibrato_phase: f64,
    /// Mutable operator state exists only for an FM voice. The box is leased
    /// from a pool prepared before playback, so ordinary voices keep this
    /// cold state out of their per-sample stride without allocating in the
    /// audio callback.
    fm_state: Option<Box<FmState>>,
    /// The same sub-quantum offset, for the PITCH envelope's param.
    ///
    /// The pitch envelope's automation begins exactly at the note's start,
    /// and detune is read from the START of the render quantum, so a note
    /// beginning mid-quantum spends its first `128 - offset` samples
    /// undetuned rather than at the envelope's floor.
    ///
    /// Note 1 of a score lands on frame 0 and never sees this; every later
    /// note does. Proved by quantum alignment against Chromium at cps 0.5:
    /// `seg(3)`, `seg(5)` and `seg(6)` put every onset on a 128-frame
    /// boundary and score corr 1.000000, while `seg(4)` (187.5 quanta) and
    /// `seg(8)` (93.75) score 0.929 and 0.945.
    pitch_quantum_skip: u8,
    /// The pink source `noise` crossfades in.
    source_noise: NoiseGen,
    /// Vowel formant bank state: 5 bandpass biquads × [mono/left, right].
    vowel_filters: [[NotchState; 5]; 2],
    /// Static formant bandpass coefficients, precomputed at voice start.
    vowel_coefs: [NotchCoefs; 5],
    /// Coarse sample-and-hold last output, per channel.
    coarse_hold: [f32; 2],
    fx_mod_hold: FxParamHold,
    /// Gate value for the pitch envelope's linear shape.
    pitch_gate: f32,
    /// Event/sample choke group. Keeping it on the live voice means groups
    /// cannot exhaust a separate lifetime registry and strand held notes.
    cut_group: Option<u32>,
    id: u64,
    /// Absolute frame at which a later cut-group trigger started fading
    /// this voice (1 → 0 over 10 ms).
    cut_fade_frame: Option<u64>,
    /// Absolute frame at which the polyphony cap started fading this voice
    /// (1 → 0 over 250 ms). Set once and never cleared: a voice the cap has
    /// dropped cannot be chosen again.
    polyphony_fade_frame: Option<u64>,
    /// Per-voice compressor delay + detector, leased from the backend's
    /// pre-allocated pool (never allocated in the callback).
    compressor: Option<Box<CompressorState>>,
    /// `limit` - a brickwall in line on this voice, leased from its own
    /// pool for the same reason: the ring is kilobytes and the callback
    /// allocates nothing.
    limiter: Option<Box<crate::limiter::InlineLimiter>>,
}

/// State advanced only while a voice has an FM operator graph.
#[derive(Debug)]
struct FmState {
    /// Phase per operator, index 0 being the one that reaches the carrier.
    /// Double precision: an operator integrates for a whole note, and an f32
    /// phase drifts audibly over a few seconds where the carrier's own
    /// short-lived phase does not - most of all when an operator modulates
    /// another at its own frequency, where the drift beats against the pair.
    phases: [f64; crate::backend::MAX_FM_OPERATORS + 1],
    /// How far into its render quantum this note starts, until the
    /// modulators have caught that up. An oscillator's first quantum is
    /// written from its sub-quantum offset while its frequency param is read
    /// from the start of that quantum.
    quantum_skip: u8,
    /// Noise state per operator, used where `fmwave` names a noise kind.
    noise: [NoiseGen; crate::backend::MAX_FM_OPERATORS + 1],
}

/// Direct-form-I biquad state for the phaser notch (WebAudio BiquadFilter,
/// notch type: b0=1, b1=−2cosω0, b2=1, a=1±α, α=sinω0/(2Q), normalized).
#[derive(Clone, Copy, Debug, Default)]
struct NotchState {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

/// The four stages `voice_fx_pre_distort` walks, passed on their own so a
/// `.FX()` stage - which has no `OscillatorControls` of its own - can run the
/// same code as the main chain.
#[derive(Clone, Copy)]
struct FxWorkletControls<'a> {
    vowel: Option<&'a crate::backend::VowelControls>,
    coarse: Option<f32>,
    crush: Option<f32>,
    shape: Option<crate::backend::ShapeControls>,
}

/// Per-`.FX()` stage's cold configuration and mutable render state.
#[derive(Debug)]
struct FxStageState {
    /// Immutable stage configuration is cold unless `.FX()` is active, so it
    /// lives with this already-pooled state instead of inflating every voice.
    controls: FxStage<CompactKabelControls>,
    kabel: Option<KabelState>,
    filters: FilterChain,
    /// Present only for a stereo path, matching the main chain's split.
    filters_right: Option<FilterChain>,
    coarse_hold: [f32; 2],
    /// Each stage owns its state outright - none of it may be shared with
    /// the main chain or with another stage. Sharing it would make two
    /// stages that name the same effect behave as one.
    vowel_coefs: [NotchCoefs; 5],
    vowel_filters: [[NotchState; 5]; 2],
    /// Seeded on first use, exactly like the main chain's: the LFO reads its
    /// frequency at the first quantum past the begin gate.
    tremolo_phase: Option<f64>,
    phaser_phase: Option<f64>,
    phaser_filters: [NotchState; 2],
    compressor: Option<Box<CompressorState>>,
    /// Whether the SOURCE is mono, which is not the same as having no right
    /// filter chain: naming pan in a stage forces the stereo path, because a
    /// StereoPanner upmixes. StereoPanner has two laws and this picks between
    /// them - see the pan stage.
    source_is_mono: bool,
    /// One phase vocoder per channel, exactly like the main chain's.
    stretch: Option<Box<[crate::stretch::Stretch; 2]>>,
    transient: Option<TransientState>,
    delay: Option<FxDelayLine>,
    room: Option<Box<crate::reverb::OrbitReverb>>,
}

type FxStageStates = [Option<FxStageState>; crate::backend::MAX_FX_STAGES];

/// Transient shaper state, per voice.
///
/// Two one-pole followers per channel track the signal at different speeds;
/// their difference is how peaky it is right now. The makeup gain is averaged
/// across BOTH channels - `avgGain` is processor state, not per-channel - so
/// a transient in one channel pulls the other's level with it.
#[derive(Clone, Copy, Debug)]
struct TransientState {
    attack_env: [f32; 2],
    sustain_env: [f32; 2],
    avg_gain: f32,
    attack_coeff: f32,
    sustain_coeff: f32,
    gain_coeff: f32,
    attack_amt: f32,
    sustain_amt: f32,
    scaling: f32,
    mix: f32,
}

/// Per-orbit DJ filter: sticky once any
/// trigger on the orbit sets `djf`; value < 0.49 sweeps a 2-pole lowpass,
/// > 0.51 a highpass (input − lowpass), cutoff (v·11)^4 Hz, resonance 0.1.
#[derive(Clone, Copy, Debug, Default)]
struct DjfState {
    active: bool,
    value: f32,
    /// Summed `djf` modulation from the voices on this orbit, latched once
    /// per 128-frame quantum because DJFProcessor reads `parameters.value[0]`.
    modulation: f32,
    /// TwoPoleFilter state per channel: [s0, s1].
    s: [[f64; 2]; 2],
}

/// Per-voice dynamics compressor, matched sample-for-sample against
/// Chromium's DynamicsCompressorNode.
///
/// NOT the Giannoulis dB-domain design most readings of the spec arrive at
/// (a dB gain computer with branching attack/release smoothing). This one
/// runs an exponential knee (`k` solved by bisection), a per-sample
/// linear-domain detector, and a compressor gain that evolves per 32-frame
/// DIVISION - multiplicatively on release with a 4th-order adaptive-release
/// polynomial, exponentially toward an `asin`-pre-warped target on attack -
/// then post-warps the gain through `sin` and applies
/// `(1 / saturate(1, k))^0.6` makeup. The dB-domain design measured 2.88
/// over the first 480 frames (settling to ~0.95) on
/// `compressor("-20:20:10:.002:.02")`, and -2.78 dB with a threshold at 0.
///
/// The 6 ms pre-delay and its 1023-frame ceiling are Chromium-verified:
/// impulse onsets at 264/288/576/1023/1023 for 44.1/48/96/176.4/192 kHz -
/// the pre-delay is `preDelayTime · sampleRate` truncated, never rounded to
/// a quantum.
///
/// The compressor exists from GRAPH BUILD, not from the note's onset: it
/// processes silence from context time zero until the source starts, and
/// its detector COLD-STARTS at zero, so the early gain dips and recovers
/// along the adaptive release curve. `new` replays that silence
/// (division-stepped, with an early exit at the silence fixed point of
/// detector 1, gain 1), so a voice triggered mid-render starts from the
/// state the graph would have reached.
const COMPRESSOR_MAX_PREDELAY_FRAMES: usize = 1024;
/// The ring is exactly the max pre-delay; indices are masked by 1023.
const COMPRESSOR_RING_FRAMES: usize = COMPRESSOR_MAX_PREDELAY_FRAMES;

/// Release zone values 0 -> 1, and the 4th-order polynomial fitted through
/// them (y1: x == 0 … y4: x == 3). The constants are load-bearing; do not
/// refit.
const COMPRESSOR_RELEASE_ZONES: [f32; 4] = [0.09, 0.16, 0.42, 0.98];
const COMPRESSOR_POLY: [[f64; 4]; 5] = [
    [
        0.999_999_999_999_999_8,
        1.843_221_968_432_392_3e-16,
        -1.937_339_435_167_642_3e-16,
        8.824_516_011_816_245e-18,
    ],
    [
        -1.578_832_035_284_588_8,
        2.330_583_703_207_428_6,
        -0.914_119_420_484_042_9,
        0.162_367_752_561_203_2,
    ],
    [
        0.533_414_286_910_642_4,
        -1.272_736_789_213_631,
        0.925_885_604_220_751_2,
        -0.186_563_101_917_762_26,
    ],
    [
        0.087_834_631_382_072_34,
        -0.169_416_296_792_562_2,
        0.085_880_579_515_952_72,
        -0.004_298_914_105_462_83,
    ],
    [
        -0.042_416_883_008_123_074,
        0.111_569_382_798_760_2,
        -0.097_646_763_252_658_72,
        0.028_494_263_462_021_576,
    ],
];
/// Detector release time.
const COMPRESSOR_SAT_RELEASE_SECS: f32 = 0.0025;

#[derive(Clone, Copy, Debug)]
struct CompressorState {
    /// Interleaved stereo pre-delay ring, index-masked.
    ring: [[f32; 2]; COMPRESSOR_RING_FRAMES],
    read: usize,
    write: usize,
    delay_frames: usize,
    /// Absolute frame counter, in lock-step with the render's, so the
    /// 32-frame division boundaries stay aligned with the context's.
    frame: u64,
    detector_average: f32,
    compressor_gain: f32,
    db_max_attack_diff: f32,
    /// Latched at each division boundary.
    envelope_rate: f32,
    scaled_desired_gain: f32,
    /// (threshold, knee, ratio) the static curve below was built from;
    /// `UpdateStaticCurveParameters` recomputes only on change.
    cached_params: (f32, f32, f32),
    k: f32,
    linear_threshold: f32,
    knee_threshold: f32,
    db_knee_threshold: f32,
    db_yknee_threshold: f32,
    slope: f32,
    linear_post_gain: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct NotchCoefs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// What a modulator aimed at one `.FX()` stage contributes.
///
/// Deliberately narrower than [`ModAdds`]: a stage is a fixed chain, so only
/// the parameters it actually builds can be a target, and carrying the whole
/// main-chain set per stage would multiply the per-voice state for buckets
/// nothing can reach.
#[derive(Clone, Copy, Default)]
struct FxStageAdds {
    lowpass_hz: f32,
    highpass_hz: f32,
    bandpass_hz: f32,
    lowpass_q: f32,
    highpass_q: f32,
    band_q: f32,
    gain: f32,
    gain_base: f32,
}

/// Per-frame modulator sums, one bucket per supported AudioParam target.
#[derive(Clone, Copy, Default)]
struct ModAdds {
    /// Modulators naming an `fxi` land here instead of the main buckets.
    fx_stage: [FxStageAdds; crate::backend::MAX_FX_STAGES],
    lowpass_hz: f32,
    highpass_hz: f32,
    bandpass_hz: f32,
    vowel_hz: f32,
    gain: f32,
    gain_base: f32,
    frequency_hz: f32,
    pan: f32,
    postgain: f32,
    postgain_base: f32,
    lowpass_q: f32,
    highpass_q: f32,
    band_q: f32,
    coarse: f32,
    crush: f32,
    shape: f32,
    shape_vol: f32,
    distort: f32,
    distort_vol: f32,
    delay_send: f32,
    room_send: f32,
    phaser_rate: f32,
    phaser_sweep: f32,
    phaser_center: f32,
    phaser_depth: f32,
    comp_threshold: f32,
    comp_ratio: f32,
    comp_knee: f32,
    comp_attack: f32,
    comp_release: f32,
    trem_rate: f32,
    trem_depth: f32,
    trem_skew: f32,
    trem_shape: f32,
    vib_rate: f32,
    vib_depth: f32,
    pulse_width: f32,
    dry: f32,
    /// A-rate modulation of the shared orbit DelayNode's `delayTime`.
    delay_time: f32,
    /// A-rate modulation of the GainNode on the shared delay's feedback edge.
    delay_feedback: f32,
    /// Per FM operator, slot 0 being the one that reaches the carrier. Both
    /// are a-rate - an oscillator frequency and an ordinary gain - so unlike
    /// the block-latched params they are NOT latched per quantum.
    fm_index: [f32; crate::backend::MAX_FM_OPERATORS + 1],
    fm_freq: [f32; crate::backend::MAX_FM_OPERATORS + 1],
    /// Modulation aimed at another MODULATOR rather than at the voice.
    mod_params: ModParamAdds,
    /// The wavetable / supersaw source's own a-rate params.
    position: f32,
    freqspread: f32,
    panspread: f32,
    warp: f32,
    djf: f32,
}

/// Modulation aimed at a filter's own LFO rather than at the filter.
#[derive(Clone, Copy, Debug, Default)]
struct FilterLfoAdds {
    depth: f32,
    dc: f32,
    skew: f32,
}

/// Modulation aimed at one of the pattern's own `lfo()` modulators, by id.
#[derive(Clone, Copy, Debug, Default)]
struct LfoParamAdds {
    rate: f32,
    depth: f32,
    skew: f32,
    curve: f32,
    dcoffset: f32,
}

/// What a modulator can move on the wavetable source: its a-rate params plus
/// its own position LFO, the latter already latched for the quantum.
#[derive(Clone, Copy, Debug, Default)]
struct WavetableAdds {
    position: f32,
    freqspread: f32,
    panspread: f32,
    lfo_rate: f32,
    lfo_depth: f32,
    lfo_skew: f32,
    warp: f32,
    warp_lfo_rate: f32,
    warp_lfo_depth: f32,
    warp_lfo_skew: f32,
}

/// Modulation aimed at the wavetable's own position LFO.
#[derive(Clone, Copy, Debug, Default)]
struct WtLfoAdds {
    rate: f32,
    depth: f32,
    skew: f32,
}

/// Modulation of the pulse oscillator's own width LFO. Both params are read
/// through `parameters.x[0]`, so these values are quantum-held.
#[derive(Clone, Copy, Debug, Default)]
struct PulseWidthLfoAdds {
    rate: f32,
    depth: f32,
}

/// The same for an `env()`.
#[derive(Clone, Copy, Debug, Default)]
struct EnvParamAdds {
    depth: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
}

/// Everything a modulator can aim at ANOTHER modulator. Latched per render
/// quantum, because LFO and envelope params are block-rate.
#[derive(Clone, Copy, Debug, Default)]
struct ModParamAdds {
    /// Indexed by `FilterLfoKind::index` - lowpass, highpass, bandpass.
    filter: [FilterLfoAdds; 3],
    /// Indexed by the target modulator's id.
    lfo: [LfoParamAdds; crate::backend::MAX_VOICE_MODS],
    env: [EnvParamAdds; crate::backend::MAX_VOICE_MODS],
    /// The wavetable's own position LFO, and its warp LFO.
    wt: WtLfoAdds,
    warp: WtLfoAdds,
    /// The pulse source's own width LFO.
    pulse_width: PulseWidthLfoAdds,
}

/// These params latch at the START of each 128-frame render quantum and
/// hold for the whole block. Modulation of them is therefore block-rate,
/// unlike the a-rate filter and gain params above; sampling them per-sample
/// would sweep more smoothly than the latched behavior - audibly wrong.
#[derive(Clone, Copy, Debug, Default)]
struct FxParamHold {
    coarse: f32,
    crush: f32,
    shape: f32,
    shape_vol: f32,
    /// The phaser's LFO params. `frequency` and `depth` are block-latched
    /// too, so a modulator on the rate moves in quantum steps - and the
    /// rate matters more than the rest, because phase integrates it and a
    /// per-sample rate would drift the phase.
    phaser_rate: f32,
    phaser_sweep: f32,
    trem_rate: f32,
    trem_skew: f32,
    trem_shape: f32,
}

/// Sidechain automation on one orbit's output gain: hold the current value
/// about 10 ms before the trigger, ramp exponentially to the ducked value at
/// t+onset, then ramp exponentially back to 1 at t+onset+attack. Piecewise
/// math over absolute frames avoids per-frame state stepping, so retriggers
/// capture the true current value.
#[derive(Clone, Copy, Debug)]
struct PendingDuck {
    arm_frame: u64,
    onset_frame: u64,
    controls: crate::backend::DuckControls,
}

#[derive(Clone, Copy, Debug, Default)]
struct DuckSegments {
    active: bool,
    capture_frame: u64,
    from: f32,
    ducked: f32,
    mid_frame: u64,
    end_frame: u64,
}

/// Two generations of automation: arming happens at event INTAKE (a
/// schedule lead ahead of the onset), so frames before the new capture
/// point must still render the PREVIOUS curve - exactly what the old
/// AudioParam timeline would have played until `cancelScheduledValues`
/// fires 10 ms before the trigger.
#[derive(Clone, Copy, Debug, Default)]
struct OrbitDuck {
    current: DuckSegments,
    previous: DuckSegments,
}

/// Per-frame DC removal of an input voice's FM phase, one ring window per
/// e-fold: slow enough to leave musical rates alone, fast enough to keep the
/// integrated bend from walking the read into the writer or out of the ring.
const INPUT_FM_LEAK: f64 = 1.0 / crate::input::INPUT_RING_FRAMES as f64;

/// Input-ring frames reserved beside the callback's own while any input
/// voice runs the FM matrix: room for the integrated bend to read ahead of
/// the shared cursor without crossing the writer.
const INPUT_FM_HEADROOM: u64 = 512;

#[derive(Clone, Debug)]
// The wavetable variant carries 32 f64 phases, which makes it much larger
// than the rest. Boxing it would put a pointer chase in the per-sample voice
// loop to save memory we are not short of - one voice's phases are 256 bytes.
#[allow(clippy::large_enum_variant)]
enum VoiceSource {
    /// `s("in")` - one channel of the audio input, read from the ring the
    /// input callback fills.
    Input {
        channel: u8,
        /// The FM matrix's integrated read-rate deviation, in input frames:
        /// how far this voice reads from the shared cursor. Each sample adds
        /// the FM sum against the voice's nominal frequency, so the voice
        /// plays the ring back at a bent rate while `note` still does not
        /// retune the input.
        fm_phase: f64,
    },
    /// `s("bus")` - the voice's source is bus N's mix rather than a generator.
    /// Receivers are evaluated after the senders, so the
    /// sum for this sample is already complete when it is read.
    Bus { bus: u8 },
    /// ZzFX's generate loop, streamed (see `crate::zzfx`). The ring is the
    /// `zdelay` history - leased from the pool only when the voice needs one,
    /// and an exhausted pool plays the voice undelayed rather than not at all.
    ZzFx {
        voice: crate::zzfx::ZzfxVoice,
        ring: Option<Box<[f32]>>,
    },
    /// `s("bytebeat")`. `t` is an integer sample counter, not a phase:
    /// seeded at `begin * sampleRate` on the first block past the begin
    /// gate and incremented once per sample from there.
    ByteBeat {
        expression: u8,
        /// A compiled custom expression, taking precedence over `expression`.
        program: Option<crate::bytebeat::ByteBeatProgram>,
        t: f64,
        /// Floored `byteBeatStartTime`, or zero after its presence has already
        /// selected the initial counter. Keep the sample loop branch-free.
        start_offset: f64,
        /// First frame the source may emit on: the begin gate holds through
        /// the quantum containing the onset, so output starts at the next
        /// 128-frame boundary strictly after it - and `t` still begins at
        /// the onset frame, not at the gate.
        gate: u64,
    },
    Oscillator {
        table_selection: TableSelection,
        phase: f64,
        /// A custom wave's most partials below Nyquist at any pitch played
        /// so far; see [`crate::periodic_wave::CustomWave::partial_limit`].
        partial_limit: usize,
    },
    Sample {
        sample: SampleId,
        position: f64,
        increment: f64,
        /// `speed < 0`: read at `(frames−1) − position` - linear interp on
        /// a reversed buffer equals interp of the original at L−1−p, so
        /// only the read index flips.
        reversed: bool,
        /// `nudge` - source frames still to wait before playback begins
        /// (the amplitude envelope is NOT delayed).
        delay_frames: u32,
        /// Soundfont zone loop region in SOURCE frames - the buffer keeps
        /// looping for the voice's whole life while the envelope gates it.
        loop_frames: Option<(f64, f64)>,
    },
    /// Wavetable oscillator, per-voice phases inline (no allocation at
    /// trigger time on the audio thread).
    Wavetable {
        controls: crate::backend::WavetableControls,
        /// f64 on purpose. f32 is close enough for every warp mode that
        /// reads the table smoothly, and not for the ones that quantise:
        /// BINARY bit-reverses a 128-step staircase, so a phase landing one
        /// step over sends the read somewhere entirely different.
        phases: [f64; MAX_UNISON],
        /// The position LFO's phase. `None` until first use, seeded then
        /// from `frac(onset · rate)` with the rate READ THERE - the same
        /// contract as every other LFO. Integrating rather than recomputing
        /// `frac(now · rate)` is what makes a MODULATED rate come out
        /// right: frequency is a velocity, not a position.
        lfo_phase: Option<f64>,
        /// The warp LFO's phase, on the same contract.
        warp_lfo_phase: Option<f64>,
    },
    /// Supersaw: polyblep saw stack with per-voice detune and alternating
    /// pan.
    Supersaw {
        plan: PreparedSupersaw,
        phases: [f32; MAX_UNISON],
    },
    /// Pulse: half-Tomisawa cosine pair with feedback and anti-hunting
    /// filters - including the deliberate per-128-frame internal decay
    /// quirk.
    Pulse {
        pulsewidth: f32,
        width_lfo: Option<crate::backend::PulseWidthLfoControls>,
        width_lfo_phase: Option<f64>,
        phi: f64,
        y0: f64,
        y1: f64,
        dphif: f64,
        envf: f64,
        env: f64,
    },
    /// `sbd`: triangle osc with an exponential pitch envelope into its sampled
    /// WaveShaper curve and amplitude envelope, plus a 25 ms brown-noise
    /// attack. It bypasses the standard ADSR by design - the drum's own
    /// envelope is the whole sound.
    Sbd {
        controls: SbdState,
        phase: f64,
        noise: NoiseGen,
    },
    /// The looping noise buffer, played back from index 0.
    Noise {
        kind: u8,
        density: f32,
        state: NoiseGen,
    },
    /// `s("one")` - a constant 1.
    One,
}

/// Immutable supersaw work shared by every sample of one voice.
///
/// The source's lane count, static detune ratios, and static pan gains depend
/// only on onset controls. Preparing them once keeps transcendental work out
/// of the steady sample loop. A-rate modulation still takes the original
/// calculation path for the samples where it contributes.
#[derive(Clone, Debug)]
struct PreparedSupersaw {
    voices: f32,
    count: usize,
    freqspread: f32,
    panspread: f32,
    detune_ratios: [f32; MAX_UNISON],
    pan_gains: (f32, f32),
}

#[derive(Debug, Default)]
struct ScalarPressureState {
    active_orbit_voices: [u16; MAX_ORBITS],
    active_pool_leases: [u64; REALTIME_POOL_COUNT],
    semantic_polyphony_fades: u64,
    voice_ceiling_drops: u64,
    pending_ceiling_drops: u64,
    pool_misses: [u64; REALTIME_POOL_COUNT],
    orbit_reverb_misses: u64,
}

/// The noise source is a two-second LOOPED buffer, so a note restarts the
/// same samples from index 0 and only wraps if it outlives the buffer.
#[derive(Clone, Copy, Debug)]
struct NoiseGen {
    rng: u64,
    /// The pink filter's seven poles (`b0`..`b6`).
    pink: [f32; 7],
    brown_last: f32,
    /// Position in the two-second buffer; wrapping restarts the sequence.
    index: u32,
    len: u32,
    /// Sub-sample read offset, `OnsetEvent::onset_lead`.
    ///
    /// A noise note starts at a FLOAT time, so one landing between output
    /// samples reads the buffer at a fractional position and every returned
    /// sample is linearly interpolated. On full-bandwidth material that is
    /// not a nuance: adjacent noise samples are uncorrelated, so mixing
    /// them attenuates by `sqrt((1-f)^2 + f^2)` - 0 dB at a whole sample
    /// and -3.01 dB at half of one. Measured against Chromium on
    /// `s("white")` at eight forced offsets, that curve held to within
    /// 0.4%.
    ///
    /// 0 for the FM modulator path, which always begins on a frame
    /// boundary.
    frac: f32,
    /// The next buffer sample, already generated. The recurrences only run
    /// forwards, so interpolating needs one sample of lookahead.
    pending: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
struct SbdState {
    decay_secs: f32,
    pdecay_secs: f32,
    penv_semitones: f32,
    stop_secs: f32,
}

/// SBD's graph becomes active one scheduler lead before its source starts.
/// This lets the zero-input WaveShaper establish the required pre-onset state
/// while the oscillator and noise clocks remain parked. A scheduling tick may
/// construct the graph earlier; 100 ms is the stable lower bound.
const SBD_GRAPH_LEAD_SECS: f64 = 0.1;

/// How far, in seconds, a takeover can move an onset whose copy a kept voice
/// still stands in for. A kept SBD onset is at most one graph lead after the
/// flip. A tempo change of 9% that starts at the flip, as a clock steer
/// does, moves it this far. A save changes the tempo from the edit instant,
/// which is before the flip, so a smaller tempo edit moves the onset as far.
/// The reach is half the 20 ms between the hits of a fast roll, so the
/// nearest kept voice is the voice of the same hit.
const KEPT_ONSET_REACH_SECS: f64 = 0.01;

/// Native capacity limit that keeps each `Voice` bounded while supporting
/// dense unison configurations.
pub const MAX_UNISON: usize = 32;

/// Default polyphony cap. Sounds beyond it are faded out oldest-first; see
/// [`ScalarBackend::cull_for_polyphony`].
pub const MAX_POLYPHONY: usize = 128;

/// Largest supported musical voice limit. The separate hard voice ceiling
/// retains room for the quarter-second fades of voices retired by this limit.
pub const MAX_CONFIGURABLE_POLYPHONY: usize = 256;

struct PolyphonyLimit(usize);

/// The fade a culled sound gets.
const POLYPHONY_FADE_SECS: f32 = 0.25;
/// Duration of the linear choke fade from full gain to silence, in seconds.
const CUT_FADE_SECS: f32 = 0.01;

/// Stable admission order without compacting the large onset descriptions.
///
/// A live callback keeps roughly two seconds of future events here. Removing
/// the events due in one block with `Vec::retain` copied every later
/// [`OnsetEvent`] down over the holes. Those controls are large, and the copy
/// became a material part of callback time on dense scores. Events stay in
/// stable slots while a compact index vector preserves admission order. A
/// retain pass may move eight-byte indices, never the event payloads, and
/// removed slots form an allocation-free free list for later intake.

#[derive(Default)]
struct PendingEvents {
    slots: Vec<PendingEventSlot>,
    order: Vec<usize>,
    free: Option<usize>,
}

struct PendingEventSlot {
    event: Option<OnsetEvent<CompactKabelControls>>,
    confirmation: Option<crate::confirmation::ConfirmationOnset>,
    expected_sample_identity: Option<u64>,
    next_free: Option<usize>,
}

/// The orbit faders, at unity by default. A backend built from `Default`
/// must not open silent.
#[derive(Clone, Copy, Debug)]
pub struct OrbitGains(pub [f32; MAX_ORBITS]);

/// The orbit taps of a stems render: every orbit's finished stereo for the
/// block in hand, beside which orbits have made sound. Kept only while an
/// offline stems render asks for them - the live path never pays for taps
/// it cannot use.
#[derive(Default)]
pub struct Stems {
    /// Interleaved stereo per orbit: `MAX_ORBITS` blocks of
    /// [`crate::reverb::REVERB_BLOCK`] frames.
    taps: Vec<f32>,
    /// Which orbits put a nonzero sample into their tap: only those are
    /// worth a stem file.
    sounded: [bool; MAX_ORBITS],
}

impl Stems {
    fn new() -> Self {
        Self {
            taps: vec![0.0; MAX_ORBITS * crate::reverb::REVERB_BLOCK * 2],
            sounded: [false; MAX_ORBITS],
        }
    }

    /// One orbit's taps for the last block rendered: `frames` interleaved
    /// stereo frames. Past the block's end the taps are not the render's.
    pub fn orbit(&self, orbit: usize, frames: usize) -> &[f32] {
        let span = frames.min(crate::reverb::REVERB_BLOCK) * 2;
        let from = orbit * crate::reverb::REVERB_BLOCK * 2;
        &self.taps[from..from + span]
    }

    /// Which orbits have made sound this render.
    pub fn sounded(&self) -> &[bool; MAX_ORBITS] {
        &self.sounded
    }

    /// Zero the taps for the block in hand: a frame an orbit did not sound
    /// must read as silence, not as the last block again.
    fn start_block(&mut self) {
        self.taps.fill(0.0);
    }

    /// A reused backend starts no render with an old verdict on an orbit.
    fn reset(&mut self) {
        self.sounded = [false; MAX_ORBITS];
    }

    /// The orbit's finished stereo for one frame of the block in hand.
    fn tap(&mut self, orbit: usize, frame: usize, left: f32, right: f32) {
        if frame >= crate::reverb::REVERB_BLOCK {
            return;
        }
        let at = (orbit * crate::reverb::REVERB_BLOCK + frame) * 2;
        self.taps[at] = left;
        self.taps[at + 1] = right;
        if left != 0.0 || right != 0.0 {
            self.sounded[orbit] = true;
        }
    }
}

/// The level, the place and the sends of an instrument, from its last note.
/// A note that changes one of them while the instrument sounds moves the
/// value in a short ramp: a step is a click on the notes before.
#[derive(Clone, Copy, Default)]
struct InstrumentGain {
    key: crate::insert::InsertKey,
    gain: crate::live_control::Ramp,
    postgain: crate::live_control::Ramp,
    reverb_wet: crate::live_control::Ramp,
    delay_wet: crate::live_control::Ramp,
    dry: crate::live_control::Ramp,
    filters: crate::FilterControls,
    cutoff: crate::live_control::Ramp,
    resonance: crate::live_control::Ramp,
    pan: Option<f32>,
    /// The place of the pan from 0 to 1. A note with no pan has the
    /// middle, where the pan changes nothing.
    pan_at: crate::live_control::Ramp,
    onset: u64,
    duration_secs: f32,
    /// The silent frames of the instrument in a row, up to now.
    quiet: u16,
}

struct InstrumentGainChange {
    bus: usize,
    frame: usize,
    value: InstrumentGain,
}

#[derive(Clone, Copy)]
struct InsertPostgain {
    owner: (u64, u64),
    value: f32,
    /// What the level still has of the level before, after a new owner: the
    /// level of the chain then moves to the new value and does not step.
    offset: crate::live_control::Ramp,
    /// The level of the last frame.
    applied: f32,
    /// A note or a reset set the level. A new owner then moves the level
    /// from it while the chain sounds.
    owned: bool,
    /// The silent frames of the chain output in a row, up to the block
    /// before.
    quiet: u16,
    /// The frames in a row with no sound of a voice into the chain, up to
    /// the frame before. The output of a block is known only after the
    /// block, and a note at the start of a block is known at once.
    unfed: u16,
}

/// The levels of the copies of one slot. A stage fades in or out with the
/// level of its active copy, and a copy that leaves the slot fades out
/// beside the copy that takes its place.
#[derive(Clone, Copy)]
pub(crate) struct InsertMix {
    /// The level of the active copy: 1 at rest, 0 at the end of a stage
    /// that leaves the order.
    active: crate::live_control::Ramp,
    /// The level of the copy in the leaving place: 0 at rest.
    leaving: crate::live_control::Ramp,
}

/// The scalar renderer: voices, modulators, effects and orbit mixing,
/// computed one sample at a time.
#[derive(Default)]
pub struct ScalarBackend {
    #[allow(clippy::vec_box)]
    kabel_programs: Vec<Box<crate::assets::PreparedKabelProgram>>,
    kabel_program_protected: Vec<bool>,
    kabel_queued: bool,
    #[allow(clippy::vec_box)]
    kabel_pool: Vec<Box<rustel_kabel::Graph>>,
    kabel_bytes: usize,
    kabel_count: usize,
    missing_kabel_events: u64,
    /// The custom waves of notes given to [`AudioBackend::note`], leased as
    /// the notes come due. A live callback takes notes from the ring, which
    /// holds their waves instead.
    wave_leases: crate::partials::WaveLeases,
    /// A noted note waits for its wave, or a leased wave waits for release.
    leasing_waves: bool,
    max_polyphony: PolyphonyLimit,
    sample_rate: u32,
    sample_resampling_mode: SampleResamplingMode,
    frame: u64,
    dispatch: DspDispatch,
    pending: PendingEvents,
    #[cfg(feature = "device-audio")]
    pub(crate) confirmations: Option<crate::confirmation::ConfirmationConsumer>,
    voices: Vec<Voice>,
    preview_epoch: u64,
    /// Recent exact targets also correct prefetched onsets. The producer's
    /// ordinary requery replaces the horizon; sounding voices own their ramps.
    live_control_targets: Vec<crate::live_control::LiveControlUpdate>,
    /// The values sliders moved, one for each parameter of a copy, for the
    /// notes in the queue: see [`Self::set_insert_value`].
    insert_values: Vec<crate::insert::InsertValue>,
    tables: Option<Arc<PeriodicWaveTables>>,
    /// SBD's WaveShaper curve, allocated when the backend is prepared and only
    /// read by the audio callback.
    sbd_saturation_curve: Option<Box<[f32]>>,
    bank: Option<SampleBank>,
    /// Events dropped because their sample id had nothing installed yet -
    /// a "loading took too long" skip, never a fatal error.
    missing_sample_events: u64,
    /// One shared delay line per orbit, allocated at init (never in the
    /// callback); `active` orbits are the only ones mixed.
    orbit_delays: Vec<OrbitDelay>,
    orbit_ducks: Vec<OrbitDuck>,
    /// Duck triggers awaiting their ARM time (the 10 ms pre-hold point).
    /// Intake pushes here - for an offline render that is ALL events up
    /// front, and arming immediately would leave only the last automation
    /// generations alive. Each render block arms exactly the triggers whose
    /// hold point it contains, so order and timing match the live path.
    pending_ducks: Vec<PendingDuck>,
    orbit_reverbs: Vec<Option<Box<crate::reverb::OrbitReverb>>>,
    /// The outside effects and the instrument of each chain. The notes that
    /// ask for an effect feed `insert_blocks`, and the output of the chain
    /// joins the orbit.
    orbit_inserts: Vec<Option<Box<dyn crate::insert::OrbitInsert>>>,
    /// Live inserts wait for a matching onset before replacing the active copy.
    prepared_inserts: Vec<Option<Box<dyn crate::insert::OrbitInsert>>>,
    /// The copy a slot gives up while its stage is heard: it fades out
    /// beside the copy that takes its place, then goes back.
    leaving_inserts: Vec<Option<Box<dyn crate::insert::OrbitInsert>>>,
    /// Displaced copies wait in this bounded list for return to the producer.
    retired_inserts: Vec<Box<dyn crate::insert::OrbitInsert>>,
    insert_blocks: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// For each chain: the input of a stage whose levels move, and the block
    /// of the copy in its leaving place. See [`InsertMix`].
    insert_dry: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    insert_leaving_blocks: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Each submitted instrument keeps its own block until collection.
    instrument_blocks: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Stage instrument sends so worker collection cannot reorder orbit sums.
    instrument_reverb_sends: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    instrument_delay_sends: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// The latest onset owns the chain level, including its remaining tail.
    insert_postgains: Vec<InsertPostgain>,
    insert_postgain_blocks: Vec<[f32; crate::reverb::REVERB_BLOCK]>,
    /// Output orbit of each physical insert bus, selected by its latest onset.
    insert_outputs: Vec<usize>,
    insert_chains: Vec<Option<u64>>,
    insert_last_used: Vec<u64>,
    /// The chains the host names: see [`Self::set_named_insert_chains`].
    /// List 0 is no list.
    insert_named: Vec<u64>,
    insert_named_list: u64,
    /// The frame a chain with no note starts its fade at. `u64::MAX` is no
    /// fade.
    insert_fade_from: Vec<u64>,
    insert_refusals: u64,
    /// A chain the host names found no free bus since the last reclaim: see
    /// [`Self::reclaim_insert_chains`].
    insert_bus_wanted: bool,
    /// Frames each insert had no input and no output. A long idle insert
    /// sleeps until a note asks for the effect again.
    insert_idle_frames: Vec<u32>,
    /// The stages of each chain that run, in signal order, with
    /// [`crate::insert::NO_STAGE`] after the last: the stages that served
    /// the newest note with an effect, in the order of its calls, and each
    /// stage that fades out in its place.
    effect_order: Vec<[u8; crate::insert::EFFECT_CHAIN]>,
    /// The levels of the copies of each slot: see [`InsertMix`].
    insert_mixes: Vec<InsertMix>,
    /// The generation of the newest note of each chain. An older note
    /// changes no copy and no order.
    insert_generations: Vec<u64>,
    /// The chains with a fade that has not ended, one bit for each bus: see
    /// [`Self::finish_insert_fades`].
    insert_fading: u64,
    /// The latest instrument note sends its output through the chain effects.
    instrument_to_effect: Vec<bool>,
    /// A shared instrument's latest onset controls its output and tail.
    instrument_gains: Vec<InstrumentGain>,
    instrument_filters: Vec<[FilterChain; 2]>,
    /// At most one change per accepted onset, reserved before rendering.
    instrument_gain_changes: Vec<InstrumentGainChange>,
    /// Offline only: builds a missing insert at activation, as the inline
    /// reverb does.
    insert_provider: Option<Arc<crate::insert::InsertProvider>>,
    /// Notes that asked for an insert their chain did not hold.
    missing_insert_events: u64,
    /// The plugin workers of the output. A chain of inserts that run on any
    /// thread posts its blocks to them as jobs. Live only.
    plugin_pool: Option<crate::pool::PoolHandle>,
    /// Per-orbit stereo accumulation for one ≤128-frame sub-block: the
    /// pre-duck orbit signal (voices + delay + reverb returns).
    orbit_blocks: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Highest absolute sample each orbit put into the mix since the peaks
    /// were last taken: the orbit meters.
    orbit_peaks: [f32; MAX_ORBITS],
    /// Each orbit's fader, linear, applied after its duck and before the
    /// sum: the mixer's strips. Unity until the mixer says otherwise.
    orbit_gains: OrbitGains,
    /// Which output pair each orbit goes to: 0 is the main pair the mix
    /// is written to, higher pairs are kept apart in `orbit_mix`.
    orbit_pairs: [u8; MAX_ORBITS],
    /// Per-orbit stereo for the orbits not on the main pair, interleaved,
    /// `MAX_ORBITS` blocks of `ORBIT_MIX_FRAMES * 2`. Only written while
    /// some orbit is routed away.
    orbit_mix: Vec<f32>,
    /// The orbit taps of a stems render, when one asked for them. `None`
    /// costs the mix loop one register flag.
    stems: Option<Stems>,
    /// Per-visual dry stereo sums for the current live render quantum.
    /// These are post-voice-DSP and pre-orbit: shared delay, reverb, ducking,
    /// DJF and orbit gain cannot be assigned to one receiver without mixing
    /// sibling voices back together. Only enabled bits are cleared and written.
    ui_visual_mix: Vec<[f32; UI_VISUAL_MIX_FRAMES * 2]>,
    ui_visual_capture_mask: u64,
    ui_visual_capture_generation_floor: u64,
    /// Per-orbit stereo reverb SEND accumulation (post-pan, scaled by wet).
    reverb_sends: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Offline renders may synthesise a missing orbit reverb at activation
    /// time; the LIVE path must never (producer installs instead).
    allow_inline_reverb: bool,
    /// An offline render drives the blocks, and waits for an insert that
    /// renders ahead to publish each block before collecting it. The
    /// live callback paces itself in real time and never waits.
    offline: bool,
    /// Onsets whose orbit reverb was not installed yet: the voice plays DRY
    /// this time, like a still-loading skip.
    missing_reverb_events: u64,
    /// Onsets dropped at the concurrent-voice ceiling.
    voice_ceiling_drops: u64,
    /// Onsets dropped because the pending queue was already at its ceiling.
    pending_ceiling_drops: u64,
    /// Per-orbit DJ filter - created by the first `djf` trigger and sticky
    /// for the orbit's lifetime.
    orbit_djf: Vec<DjfState>,
    /// Where each ORBIT's output lands, keyed by the FIRST voice that ever
    /// triggers into it: the destination is wired once, at orbit creation,
    /// so the first voice's `channels` - or the plain stereo default when
    /// it has none - routes the whole orbit for its entire life, sends
    /// included, and every later voice's `channels` is INERT. `None` is an
    /// orbit nothing has triggered into yet.
    orbit_channels: Vec<Option<[u8; 2]>>,
    next_voice_id: u64,
    /// Independent, fixed release capacity: computer piano has 15 keys.
    /// Key-ups must still be admitted when the voice/pending pools are full.
    pending_chokes: [Option<(u64, u32)>; 16],
    /// Mutable FM operator state, leased only by voices that use FM. The pool
    /// keeps every activation allocation-free without making ordinary voices
    /// stride over phases and noise generators they never read.
    #[allow(clippy::vec_box)]
    fm_state_pool: Vec<Box<FmState>>,
    /// Pre-allocated compressor states (10 KB rings): leased to voices at
    /// activation, returned at retire. Empty pool → the voice plays
    /// uncompressed rather than allocating on the audio thread.
    compressor_pool: Vec<Box<CompressorState>>,
    /// Per-voice brickwalls for `.limit()`, leased at activation and returned
    /// at retire, one per voice the engine can sound at once so that a voice
    /// asking for a ceiling always gets one.
    ///
    /// Boxed for the reason `fm_state_pool` is boxed, not for the reason
    /// clippy's threshold assumes: the state is small - a four-frame ring
    /// rather than a kilobyte one - but it lives in `Voice` as
    /// `Option<Box<_>>`, and `Voice` is the struct the render loop strides
    /// over every frame for every voice. Keeping the rings off that stride is
    /// worth a pointer, which is why the lint is answered rather than obeyed.
    #[allow(clippy::vec_box)]
    limiter_pool: Vec<Box<crate::limiter::InlineLimiter>>,
    /// Leased by `.FX()` stages that name a delay. Allocated at init: a
    /// 1-second stereo line is 384 KiB at 48 kHz.
    fx_delay_pool: Vec<FxDelayLine>,
    /// `zdelay` history rings, two seconds each, leased by ZzFX voices that
    /// name a delay and returned when the voice ends. Allocated at init
    /// like every other pool; a `zdelay` past two seconds is clamped by the
    /// ring's own wrap.
    zzfx_ring_pool: Vec<Box<[f32]>>,
    /// Audio buses, as `.bus(n)` fills them and `.bmod({b: n})` reads them.
    ///
    /// Two copies, because a bus is read by voices in the same sample it is
    /// written by others and the voice list has no meaningful order. Receivers
    /// read what the previous sample produced, so the result does not depend
    /// on which voice happens to come first. One sample is 20 us at 48 kHz -
    /// far below anything audible, and the alternative is either an ordering
    /// that changes with the pattern or a second pass over every voice.
    bus_now: [[f32; 2]; crate::backend::MAX_BUSES],
    /// The audio input, when a device has one open.
    input: Option<std::sync::Arc<crate::input::InputRing>>,
    /// Where the input voices read next, in the ring's own frames: the read
    /// position for output frame zero of the next block. The ring places it
    /// and can move it. It holds a fraction when the ring runs at another
    /// rate than the output.
    input_cursor: Option<f64>,
    /// Reverbs available to `.FX()` stages, each already carrying a generated
    /// impulse response. A stage's reverb is per VOICE and its IR depends on
    /// that hap's own roomsize, so unlike the orbit reverbs these cannot be a
    /// fixed set: they are generated OFF the audio thread and installed here.
    /// Generating one costs 2.7 ms at roomsize 0.5 and 31 ms at 6, against
    /// the 2.7 ms a 128-frame callback has to spend.
    fx_reverb_pool: Vec<Box<crate::reverb::OrbitReverb>>,
    /// Params a stage asked for and did not get, for the producer to
    /// synthesise and install. Bounded: a pattern sweeping `roomsize` would
    /// otherwise queue one per hap forever.
    fx_reverb_wanted: Vec<crate::reverb::ReverbParams>,
    /// Bytes admitted through `install_fx_reverb`, including leased boxes.
    fx_reverb_bytes: usize,
    /// Offline inline generation retains its separate allocation policy.
    /// Each created box reserves an extra return slot before being leased.
    fx_reverb_inline_count: usize,
    /// Phase vocoders leased to voices / `.FX()` stages that name `stretch`.
    /// Built at init with shared FFT plans: activation must never run
    /// `FftPlanner` or allocate the analysis buffers on the audio thread.
    ///
    /// The `Box` is required. `Voice::stretch` is an
    /// `Option<Box<[Stretch; 2]>>`, so a boxed pool element moves into the
    /// voice as a pointer. Clippy's `Vec<[Stretch; 2]>` would return an
    /// array by value that needs `Box::new` to store: one heap allocation
    /// per activation on the audio thread, which this pool exists to avoid.
    #[allow(clippy::vec_box)]
    stretch_pool: Vec<Box<[crate::stretch::Stretch; 2]>>,
    /// Empty containers for the rare per-voice `.FX()` state. Keeping the
    /// 6.7 KiB array inline made every ordinary voice carry and stride over
    /// it. Containers are allocated on the producer side and leased here.
    #[allow(clippy::vec_box)]
    fx_stage_state_pool: Vec<Box<FxStageStates>>,
    /// A rewind's cut (a from-zero reload), armed by
    /// [`Self::begin_takeover_cut`] and expired once a block starts past its
    /// frame. The choke ramp makes the silence click-free.
    ///
    /// The pair is (cut frame, OUTGOING generation). The frame is where the
    /// outgoing rendition falls silent: the flip block for an immediate
    /// rewind (its ghost window - onsets scheduled between the flip and the
    /// takeover, a restarted loop's own first beat - must not sound, and the
    /// consumer drops those from the ring), the takeover frame for a
    /// quantised one (the countdown plays to its line), or the line itself
    /// when a pre-armed line cut fires before any flip. An edit's flip that
    /// lands before the frame (a control requery inside a quantised
    /// rewind's head-room) leaves the arm in place: the generation guard
    /// keeps the pre-fade off every generation but the one it names, and a
    /// countdown onset that activates after the edit's flip still has to
    /// stop at the line. That generation's onsets before the frame are kept
    /// through the edit's flip too, pending and in the ring, whatever the
    /// edit's own takeover (see [`Self::retire_pending_keeping_countdown`]).
    ///
    /// This is NOT the audition verb (`cut_sounding`), which silences
    /// immediately: the takeover cut is click-free over the choke ramp,
    /// and nothing of the new generation is touched. Voices constructed
    /// from pending events of the outgoing generation are pre-faded at
    /// activation by the same contract.
    pub(crate) armed_takeover_cut: Option<(u64, u64)>,
    /// Set while voices can stand in for events of the newest generation:
    /// (that generation, the block start its flip landed on, the last frame
    /// such an event can aim at). See [`Self::keep_started_onsets`].
    kept_onsets: Option<(u64, u64, u64)>,
    pressure: ScalarPressureState,
}

impl ScalarBackend {
    /// Select how this backend reads fractional positions in sample voices.
    /// The mode is sampled once per render block and does not allocate.
    pub fn set_sample_resampling_mode(&mut self, mode: SampleResamplingMode) {
        self.sample_resampling_mode = mode;
    }

    pub fn sample_resampling_mode(&self) -> SampleResamplingMode {
        self.sample_resampling_mode
    }

    /// Musical voice limit, excluding voices already in their retirement fade.
    pub fn max_polyphony(&self) -> usize {
        self.max_polyphony.0
    }

    /// Update this renderer's limit without allocating or resetting playback.
    /// Lowering it fades the oldest excess voices over a quarter second.
    pub fn set_max_polyphony(&mut self, limit: usize) {
        let limit = limit.clamp(1, MAX_CONFIGURABLE_POLYPHONY);
        if limit == self.max_polyphony.0 {
            return;
        }
        self.max_polyphony.0 = limit;
        let active = self
            .voices
            .iter()
            .filter(|voice| voice.polyphony_fade_frame.is_none())
            .count();
        let faded = Self::fade_oldest_voices(
            &mut self.voices,
            active.saturating_sub(limit),
            self.frame,
            self.sample_rate,
        );
        self.pressure.semantic_polyphony_fades = self
            .pressure
            .semantic_polyphony_fades
            .saturating_add(faded as u64);
    }

    /// Change a proven continuous binding without retriggering any voice.
    pub fn set_live_control(&mut self, update: crate::live_control::LiveControlUpdate) {
        if update.binding == 0 || !update.value.is_finite() {
            return;
        }
        for voice in &mut self.voices {
            voice.live_gain.retarget(update, self.sample_rate);
            voice.live_cutoff.retarget(update, self.sample_rate);
            voice.live_resonance.retarget(update, self.sample_rate);
            voice.live_fm_index.retarget(update, self.sample_rate);
        }
        for instrument in &mut self.instrument_gains {
            instrument.gain.retarget(update, self.sample_rate);
            instrument.cutoff.retarget(update, self.sample_rate);
            instrument.resonance.retarget(update, self.sample_rate);
        }
        if let Some(target) = self
            .live_control_targets
            .iter_mut()
            .find(|target| target.binding == update.binding)
        {
            *target = update;
        } else if self.live_control_targets.len() < self.live_control_targets.capacity() {
            self.live_control_targets.push(update);
        } else if !self.live_control_targets.is_empty() {
            // Bounded history: even an exceptionally large score cannot make
            // a mouse update allocate inside the callback. Existing voices
            // already received the update above; new queries read the cell.
            self.live_control_targets.rotate_left(1);
            *self.live_control_targets.last_mut().unwrap() = update;
        }
    }

    /// Moves a parameter value of an insert: see
    /// [`crate::insert::InsertValue`]. The copy in the slot of the value
    /// takes it at once, when the copy has the key of the value. Each note
    /// of the generation of the value or older takes it too. A new move of a
    /// parameter takes the place of the move before and goes last. The
    /// backend keeps the last moves of [`MAX_INSERT_VALUES`] values. A full
    /// table drops the oldest move. A value that is not finite goes nowhere.
    pub fn set_insert_value(&mut self, value: crate::insert::InsertValue) {
        if !value.param.value.is_finite() {
            return;
        }
        let held = |chain: &Option<u64>| *chain == Some(value.slot.chain);
        let bus = value
            .slot
            .is_valid()
            .then(|| self.insert_chains.iter().position(held))
            .flatten();
        let copy = bus.and_then(|bus| self.orbit_inserts[value.slot.physical(bus)].as_mut());
        if let Some(copy) = copy.filter(|copy| copy.key() == value.key) {
            copy.set_param_now(value.param);
        }
        let same = |kept: &crate::insert::InsertValue| {
            (kept.slot, kept.key, kept.param.id) == (value.slot, value.key, value.param.id)
        };
        if let Some(at) = self.insert_values.iter().position(same) {
            // The new move goes last, so a full table drops the oldest move.
            self.insert_values[at..].rotate_left(1);
            *self.insert_values.last_mut().unwrap() = value;
        } else if self.insert_values.len() < self.insert_values.capacity() {
            self.insert_values.push(value);
        } else if !self.insert_values.is_empty() {
            // Bounded, so a move never allocates in the callback.
            self.insert_values.rotate_left(1);
            *self.insert_values.last_mut().unwrap() = value;
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a renderer with an immutable engine-kernel selection.
    pub fn with_dispatch(dispatch: DspDispatch) -> Self {
        Self {
            dispatch,
            ..Self::default()
        }
    }

    pub const fn dispatch(&self) -> DspDispatch {
        self.dispatch
    }

    /// Heap this backend wrote while it was prepared: the pools voices and
    /// `.FX()` stages lease from, filled up front so the callback never
    /// allocates, the saturation curve, and its own wave tables at a rate
    /// that does not share them. Room reserved and never written - voice
    /// and pending slots, delay lines, ZzFX rings, the orbit mix - is left
    /// out: zeroed or untouched, it takes memory only as voices reach it.
    /// Worked out on the thread that prepares the backend, before the
    /// callback owns it.
    pub fn prepared_bytes(&self) -> usize {
        use std::mem::size_of;
        let stretch: usize = self
            .stretch_pool
            .iter()
            .map(|pair| {
                size_of::<[crate::stretch::Stretch; 2]>()
                    + pair
                        .iter()
                        .map(crate::stretch::Stretch::heap_bytes)
                        .sum::<usize>()
            })
            .sum();
        let tables = self
            .tables
            .as_ref()
            .filter(|_| self.sample_rate != crate::periodic_wave::SHARED_TABLES_RATE)
            .map_or(0, |tables| tables.bytes());
        let curve = self
            .sbd_saturation_curve
            .as_deref()
            .map_or(0, std::mem::size_of_val);
        stretch
            + self.kabel_programs.capacity() * size_of::<Box<crate::assets::PreparedKabelProgram>>()
            + self.kabel_program_protected.capacity() * size_of::<bool>()
            + self.kabel_bytes
            + self.fx_stage_state_pool.len() * size_of::<FxStageStates>()
            + self.compressor_pool.len() * size_of::<CompressorState>()
            + self.fm_state_pool.len() * size_of::<FmState>()
            + self.limiter_pool.len() * size_of::<crate::limiter::InlineLimiter>()
            + curve
            + tables
    }

    /// The orbit meters: each orbit's highest absolute sample since the
    /// last take, reset by taking.
    /// Give input voices a ring to read; the device that opens the input
    /// fills it.
    pub fn set_input(&mut self, ring: Option<std::sync::Arc<crate::input::InputRing>>) {
        self.input = ring;
        self.input_cursor = None;
    }

    /// Keyboard voices have their own lifetime and cannot prolong a score stop.
    #[cfg(feature = "device-audio")]
    pub(crate) fn score_sources_active(&self) -> bool {
        self.voices.iter().any(|voice| !voice.piano)
            || self.pending.order.iter().any(|index| {
                self.pending.slots[*index]
                    .event
                    .as_ref()
                    .is_some_and(|event| !event.controls.piano)
            })
            || self
                .orbit_delays
                .iter()
                .any(|delay| delay.active && delay.energized != 0)
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn pressure_observation(&self) -> RealtimePressureObservation {
        RealtimePressureObservation {
            max_polyphony: self.max_polyphony() as u64,
            active_voices: u64::try_from(self.voices.len()).unwrap_or(u64::MAX),
            pending_events: u64::try_from(self.pending.len()).unwrap_or(u64::MAX),
            active_orbits: u64::try_from(
                self.pressure
                    .active_orbit_voices
                    .iter()
                    .filter(|voices| **voices != 0)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            active_pool_leases: self.pressure.active_pool_leases,
            active_orbit_delays: u64::try_from(
                self.orbit_delays
                    .iter()
                    // An allocated line can be silent between an onset and
                    // its first echo. Stored energy, rather than the sticky
                    // routing flag, says whether Stop still owes an echo.
                    .filter(|delay| delay.active && delay.energized != 0)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            active_orbit_reverbs: u64::try_from(
                self.orbit_reverbs
                    .iter()
                    .filter(|reverb| reverb.is_some())
                    .count(),
            )
            .unwrap_or(u64::MAX),
            active_dj_filters: u64::try_from(
                self.orbit_djf.iter().filter(|filter| filter.active).count(),
            )
            .unwrap_or(u64::MAX),
            semantic_polyphony_fades: self.pressure.semantic_polyphony_fades,
            voice_ceiling_drops: self.pressure.voice_ceiling_drops,
            pending_ceiling_drops: self.pressure.pending_ceiling_drops,
            pool_misses: self.pressure.pool_misses,
            orbit_reverb_misses: self.pressure.orbit_reverb_misses,
        }
    }

    pub fn install_sample(
        &mut self,
        id: SampleId,
        sample: Box<DecodedSample>,
    ) -> Result<Option<Box<DecodedSample>>, Box<DecodedSample>> {
        // Callable before `init`: preloading an offline render's samples
        // must survive the later init, which only tops up the bundled slot.
        self.bank
            .get_or_insert_with(SampleBank::empty)
            .install(id, sample)
    }

    /// Empty a live slot so unused preview PCM can be reclaimed. The
    /// bundled `bd` is left alone.
    pub fn clear_sample(&mut self, id: SampleId) -> Option<Box<DecodedSample>> {
        self.bank.as_mut()?.clear(id)
    }

    /// Events skipped because their sample was not installed at onset time.
    pub fn missing_sample_events(&self) -> u64 {
        self.missing_sample_events
    }

    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn prepared(sample_rate: u32, voice_capacity: usize) -> Result<Self, String> {
        Self::prepared_with_dispatch(sample_rate, voice_capacity, DspDispatch::automatic())
    }

    /// Preallocate the renderer without changing its selected kernels on init.
    pub fn prepared_with_dispatch(
        sample_rate: u32,
        voice_capacity: usize,
        dispatch: DspDispatch,
    ) -> Result<Self, String> {
        let mut backend = Self {
            dispatch,
            pending: PendingEvents::with_capacity(voice_capacity),
            voices: Vec::with_capacity(voice_capacity),
            ..Self::default()
        };
        backend.init(sample_rate)?;
        backend.ensure_fx_stage_state_pool(voice_capacity);
        Ok(backend)
    }

    /// Clear scheduled/active voices while retaining capacity and align the
    /// backend with an absolute live sample clock.
    pub fn reset_at(&mut self, frame: u64) {
        self.pending.clear();
        self.pending_chokes.fill(None);
        self.preview_epoch = 0;
        if let Some(stems) = self.stems.as_mut() {
            stems.reset();
        }
        self.live_control_targets.clear();
        self.insert_values.clear();
        while let Some(mut voice) = self.voices.pop() {
            retire_voice_resources(
                &mut voice,
                &mut self.pressure,
                &mut self.fm_state_pool,
                &mut self.compressor_pool,
                &mut self.limiter_pool,
                &mut self.fx_delay_pool,
                &mut self.fx_reverb_pool,
                &mut self.zzfx_ring_pool,
                &mut self.stretch_pool,
                &mut self.fx_stage_state_pool,
                &mut self.kabel_pool,
            );
        }
        self.pressure.clear_active();
        self.kept_onsets = None;
        // Automation does not survive a clock reset: the graph builds new
        // gain curves and never replays an old AudioParam timeline.
        for duck in &mut self.orbit_ducks {
            *duck = OrbitDuck::default();
        }
        self.pending_ducks.clear();
        for reverb in self.orbit_reverbs.iter_mut().flatten() {
            reverb.reset();
        }
        // Each fade ends now: the copy in a leaving place goes back, and a
        // stage that fades out leaves the order. A copy with no place in the
        // list of the copies that go back waits for the next stopped block.
        let mut buses = self.insert_fading;
        while buses != 0 {
            let bus = buses.trailing_zeros() as usize;
            buses &= buses - 1;
            for stage in 0..=crate::insert::EFFECT_CHAIN {
                let mix = &mut self.insert_mixes[stage * crate::insert::MAX_INSERT_BUSES + bus];
                *mix = InsertMix {
                    active: crate::live_control::Ramp::new(0, mix.active.target()),
                    ..InsertMix::default()
                };
            }
        }
        self.finish_insert_fades();
        self.insert_generations.fill(0);
        for insert in self.orbit_inserts.iter_mut().flatten() {
            insert.reset();
        }
        for (bus, output) in self.insert_outputs.iter_mut().enumerate() {
            *output = bus.min(MAX_ORBITS - 1);
        }
        self.insert_last_used.fill(frame);
        self.insert_fade_from.fill(u64::MAX);
        // Each chain is silent until a note owns it again. Some inserts keep a
        // tail through their reset. The level of 0 keeps that tail silent
        // after a resume. The next note of the chain moves the level up from
        // 0, and the kept tail then sounds under that note.
        for gain in &mut self.instrument_gains {
            gain.gain = crate::live_control::Ramp::new(0, 0.0);
            // An instrument that feeds effects sets the level after them.
            gain.postgain = crate::live_control::Ramp::new(0, 0.0);
        }
        self.instrument_filters.fill([FilterChain::default(); 2]);
        // No insert runs while the output is stopped, so each chain keeps its
        // count of silent frames.
        for gain in &mut self.insert_postgains {
            *gain = InsertPostgain::silent(gain.quiet);
        }
        self.instrument_gain_changes.clear();
        for bus in &mut self.orbit_delays {
            // Skip inactive lines: after the stop ramp reset_at runs on every
            // stopped block, and re-zeroing 16 one-second stereo lines would
            // write 6 MB per block.
            if bus.active {
                bus.left.fill(0.0);
                bus.right.fill(0.0);
                bus.active = false;
                bus.energized = 0;
            }
        }
        self.frame = frame;
    }
}
