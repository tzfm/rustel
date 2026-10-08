//! Backend trait and typed onset parameters shared by live and offline audio.

/// Oscillator shapes of the built-in synth family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Waveform {
    #[default]
    Sine,
    Triangle,
    Square,
    Sawtooth,
}

/// Resolved linear amplitude envelope. The runtime resolves the
/// presence-sensitive defaults before this reaches a real-time boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    pub attack_secs: f32,
    pub decay_secs: f32,
    pub sustain: f32,
    pub release_secs: f32,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            attack_secs: 0.001,
            decay_secs: 0.05,
            sustain: 0.6,
            release_secs: 0.01,
        }
    }
}

/// Base cutoff and resonance for one biquad stage, before modulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticBiquad {
    pub frequency_hz: f32,
    pub q: f32,
}

/// Per-orbit feedback-delay send: the voice sends `wet` of its pre-pan signal
/// into its orbit's shared delay line, whose time/feedback are updated per
/// trigger. Present iff `delay > 0 && delaytime > 0 && delayfeedback > 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DelayControls {
    pub wet: f32,
    /// Clamped to the delay line's fixed 1-second capacity.
    pub time_secs: f32,
    /// Clamped 0..=0.98 (the feedback ceiling).
    pub feedback: f32,
}

/// Resolved exponential frequency envelope used by the filters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterEnvelope {
    pub attack_secs: f32,
    pub decay_secs: f32,
    /// Kept in f64 with the bounds so the decay target can cancel to zero;
    /// see [`FilterEnvelope::min_hz`].
    pub sustain: f64,
    pub release_secs: f32,
    /// The decay target is `min + sustain * (max - min)`. Pitch envelopes
    /// can make this cancel to exactly zero, which the envelope evaluator
    /// replaces with 0.001 for an exponential ramp. Narrowing the bounds or
    /// sustain to f32 can leave a small nonzero residue and bypass that
    /// replacement, changing the sweep.
    pub min_hz: f64,
    pub max_hz: f64,
}

/// The `ftype` filter model: '12db' (one biquad stage), '24db' (two), or
/// 'ladder' (the 4-pole tanh ladder, which replaces the biquad for every
/// enabled section).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterStages {
    #[default]
    One,
    Two,
    Ladder,
}

/// Presence-sensitive filter chain resolved by the runtime: installs
/// low-pass, high-pass, and band-pass filters in that order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterControls {
    pub lowpass: Option<StaticBiquad>,
    pub lowpass_envelope: Option<FilterEnvelope>,
    pub highpass: Option<StaticBiquad>,
    pub highpass_envelope: Option<FilterEnvelope>,
    pub bandpass: Option<StaticBiquad>,
    pub bandpass_envelope: Option<FilterEnvelope>,
    pub stages: FilterStages,
    /// Ladder-model input drive (the `drive` control, default 0.69); the
    /// DSP applies `clamp(exp(drive), 0.1, 2000)`.
    pub drive: f32,
}

impl Default for FilterControls {
    fn default() -> Self {
        Self {
            lowpass: None,
            lowpass_envelope: None,
            highpass: None,
            highpass_envelope: None,
            bandpass: None,
            bandpass_envelope: None,
            stages: FilterStages::default(),
            drive: 0.69,
        }
    }
}

/// Plain oscillator/control data shared by offline and live scalar rendering.
/// `pan: None` preserves the distinction between a mono source
/// upmix and an explicitly inserted centered stereo panner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OscillatorControls {
    /// Host-only direct slider bindings: gain, then lowpass cutoff. Zero is
    /// unbound. Tokens never cross score evaluations or arbitrary value maps.
    pub live_controls: [u64; 2],
    /// Host preview identity. Zero belongs to the score; a newer nonzero
    /// identity replaces older preview voices without touching score voices.
    pub preview_epoch: u64,
    /// Host-only note release: apply `cut` without admitting a new voice.
    /// Scores cannot set this through resolved controls.
    pub choke_only: bool,
    /// Host-owned keyboard audition, independent of score generations.
    pub piano: bool,
    /// `noise` - pink noise crossfaded with the oscillator before the
    /// envelope. Zero skips the mix entirely and leaves the oscillator
    /// untouched.
    pub noise: f32,
    pub waveform: Waveform,
    pub envelope: Envelope,
    /// Begin time in seconds for LFO, pulse, wavetable, supersaw and bytebeat
    /// gates.
    ///
    /// These gates compare an f64 render-quantum time with a begin time
    /// rounded to f32, preserving the original AudioParam timing semantics.
    /// Rounding can put the begin time on either side of a quantum boundary,
    /// so retaining this precision difference affects which quantum starts.
    /// The transient shaper uses a separate frame-based gate.
    pub worklet_begin_secs: f32,
    /// Note end plus modulator release, rounded the same way.
    pub lfo_end_secs: f32,
    /// Note hold end, rounded the same way; a filter's own LFO stops before
    /// the release tail.
    pub filter_lfo_end_secs: f32,
    /// How long a modulator outlives the note, in seconds. The resolver uses
    /// the raw `release` control (default 0.01), extended by `FXrelease`.
    /// This is separate from the amplitude envelope's release, whose
    /// source-specific defaults and 0.01-second floor can produce a
    /// different duration.
    pub modulator_release_secs: f32,
    pub velocity: f32,
    pub postgain: f32,
    pub pan: Option<f32>,
    pub filters: FilterControls,
    /// Per-voice waveshaper (post-filter, pre-pan) when the
    /// `distort` control is present.
    pub distort: Option<crate::distortion::DistortControls>,
    /// Per-orbit feedback-delay send, when the `delay` control is present.
    pub delay: Option<DelayControls>,
    /// Sidechain: this voice's onset ducks ANOTHER orbit's output gain.
    pub duck: Option<DuckControls>,
    /// `room` - the orbit's generated-IR convolution reverb send.
    pub reverb: Option<ReverbControls>,
    /// `dry` - a gain on the voice's direct path into its orbit. The delay
    /// and reverb sends tap the chain separately, so lowering this thins the
    /// unprocessed signal while the wet paths keep their own levels.
    /// `None` is the ordinary full-strength path.
    pub dry: Option<f32>,
    /// `stretch` - the phase vocoder's pitch factor. Present only when the
    /// pattern asked for it; the vocoder is expensive and is not in the chain
    /// otherwise.
    pub stretch: Option<f32>,
    /// `fm`/`fmi` + `fmh` - the simple FM chain on the frequency path.
    pub fm: Option<FmControls>,
    /// Orbit index (default 1); selects the shared delay bus.
    pub orbit: u8,
    /// `lfo()` modulators: each rides an
    /// AudioParam additively. Fixed capacity keeps the ring event POD; the
    /// resolver skips-and-logs beyond it.
    pub lfos: [Option<LfoMod>; MAX_VOICE_MODS],
    /// Modulators reading another pattern's audio off a bus.
    pub bus_mods: [Option<BusMod>; MAX_VOICE_MODS],
    /// `bus` - which bus this voice sends its output into, if any.
    pub bus: Option<u8>,
    /// `busgain` - the send level into that bus (default 1).
    pub busgain: f32,
    /// `channels` - where each source channel lands on the output, as
    /// `destination + 1` so that 0 means "dropped". `None` is the plain
    /// stereo default.
    ///
    /// The control resolves to 0-based destinations, and source channel `i`
    /// is wired to `channels[i]`, so a source channel with no entry is
    /// dropped, an output nobody names stays silent, and two entries naming
    /// one output sum there. The index wraps on the render's channel count.
    /// All measured against Chromium on `s("bd").bank("tr909")`, left/right
    /// rms:
    ///
    /// ```text
    ///   (none)     0.092521  0.092374      "2:1"      0.092374  0.092521
    ///   channels(1)0.092521  0.000000      "1:1"      0.184855  0.000000
    ///   channels(2)0.000000  0.092521      "3:4"      0.092521  0.092374
    ///   channels(3)0.092521  0.000000      "1:2:3"    0.092521  0.092374
    /// ```
    pub channels: Option<[u8; 2]>,
    /// `env({...})` modulators, same contract.
    pub envs: [Option<EnvMod>; MAX_VOICE_MODS],
    /// `phaser` - post-pan notch whose detune rides a tri LFO.
    pub phaser: Option<PhaserControls>,
    /// `tremolo`/`tremolosync` - amplitude LFO into a gain stage after the
    /// waveshaper.
    pub tremolo: Option<TremoloControls>,
    /// `vowel` - 5 parallel bandpass formants + x8 makeup.
    pub vowel: Option<VowelControls>,
    /// `coarse` - block-local sample-and-hold.
    pub coarse: Option<f32>,
    /// `crush` - bit depth reduction.
    pub crush: Option<f32>,
    /// `shape` + `shapevol` - the soft-clip waveshaper.
    pub shape: Option<ShapeControls>,
    /// `vib`/`vibmod` - sine on the source's detune param, phase 0 at the
    /// note start.
    pub vibrato: Option<VibratoControls>,
    /// `penv` family - an ADSR on the detune param in cents;
    /// reuses FilterEnvelope's fields with
    /// min_hz/max_hz holding min/max cents.
    pub pitch_env: Option<PitchEnvControls>,
    /// `djf` - sets the trigger orbit's sticky DJ-filter value.
    pub djf: Option<f32>,
    /// `compressor` family - per-voice dynamics compressor between
    /// tremolo and the panner.
    pub compressor: Option<CompressorControls>,
    /// `limit` - a brickwall in line on this voice, after its post-gain
    /// and before anything that taps it. It is a creative effect, not output
    /// protection: it limits one voice, and ten voices each under the
    /// ceiling can still sum above it.
    pub limit: Option<crate::meter::LimiterSettings>,
    /// `transient` + `transsustain` - the transient shaper, last of the
    /// effects before the gain stage.
    pub transient: Option<TransientControls>,
    /// `.FX(...)` stages, run in order BEFORE this voice's own params.
    pub fx_stages: [Option<FxStage>; MAX_FX_STAGES],
    /// `partials`/`phases` (or `n` on a synth sound): a custom PeriodicWave
    /// built from the base waveform's Fourier terms.
    pub partials: Option<PartialsControls>,
}

/// Transient shaper: two envelope followers per channel whose difference
/// says how "peaky" the signal is right now, driving a gain that emphasises
/// attacks or sustains, with an averaged makeup and a soft clip.
///
/// Everything else it takes - attack/sustain times, sensitivity, mix - is
/// fixed at the shaper's own defaults; only these two are passed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransientControls {
    /// `transient`, clamped -1..1: above zero emphasises attacks.
    pub attack: f32,
    /// `transsustain`, clamped -1..1.
    pub sustain: f32,
}

/// Resolved additive-synthesis coefficients: `x(φ) = norm · Σ real[k]·cos
/// (2π(k+1)φ) + imag[k]·sin(2π(k+1)φ)`, matching Web Audio
/// `createPeriodicWave`'s normalized output. Partials whose frequency would
/// cross Nyquist are skipped at render time (band-limited playback).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartialsControls {
    pub real: [f32; MAX_PARTIALS],
    pub imag: [f32; MAX_PARTIALS],
    pub len: u8,
    /// 1 / peak of one cycle (createPeriodicWave normalizes by default).
    pub norm: f32,
}

pub const MAX_PARTIALS: usize = 32;

/// DynamicsCompressorNode parameters (defaults: threshold −3,
/// ratio 10, knee 10, attack 0.005, release 0.05).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressorControls {
    pub threshold_db: f32,
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_secs: f32,
    pub release_secs: f32,
}

/// Vibrato oscillator: `detune += sin(2π·freq·t)·cents` (vibmod·100).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VibratoControls {
    pub freq_hz: f32,
    pub cents: f32,
}

/// Pitch envelope: min = −cents·panchor, max = cents − cents·panchor with
/// cents = penv·100; ADSR defaults [0.2, 0.001, 1, 0.001]; pcurve selects
/// linear (default) or exponential ramps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchEnvControls {
    pub adsr: FilterEnvelope,
    pub exponential: bool,
}

/// Vowel filter: the chain signal feeds 5 parallel bandpass biquads
/// (linear Q), each scaled by its formant gain, summed into an x8 makeup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VowelControls {
    pub freqs: [f32; 5],
    pub gains: [f32; 5],
    pub qs: [f32; 5],
}

/// ShapeProcessor: `k = 2s/(1-s)` with s capped just below 1, transfer
/// `(1+k)·x / (1 + k·|x|)`, output scaled by `clamp(shapevol, 0.001, 1)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeControls {
    pub shape: f32,
    pub postgain: f32,
}

/// Phaser: one notch biquad at `center + 282` Hz (a single stage; the 282
/// offset is historical and part of the sound), Q = `2 − clamp(2·depth, 0,
/// 1.9)`, whose detune param (cents) rides a default tri LFO clamped to
/// ±sweep. The LFO anchors to the note's context begin, so phase0 =
/// ffrac(begin·rate) - see `time_secs`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaserControls {
    pub rate_hz: f32,
    /// phaserdepth (default 0.75); > 0 or the phaser is skipped entirely.
    pub depth: f32,
    /// phasercenter (default 1000).
    pub center_hz: f32,
    /// phasersweep (default 2000) - the detune clamp, in cents.
    pub sweep_cents: f32,
    /// The note's onset in context seconds - the anchor for phase0.
    pub time_secs: f32,
}

/// Tremolo stage: `gain = max(1−depth, 0)` plus an LFO output
/// `clamp(pow(shape(phase,skew)·depth, 1.5), 0, 1)` summed onto the gain
/// param. Note the skew default is 1 (a ramp) unless a shape was given, and
/// the curve is fixed at 1.5.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TremoloControls {
    pub frequency_hz: f32,
    pub depth: f32,
    pub skew: f32,
    /// 0 tri, 1 sine, 2 ramp, 3 saw, 4 square.
    pub shape: u8,
    pub phase_offset: f32,
    /// `cycle / cps` - the musical time anchoring phase0.
    pub time_secs: f32,
}

impl TremoloControls {
    /// The carrier gain node's own value, `max(1 − depth, 0)`, which the LFO
    /// output adds to and a `tremolodepth` modulator rides.
    pub fn gain_floor(&self) -> f32 {
        (1.0 - self.depth).max(0.0)
    }
}

/// Fixed per-voice modulator capacity (each slot is POD in the live ring).
pub const MAX_VOICE_MODS: usize = 4;

/// How many `.FX()` stages a voice carries beyond its own params.
///
/// A voice is a fixed-size POD record in the lock-free live ring, so the
/// count has to be decided somewhere. Three is what the documented examples
/// reach for; a pattern naming more is skipped with a log rather than
/// silently dropping an effect.
pub const MAX_FX_STAGES: usize = 3;

/// One `.FX(...)` stage: a complete effects pass the voice runs before its
/// own params. The hap's own controls always come last.
///
/// A stage carries its own gain, filters and effects, including an inline
/// delay and an inline reverb. `orbit` and `duckorbit` have no effect inside
/// a stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxStage {
    /// `gain × velocity` for this stage. The gain default is 0.8, so a stage
    /// naming no gain still attenuates; velocity's default is 1.
    /// The FX-loop chain runs stretch and the transient shaper first, ahead of
    /// the gain stage:
    ///
    ///   stretch -> transient -> gain -> filters -> vowel -> coarse -> crush
    ///           -> shape -> distort -> tremolo -> compressor -> pan -> phaser
    ///
    /// The phase vocoder's pitch factor. Only a stretch on the hap's own
    /// params has its latency compensated: rustel-voice pulls the voice's
    /// start back by `STRETCH_LATENCY_SECS` plus
    /// `stretch::QUANTUM_LAG_FRAMES`. An `.FX()` stretch keeps its delay, so
    /// there is no compensation here.
    pub stretch: Option<f32>,
    pub transient: Option<TransientControls>,
    pub gain: f32,
    pub filters: FilterControls,
    pub vowel: Option<VowelControls>,
    pub coarse: Option<f32>,
    pub crush: Option<f32>,
    pub shape: Option<ShapeControls>,
    pub distort: Option<crate::distortion::DistortControls>,
    pub tremolo: Option<TremoloControls>,
    pub compressor: Option<CompressorControls>,
    /// Already converted to the StereoPanner's axis: `2·pan − 1`.
    pub pan_x: Option<f32>,
    pub phaser: Option<PhaserControls>,
    /// An `.FX()` stage's delay is inline - a feedback delay of the stage's
    /// own, not the orbit send: `out = signal·dry + delay(signal)·wet`.
    pub delay: Option<DelayControls>,
    /// `fx.dry ?? 1` - the dry leg's gain in the inline delay and reverb mix.
    pub dry: f32,
    /// An `.FX()` stage's reverb is inline and per voice - a convolver of
    /// the stage's own, unlike the main chain's per-orbit send:
    /// `out = signal·dry + reverb(signal)·wet`.
    pub room: Option<ReverbControls>,
}

/// Which audio parameter a modulator rides. This is the subset exposed by the
/// native graph; the resolver logs and skips unsupported targets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModTarget {
    LowpassFreq,
    HighpassFreq,
    BandFreq,
    /// `vowel` - the `frequency` AudioParam on each of the five parallel
    /// formant BiquadFilterNodes. One modulator signal is connected to all
    /// five; its base and frequency range come from the first formant.
    VowelFreq,
    Gain,
    Frequency,
    /// StereoPanner pan param (base = 2·pan − 1, clamped to [-1, 1]).
    Pan,
    /// The post GainNode (postgain control).
    Postgain,
    LowpassQ,
    HighpassQ,
    BandQ,
    /// Sample-and-hold divisor (`coarse`), bitcrush depth (`crush`) and
    /// waveshaper amount (`shape`) - ordinary modulation targets, so an LFO
    /// can sweep them.
    Coarse,
    Crush,
    Shape,
    /// `shapevol` - the shape stage's `postgain`, so it exists only
    /// alongside `shape`.
    ShapeVol,
    /// The distortion stage's `distort` and `postgain`. Unlike coarse,
    /// crush and shape, these are a-rate: the value moves within a render
    /// quantum rather than being latched at its start.
    Distort,
    DistortVol,
    /// Per-voice send gains. The delay line and the reverb are shared by the
    /// orbit, but the gain feeding each one belongs to the voice, so a
    /// modulator here moves this voice's contribution and nothing else.
    DelaySend,
    RoomSend,
    /// The phaser's own LFO (`phaserrate`, `phasersweep`) and the notch it
    /// drives (`phasercenter`, `phaserdepth`).
    PhaserRate,
    PhaserSweep,
    PhaserCenter,
    PhaserDepth,
    /// The dynamics compressor's five AudioParams.
    CompressorThreshold,
    CompressorRatio,
    CompressorKnee,
    CompressorAttack,
    CompressorRelease,
    /// Tremolo: its LFO frequency, skew, shape and phase, and the gain its
    /// depth drives. Frequency, skew, shape and phase belong to the LFO and
    /// move per quantum; the depth gain is an ordinary a-rate gain.
    TremoloRate,
    TremoloDepth,
    TremoloSkew,
    TremoloShape,
    /// Vibrato: the oscillator's rate, and the gain converting it to cents.
    VibratoRate,
    VibratoDepth,
    /// `pw` - the pulse oscillator's width, an a-rate source param.
    PulseWidth,
    /// The pulse-width LFO's `frequency` (`pwrate`) and `depth` (`pwsweep`)
    /// params; modulators targeting them are latched once per render
    /// quantum.
    PulseWidthLfoRate,
    PulseWidthLfoDepth,
    /// `dry` - the gain on the direct path.
    Dry,
    /// `djf` - the orbit's DJ filter. There is deliberately one per orbit,
    /// while the modulator belongs to a voice, so a voice's LFO here sweeps
    /// everything that orbit is carrying.
    Djf,
    /// `delaytime`/`delaysync` - the orbit feedback delay's a-rate
    /// `DelayNode.delayTime` param. The node is shared by the orbit, so every
    /// active voice targeting it contributes to the same param.
    DelayTime,
    /// `delayfeedback` - the gain feeding the shared orbit delay back into
    /// itself. Like an ordinary GainNode param, it is a-rate and receives the
    /// sum of every active voice modulating that orbit.
    DelayFeedback,
    /// `fmi`, `fmi2`..`fmi8` - the gain node carrying each operator's
    /// modulation index (`fm_{k}_gain`). Slot 0 is operator 1, the one that
    /// reaches the carrier.
    FmIndex(u8),
    /// `fmh`, `fmh2`..`fmh8` - the operator oscillator's frequency param
    /// (`fm_{k}`). This moves the operator's own pitch and nothing else: the
    /// gain converting its output back into hertz was built with the static
    /// frequency and does not follow, so the modulation depth stays put
    /// while the modulating tone slides.
    FmFreq(u8),
    /// A filter's own LFO: `lpdepth`/`lpdepthfrequency`, `lpdc`, `lpskew`
    /// and their hp/bp twins, reaching the LFO the filter builds for itself.
    ///
    /// Three of that family, `lprate`/`lpsync` and `lpshape` (with their
    /// hp/bp twins), are absent on purpose. Modulating them crashes the
    /// reference implementation (browser-confirmed), and Rustel does not
    /// copy a crash.
    FilterLfoDepth(FilterLfoKind),
    FilterLfoDc(FilterLfoKind),
    FilterLfoSkew(FilterLfoKind),
    /// The wavetable source's own a-rate params. `wt` is the table position,
    /// `detune` the unison frequency spread and `spread` the pan spread; the
    /// last two are shared with the supersaw source.
    WavetablePosition,
    SourceFreqspread,
    SourcePanspread,
    /// The wavetable's OWN position LFO (`wtrate`/`wtsync`, `wtdepth`,
    /// `wtskew`). Another LFO, block-latched like the filters'.
    ///
    /// `wtdc` is absent for the reason `lprate` is: modulating it crashes
    /// the reference implementation (browser-confirmed).
    WtLfoRate,
    WtLfoDepth,
    WtLfoSkew,
    /// `warp`, and its own LFO (`warprate`/`warpsync`, `warpdepth`,
    /// `warpskew`). `warpdc` is absent for the same reason `wtdc` is.
    WavetableWarp,
    WarpLfoRate,
    WarpLfoDepth,
    WarpLfoSkew,
    /// Another `lfo()`'s own params, named by that modulator's id - a
    /// modulator modulating a modulator.
    LfoParam(u8, ModulatorParam),
    /// The same for an `env()`.
    EnvParam(u8, EnvelopeParam),
}

/// Which param of another `lfo()` a modulator rides; when none is named,
/// rate is the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModulatorParam {
    /// `rate`/`sync`, and the default when nothing is named.
    Rate,
    /// `depth`/`depthabs` - both land on `depth`.
    Depth,
    Skew,
    Curve,
    Dcoffset,
}

/// Which param of another `env()` a modulator rides, same addressing.
/// When none is named this is `Depth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeParam {
    Depth,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Which filter's own LFO a modulator targets. `lpdepth` and its relatives
/// reach the filter's LFO, not the filter's biquad, so they name the filter
/// as well as the param.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterLfoKind {
    Lowpass,
    Highpass,
    Bandpass,
}

impl FilterLfoKind {
    /// Dense index, for the per-filter accumulator.
    pub fn index(self) -> usize {
        match self {
            FilterLfoKind::Lowpass => 0,
            FilterLfoKind::Highpass => 1,
            FilterLfoKind::Bandpass => 2,
        }
    }

    /// The control prefix that names this filter's LFO params.
    pub fn prefix(self) -> &'static str {
        match self {
            FilterLfoKind::Lowpass => "lp",
            FilterLfoKind::Highpass => "hp",
            FilterLfoKind::Bandpass => "bp",
        }
    }
}

/// Slot for `fm{i,h}{k}`: bare `fmi`/`fmh` is operator 1, `fmi2` is operator
/// 2, and so on. `None` for anything past the matrix's capacity.
pub fn fm_operator_slot(suffix: &str) -> Option<u8> {
    if suffix.is_empty() {
        return Some(0);
    }
    match suffix.parse::<u8>() {
        Ok(n) if (2..=(MAX_FM_OPERATORS as u8)).contains(&n) => Some(n - 1),
        _ => None,
    }
}

/// A modulator whose SIGNAL is another pattern's audio, taken off a bus.
///
/// `.bus(n)` sends a voice's post-gain output into bus `n`; `.bmod({b: n})`
/// reads that bus back and adds it to one of this voice's params. It is
/// modular-synth patching between patterns, and unlike an LFO the shape is
/// whatever the other pattern happens to be playing.
///
/// The modulator signal is `(bus + dc) · depth / 0.3`, clamped to the
/// target param's range. The 0.3 is a fixed normalisation constant, not a
/// tuning choice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BusMod {
    /// `fxi` - which `.FX()` stage this modulator aims at. `None` is the main
    /// chain, which is what an unnamed modulator means.
    pub fxi: Option<u8>,
    pub bus: u8,
    pub target: ModTarget,
    /// `depthabs` when given, else `depth × the param's value at connect
    /// time`.
    pub depth: f32,
    pub dc: f32,
    pub min: f32,
    pub max: f32,
    /// The target param's value at connect time, with 0 read as 1. This value
    /// is the divisor when the modulated param scales the signal (gain).
    pub param_base: f32,
}

/// How many buses a set may use. Fixed because the mixing buffers are
/// allocated at init.
pub const MAX_BUSES: usize = 16;

/// One `lfo()` modulator, resolved: modval per sample is
/// `clamp(pow((shape(phase) + dcoffset) * depth, curve), min, max)` with the
/// phase free-running from `ffrac(time·frequency + phaseoffset)` at onset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoMod {
    /// `fxi` - which `.FX()` stage this modulator aims at. `None` is the main
    /// chain, which is what an unnamed modulator means.
    pub fxi: Option<u8>,
    pub target: ModTarget,
    pub frequency_hz: f32,
    pub phase0: f32,
    pub depth: f32,
    pub dcoffset: f32,
    pub skew: f32,
    pub curve: f32,
    /// 0 tri, 1 sine, 2 ramp, 3 saw, 4 square.
    pub shape: u8,
    pub min: f32,
    pub max: f32,
    /// The target param's value at connect time, zero included. This value is
    /// the divisor when the modulated param scales the signal (gain). A NaN
    /// sample subtracts this value from the param default.
    pub param_base: f32,
    /// Set when this LFO is the one a filter built for itself rather than one
    /// the pattern asked for with `lfo()`. `lpdepth` and its relatives aim at
    /// this LFO, and a pattern LFO on the same filter frequency must not
    /// absorb them - the two are distinct modulators.
    pub filter: Option<FilterLfoKind>,
    /// This modulator's own id, so another modulator can name it (the
    /// position of its entry in the pattern's `lfo` map). `None` for a
    /// filter's own LFO, which the pattern never named and so cannot
    /// address.
    pub id: Option<u8>,
}

/// One `env()` modulator, resolved. The value ramps
/// 0→1 over attack (warped by the curve), 1→sustain over decay, holds until
/// `sustain_secs`, then releases; output is `clamp(val · depth, min, max)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvMod {
    /// `fxi` - which `.FX()` stage this modulator aims at. `None` is the main
    /// chain, which is what an unnamed modulator means.
    pub fxi: Option<u8>,
    pub target: ModTarget,
    pub attack_secs: f32,
    pub decay_secs: f32,
    pub sustain: f32,
    pub release_secs: f32,
    pub a_curve: f32,
    pub d_curve: f32,
    pub r_curve: f32,
    pub depth: f32,
    pub min: f32,
    pub max: f32,
    /// Hap duration + full release.
    pub sustain_secs: f32,
    pub param_base: f32,
    /// This envelope's own id, so another modulator can name it with
    /// `env_{id}`. See [`LfoMod::id`].
    pub id: Option<u8>,
}

/// Wavetable oscillator. Position = base + linear ADSR (min=base,
/// max=base+env_amount, defaults [0, 0.5, 0, 0.1]) + LFO
/// `clamp((shape(phase,skew)+dc)·depth, dc·depth, dc·depth+depth)` with
/// phase locked to absolute time; per-unison-voice detune
/// `n·(spread/(voices−1)) − spread/2` semitones, pan
/// `√(0.5∓0.5·panspread)` with odd voices swapped, output normalised by
/// `1/√voices`. Warp modes beyond NONE are refused at resolve time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WavetableControls {
    pub table: crate::sample::SampleId,
    /// Samples per single-cycle frame (2048 unless a `tables()` call says
    /// otherwise).
    pub frame_len: u32,
    /// Unison voice count - kept as a raw float: the render loop runs
    /// `ceil(voices)` voices while the detuner divides by the raw value
    /// (`unison(1.9)` is two voices spread as 1.9).
    pub voices: f32,
    /// LFO shape: 0 tri, 1 sine, 2 ramp, 3 saw, 4 square.
    pub lfo_shape: u8,
    /// `1` randomises initial phases. The seed is derived from the onset so
    /// offline renders remain deterministic.
    pub phaserand: f32,
    /// `detune` control (unison spread in semitones), default 0.18.
    pub freqspread: f32,
    /// `spread` control, default 0.7.
    pub panspread: f32,
    /// `wt` control: position base, 0..1.
    pub position: f32,
    pub pos_env_amount: f32,
    pub pos_attack: f32,
    pub pos_decay: f32,
    pub pos_sustain: f32,
    pub pos_release: f32,
    pub lfo_depth: f32,
    pub lfo_rate: f32,
    pub lfo_skew: f32,
    pub lfo_dc: f32,
    /// `warp` - how far the table's read phase is bent before sampling, and
    /// `warpmode` choosing which of the 21 shapes does the bending. Carries
    /// its own envelope and LFO, built exactly like the position pair above:
    /// `applyParameterModulators` is called twice with the same shape.
    pub warp: f32,
    /// Index into [`crate::warp::WarpMode`]; 0 is NONE, the unwarped table.
    pub warp_mode: u8,
    pub warp_env_amount: f32,
    pub warp_attack: f32,
    pub warp_decay: f32,
    pub warp_sustain: f32,
    pub warp_release: f32,
    pub warp_lfo_depth: f32,
    pub warp_lfo_rate: f32,
    pub warp_lfo_skew: f32,
    pub warp_lfo_dc: f32,
    pub warp_lfo_shape: u8,
}

/// The synthesised sources beyond the four table oscillators. One enum keeps
/// the event layout flat; the voice resolver applies parameter defaults.
/// How many built-in bytebeat expressions `n` selects between.
pub const BYTEBEAT_EXPRESSIONS: u8 = 15;

// The bytebeat program is 500-odd bytes against Bus's one, and that is the
// point rather than a problem: the event must stay POD for the ring, so a
// compiled expression rides inline. Boxing it would put an allocation on the
// path the no-alloc callback contract exists to keep clear.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SynthSource {
    /// `s("bus").n(N)` - play bus N back as a source, so a group of patterns
    /// can be processed together. The bus signal replaces the
    /// oscillator and then runs the ordinary voice chain, which is what lets
    /// one distortion or filter apply to everything feeding that bus.
    Bus { bus: u8 },
    /// `s("in")` - the audio input, one channel, as a source: a live signal
    /// with no length, gated by the event like a synth. `n` picks the
    /// channel of the selected input device.
    Input { channel: u8 },
    /// `s("bytebeat")` - one of fifteen built-in integer expressions
    /// sampled as an audio signal, chosen by `n`.
    ///
    /// A custom `byteBeatExpression` arrives as literal text (the transpiler
    /// exempts `bbexpr` from mini-notation) and is compiled into `program`.
    ByteBeat {
        /// Index into the built-in table, already reduced modulo its length.
        /// Ignored when `program` carries a compiled custom expression.
        expression: u8,
        /// A custom `byteBeatExpression`, compiled on the producer side.
        /// Parsing the control and then dropping it here (playing built-in
        /// 0 instead) once put the documented `bbexpr('t*(t>>15^t>>66)')`
        /// at corr -0.000120 and +4.745 dB.
        program: Option<crate::bytebeat::ByteBeatProgram>,
        /// Presence matters: when `byteBeatStartTime` is supplied, the
        /// counter resets to zero and adds this floored offset. When it is
        /// absent, the counter starts at the voice's onset frame instead.
        start_offset: Option<f64>,
    },
    /// `s("zzfx")` and the `z_*` waves: ZzFX's `buildSamples` loop,
    /// streamed. A ZzFX note has no outer envelope and no stop - it runs to
    /// its own end whatever the hap's duration.
    ZzFx { params: crate::zzfx::ZzfxParams },
    /// `supersaw`: polyblep saw stack,
    /// per-voice detune spread, alternating pan, env peak `0.3/√voices`.
    Supersaw {
        /// Raw float voices (`unison`), clamped here to 32.
        voices: f32,
        /// `detune ?? n ?? 0.18` - semitone spread across the stack.
        freqspread: f32,
        /// `spread` (0.6 default), 0 when a single voice.
        panspread: f32,
    },
    /// `white`, `pink`, `brown`, `crackle`: a
    /// looping two-second noise buffer at gain 0.3 under the ordinary synth
    /// ADSR.
    ///
    /// The noise is per-session, not per-note: every note of one type
    /// replays the same buffer from index 0. The stream is seeded, so
    /// renders are reproducible - same policy as the reverb IR and
    /// wavetable phase randomisation. `crackle` is the exception, rebuilt
    /// per note.
    Noise {
        /// 0 white, 1 pink, 2 brown, 3 crackle.
        kind: u8,
        /// `density`, crackle only (default 0.02).
        density: f32,
    },
    /// `pulse`: half-Tomisawa cosine pair with feedback and anti-hunting
    /// filters - including the deliberate per-block internal decay quirk.
    Pulse {
        /// `pw`, default 0.5.
        pulsewidth: f32,
        /// Present when the resolved `pwsweep` is nonzero. The pulse-width
        /// LFO is a separate modulator whose output adds to `pulsewidth`.
        width_lfo: Option<PulseWidthLfoControls>,
    },
    /// `sbd`: synthesised bass drum - triangle osc with an
    /// exponential pitch envelope (`penv` semitones over `pdecay`), a
    /// tanh(2x) saturation shaper, a 25 ms brown-noise burst at 1.2, and
    /// its own amplitude envelope (20 ms hold, exponential decay). Like
    /// [`SynthSource::Noise`], every voice replays the session's cached noise
    /// buffer from its beginning.
    Sbd {
        /// `decay` (0.5 default) - the drum body length.
        decay_secs: f32,
        /// `pdecay` (0.5 default) - pitch-envelope length.
        pdecay_secs: f32,
        /// `penv` (36 default) - pitch-envelope depth in semitones.
        penv_semitones: f32,
        /// `clip`-shortened stop, resolved against the hap duration.
        stop_secs: f32,
    },
}

/// The pulse synth's `pwrate`/`pwsweep` LFO: a triangle centered on zero,
/// with frequency/depth params read once per quantum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PulseWidthLfoControls {
    pub frequency_hz: f32,
    pub depth: f32,
    /// The note begin time that seeds the LFO phase once.
    pub time_secs: f32,
}

/// The SIMPLE fm chain (`fmi`/`fm` + `fmh` + optional fm ADSR): one sine
/// modulator at `carrier × harmonicity`, depth `fmi × modfreq` Hz.
/// The full 8-operator
/// matrix (`fmi2…`, `fmwave`, cross-targets) refuses at resolve time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FmControls {
    /// Operators 1..=8; slot `k - 1` holds operator `k`. `None` where no
    /// route names it - an operator exists only once a route mentions it,
    /// as either source or target.
    pub operators: [Option<FmOperator>; MAX_FM_OPERATORS],
    /// Every connection the `fmi` matrix declares, over the whole i x j
    /// grid: the diagonal `i == j + 1` is spelled `fmi{i}` (the plain
    /// chain), and everything else is `fmi{i}{j}`.
    pub routes: [Option<FmRoute>; MAX_FM_ROUTES],
}

/// One connection in the FM matrix.
///
/// The deviation it contributes is `source signal x amount x the SOURCE's own
/// nominal frequency`, which is what makes `fmi` read as a ratio rather than a
/// number of hertz. That last gain is built from the STATIC frequency, so
/// sliding an operator's pitch leaves the depth it contributes alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FmRoute {
    /// Operator 1..=8 whose output is sent.
    pub source: u8,
    /// Operator 1..=8 whose frequency is bent, or 0 for the carrier's.
    pub target: u8,
    pub amount: f32,
    /// Which `fm_index` modulation slot adds to `amount`. Only the diagonal
    /// routes have one, because only they have an `fmi{n}` name a modulator
    /// can address.
    pub mod_slot: Option<u8>,
}

/// How many connections one voice's FM matrix may declare.
pub const MAX_FM_ROUTES: usize = 16;

/// How many operators the matrix addresses: `fmi`..`fmi8`, so 1..=8.
pub const MAX_FM_OPERATORS: usize = 8;

/// What an FM operator oscillates with (`fmwave`).
///
/// A noise operator is a looping buffer rather than an oscillator, so it
/// has no frequency param: nothing can modulate INTO one (such a route is
/// dropped with a warning). Its nominal frequency still scales what it
/// sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FmWave {
    #[default]
    Sine,
    Triangle,
    Square,
    Sawtooth,
    /// Same kind codes as [`SynthSource::Noise`]: white, pink, brown, crackle.
    Noise(u8),
}

impl FmWave {
    /// The band-limited oscillator shape, or `None` for a noise buffer.
    pub fn oscillator(self) -> Option<Waveform> {
        match self {
            FmWave::Sine => Some(Waveform::Sine),
            FmWave::Triangle => Some(Waveform::Triangle),
            FmWave::Square => Some(Waveform::Square),
            FmWave::Sawtooth => Some(Waveform::Sawtooth),
            FmWave::Noise(_) => None,
        }
    }

    /// Whether an operator of this shape can be modulated by another one.
    pub fn accepts_modulation(self) -> bool {
        self.oscillator().is_some()
    }
}

/// One operator in the FM chain. It oscillates at `carrier x harmonicity` and
/// adds `index x its own frequency` to whatever it modulates, which is what
/// makes the index read as a ratio rather than a number of hertz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FmOperator {
    pub harmonicity: f32,
    /// `fmwave{n}` for this operator.
    pub waveform: FmWave,
    pub env: Option<Envelope>,
    pub env_exponential: bool,
}

/// The `room` send plus `roomsize`/`roomfade`/`roomlp`/`roomdim`, with
/// generate defaults (2 / 0.1 / 15000 / 1000).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbControls {
    /// `room` - the wet send amount.
    pub wet: f32,
    pub size_secs: f32,
    pub fade_secs: f32,
    pub lp_start_hz: f32,
    pub lp_end_hz: f32,
    /// `ir`/`irspeed`/`irbegin` - a custom impulse response resolved to a
    /// sample slot; `None` = the generated IR.
    pub ir: Option<crate::reverb::IrParams>,
}

/// One sidechain dip: the orbit list fans out per index, each entry falling
/// back to the FIRST onset/attack/depth value when its own is missing, so
/// `duckorbit("2:3").duckdepth("1:0.5")` dips orbit 2 by 1 and orbit 3 by
/// 0.5. Fixed capacity; the resolver logs-and-skips overflow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DuckControls {
    pub targets: [Option<DuckTarget>; MAX_DUCK_TARGETS],
}

pub const MAX_DUCK_TARGETS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DuckTarget {
    pub orbit: u8,
    pub onset_secs: f32,
    pub attack_secs: f32,
    pub depth: f32,
}

impl DuckControls {
    pub fn is_empty(&self) -> bool {
        self.targets.iter().all(Option::is_none)
    }
}

impl Default for OscillatorControls {
    fn default() -> Self {
        Self {
            live_controls: [0; 2],
            preview_epoch: 0,
            choke_only: false,
            piano: false,
            noise: 0.0,
            bus_mods: [None; MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            waveform: Waveform::Sine,
            envelope: Envelope::default(),
            worklet_begin_secs: 0.0,
            lfo_end_secs: f32::INFINITY,
            filter_lfo_end_secs: f32::INFINITY,
            // superdough's own `release` default, which is what
            // `endWithRelease` reads.
            modulator_release_secs: 0.01,
            velocity: 1.0,
            postgain: 1.0,
            pan: None,
            filters: FilterControls::default(),
            distort: None,
            delay: None,
            duck: None,
            reverb: None,
            dry: None,
            stretch: None,
            fm: None,
            orbit: 1,
            lfos: [None; MAX_VOICE_MODS],
            envs: [None; MAX_VOICE_MODS],
            phaser: None,
            tremolo: None,
            vowel: None,
            coarse: None,
            crush: None,
            shape: None,
            vibrato: None,
            pitch_env: None,
            djf: None,
            compressor: None,
            limit: None,
            partials: None,
            transient: None,
            fx_stages: [None; MAX_FX_STAGES],
        }
    }
}

/// Rust-owned onset description for the scalar oscillator or bundled-sample family.
/// Sample-accurate timing uses `onset_frame`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnsetEvent {
    pub onset_frame: u64,
    pub generation: u64,
    pub ui_visuals: u64,
    /// Sub-sample placement. A source is scheduled at a float time and so
    /// starts between output samples; rounding to a whole frame
    /// costs up to half a sample per onset. `onset_frame` is the first output
    /// frame at or after the true onset, and this is how far the source has
    /// already advanced by then, in frames - always 0 <= lead < 1. Only cps
    /// values whose cycle is a whole number of samples give 0 throughout (at
    /// 48 kHz, cps 0.5625 makes a cycle 85333.33 frames, so two onsets in
    /// three carry a lead).
    pub onset_lead: f32,
    pub freq_hz: f32,
    pub gain: f32,
    /// Note length in seconds (gate/envelope duration).
    pub duration_secs: f32,
    pub controls: OscillatorControls,
    /// `None` selects the oscillator family.
    pub sample: Option<crate::SampleControls>,
    /// `wt_` sounds: the wavetable oscillator instead of one-shot playback.
    pub wavetable: Option<WavetableControls>,
    /// supersaw / pulse / sbd - synthesised sources beyond the four tables.
    pub synth: Option<SynthSource>,
    /// Event-level choke group: a new onset in the group cuts the previous
    /// one over 10 ms, for any kind of source. A sample's own `.cut(n)`
    /// reaches the same registry through `SampleControls`. This field serves
    /// onsets that have no sample controls, mainly the browser's auditions,
    /// which play one preview at a time.
    pub cut: Option<f32>,
}

impl OnsetEvent {
    /// Place the onset between output frames (see `onset_lead`).
    #[must_use]
    pub fn with_onset_lead(mut self, lead: f32) -> Self {
        self.onset_lead = if lead.is_finite() {
            lead.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self
    }

    pub fn new(onset_frame: u64, freq_hz: f32, gain: f32, duration_secs: f32) -> Self {
        Self {
            onset_frame,
            generation: 0,
            ui_visuals: 0,
            onset_lead: 0.0,
            freq_hz,
            gain,
            duration_secs,
            controls: OscillatorControls::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }
    }

    #[must_use]
    pub fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    #[must_use]
    pub fn with_ui_visuals(mut self, visuals: u64) -> Self {
        self.ui_visuals = visuals;
        self
    }

    pub fn with_optional_synth(mut self, synth: Option<SynthSource>) -> Self {
        self.synth = synth;
        self
    }

    /// Join an event-level choke group (see [`OnsetEvent::cut`]).
    #[must_use]
    pub fn with_cut(mut self, cut: Option<f32>) -> Self {
        self.cut = cut;
        self
    }

    pub fn with_optional_wavetable(mut self, wavetable: Option<WavetableControls>) -> Self {
        self.wavetable = wavetable;
        self
    }

    pub fn with_controls(mut self, controls: OscillatorControls) -> Self {
        self.controls = controls;
        self
    }

    pub fn with_sample(mut self, sample: crate::SampleControls) -> Self {
        self.sample = Some(sample);
        self
    }

    pub(crate) fn with_optional_sample(mut self, sample: Option<crate::SampleControls>) -> Self {
        self.sample = sample;
        self
    }
}

/// Pluggable offline/real-time audio backend. Implementations accept only
/// Rust-owned params and produce interleaved stereo `f32` PCM.
pub trait AudioBackend {
    /// Prepare (or re-prepare) the engine for `sample_rate` Hz rendering.
    fn init(&mut self, sample_rate: u32) -> Result<(), String>;

    /// Clear voices/schedule/clock without tearing down capacity.
    fn reset(&mut self);

    /// Submit a note. Offline renderers may buffer until [`process_block`].
    fn note(&mut self, event: OnsetEvent);

    /// Render the next `frames` stereo frames into `out` (length `frames * 2`).
    fn process_block(&mut self, out: &mut [f32], frames: usize);

    /// Human-readable backend name.
    fn name(&self) -> &'static str;
}

/// JavaScript's ToInt32: truncate toward zero, wrap modulo 2^32, reinterpret
/// as signed. Every bitwise operator in a bytebeat expression applies it to
/// both operands, so the arithmetic around them has to keep float semantics
/// while the bitwise steps do not.
pub(crate) fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    // A direct float-to-integer cast saturates once the value leaves the i64
    // range. JavaScript instead wraps every finite integer modulo 2^32, even
    // for values such as `1e20` that ByteBeat start offsets can expose.
    let wrapped = value.trunc().rem_euclid(4_294_967_296.0);
    (wrapped as u32) as i32
}

/// The fifteen built-in expressions `s("bytebeat")` selects between.
///
/// Written out rather than interpreted: a control string is mini-notation, so
/// a custom expression cannot reach the synth, and these are the only ones a
/// score can actually choose.
pub fn bytebeat_sample(expression: u8, t: f64) -> f64 {
    let i = |v: f64| f64::from(to_int32(v));
    match expression % BYTEBEAT_EXPRESSIONS {
        // '(t%255 >= t/255%255)*255'
        0 => f64::from(u8::from(t % 255.0 >= (t / 255.0) % 255.0)) * 255.0,
        // '(t*(t*8%60 <= 300)|(-t)*(t*4%512 < 256))+t/400'
        1 => {
            let left = t * f64::from(u8::from((t * 8.0) % 60.0 <= 300.0));
            let right = -t * f64::from(u8::from((t * 4.0) % 512.0 < 256.0));
            f64::from(to_int32(left) | to_int32(right)) + t / 400.0
        }
        // 't'
        2 => t,
        // 't*(t >> 10^t)'
        3 => t * i(f64::from(to_int32(t) >> 10 ^ to_int32(t))),
        // 't&128'
        4 => i(f64::from(to_int32(t) & 128)),
        // 't&t>>8'
        5 => i(f64::from(to_int32(t) & (to_int32(t) >> 8))),
        // '((t%255+t%128+t%64+t%32+t%16+t%127.8+t%64.8+t%32.8+t%16.8)/3)'
        6 => {
            (t % 255.0
                + t % 128.0
                + t % 64.0
                + t % 32.0
                + t % 16.0
                + t % 127.8
                + t % 64.8
                + t % 32.8
                + t % 16.8)
                / 3.0
        }
        // '((t%64+t%63.8+t%64.15+t%64.35+t%63.5)/1.25)'
        7 => (t % 64.0 + t % 63.8 + t % 64.15 + t % 64.35 + t % 63.5) / 1.25,
        // '(t&(t>>7)-t)'
        8 => i(f64::from(
            to_int32(t) & to_int32(f64::from(to_int32(t) >> 7) - t),
        )),
        // '(sin(t*PI/128)*127+127)'
        9 => (t * std::f64::consts::PI / 128.0).sin() * 127.0 + 127.0,
        // '((t^t/2+t+64*(sin((t*PI/64)+(t*PI/32768))+64))%128*2)'
        10 => {
            let inner = ((t * std::f64::consts::PI / 64.0) + (t * std::f64::consts::PI / 32768.0))
                .sin()
                + 64.0;
            let xor = to_int32(t) ^ to_int32(t / 2.0 + t + 64.0 * inner);
            (f64::from(xor) % 128.0) * 2.0
        }
        // '((t^t/2+t+64*(cos >> 0))%127.85*2)'
        //
        // `cos` here is the Math.cos FUNCTION, not a call: `cos >> 0` coerces
        // a function object to 0. Reproduced rather than corrected.
        11 => {
            let xor = to_int32(t) ^ to_int32(t / 2.0 + t + 64.0 * 0.0);
            (f64::from(xor) % 127.85) * 2.0
        }
        // '((t^t/2+t+64)%128*2)'
        12 => {
            let xor = to_int32(t) ^ to_int32(t / 2.0 + t + 64.0);
            (f64::from(xor) % 128.0) * 2.0
        }
        // '(((t * .25)^(t * .25)/100+(t * .25))%128)*2'
        13 => {
            let q = t * 0.25;
            let xor = to_int32(q) ^ to_int32(q / 100.0 + q);
            (f64::from(xor) % 128.0) * 2.0
        }
        // '((t^t/2+t+64)%7 * 24)'
        _ => {
            let xor = to_int32(t) ^ to_int32(t / 2.0 + t + 64.0);
            (f64::from(xor) % 7.0) * 24.0
        }
    }
}

/// `funcValue & 255` - the low byte of the expression's ToInt32 value.
pub fn bytebeat_byte(value: f64) -> u8 {
    (to_int32(value) & 255) as u8
}

#[cfg(test)]
mod bytebeat_integer_tests {
    use super::to_int32;

    #[test]
    fn to_int32_wraps_like_javascript_outside_the_i64_range() {
        assert_eq!(to_int32(4_294_967_297.0), 1);
        assert_eq!(to_int32(-4_294_967_297.0), -1);
        assert_eq!(to_int32(1e20), 1_661_992_960);
        assert_eq!(to_int32(-1e20), -1_661_992_960);
        assert_eq!(to_int32(1e308), 0);
        assert_eq!(to_int32(f64::INFINITY), 0);
        assert_eq!(to_int32(f64::NAN), 0);
    }
}
