use super::*;

impl Default for OrbitDelayBlock {
    fn default() -> Self {
        Self {
            input: [[0.0; crate::reverb::REVERB_BLOCK]; 2],
            time_mod: [0.0; crate::reverb::REVERB_BLOCK],
            feedback_mod: [0.0; crate::reverb::REVERB_BLOCK],
        }
    }
}

/// What a feedback line is allowed to remember.
///
/// A delay line is the one place in the graph where a single bad sample is
/// permanent: the write is `input + feedback * delayed`, so once a `NaN`
/// lands in the buffer it reads back as `NaN`, and `NaN * feedback` is `NaN`
/// however far below one the feedback is clamped. The line never decays, the
/// orbit never sounds again, and taking the offending score back out changes
/// nothing - the state that is broken is not in the score.
///
/// So nothing but a real number is stored. Whatever produced it is a bug
/// worth fixing at the source; this is what keeps that bug an audible glitch
/// rather than the end of the set.
#[inline]
fn storable(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

impl OrbitDelay {
    const SILENCE_FLOOR: f32 = 0.000_001;

    /// DelayNode linearly interpolates an a-rate delay between adjacent
    /// samples. A node in a feedback cycle cannot read inside the current
    /// quantum, so the effective delay is at least 128 frames.
    #[inline]
    pub(super) fn read(line: &[f32], write: usize, delay_frames: f32) -> f32 {
        let len = line.len();
        if len == 0 {
            return 0.0;
        }
        // A sample rate under 16 kHz gives a line shorter than the
        // 128-frame quantum; the floor yields to the line's own end so the
        // clamp stays a range.
        let ceiling = (len - 1) as f32;
        let floor = 128.0_f32.min(ceiling);
        let delay_frames = delay_frames.clamp(floor, ceiling);
        let whole = delay_frames.floor() as usize;
        let fraction = delay_frames - whole as f32;
        let newer = (write + len - whole) % len;
        let older = (newer + len - 1) % len;
        line[older] * fraction + line[newer] * (1.0 - fraction)
    }

    #[inline]
    pub(super) fn store(&mut self, left: f32, right: f32) {
        let left = storable(left);
        let right = storable(right);
        for (old, new) in [
            (self.left[self.write], left),
            (self.right[self.write], right),
        ] {
            self.energized = self
                .energized
                .saturating_sub(usize::from(old.abs() > Self::SILENCE_FLOOR))
                .saturating_add(usize::from(new.abs() > Self::SILENCE_FLOOR));
        }
        self.left[self.write] = left;
        self.right[self.write] = right;
        self.write = (self.write + 1) % self.left.len();
    }
}

impl FxDelayLine {
    pub(super) fn new(sample_rate: u32) -> Self {
        // The line is fixed at a 1-second maximum, and `delaytime` is
        // clamped to that at resolve.
        let frames = sample_rate.max(1) as usize + 1;
        Self {
            left: vec![0.0; frames],
            right: vec![0.0; frames],
            write: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
    }
}

#[allow(clippy::vec_box)]
pub(super) fn reserve_fx_reverb_returns(
    pool: &mut Vec<Box<crate::reverb::OrbitReverb>>,
    inline_count: usize,
) {
    let target = MAX_INSTALLED_FX_REVERBS
        .checked_add(inline_count)
        .expect("FX reverb return capacity overflow");
    if pool.capacity() < target {
        pool.reserve(target - pool.len());
    }
}

impl NotchState {
    #[inline]
    pub(super) fn process(&mut self, c: &NotchCoefs, x: f32) -> f32 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Bandpass coefficients (linear Q): alpha = sin(w0)/(2Q). Vowel formants
/// always land in the well-defined range, but keep the edges.
pub(super) fn bandpass_coefs(frequency_hz: f32, q: f32, sample_rate: f32) -> NotchCoefs {
    let f = (frequency_hz / (sample_rate * 0.5)).clamp(0.0, 1.0);
    if f > 0.0 && f < 1.0 {
        if q <= 0.0 {
            return NotchCoefs {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            };
        }
        let w0 = std::f32::consts::PI * f;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let a0 = 1.0 + alpha;
        return NotchCoefs {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
        };
    }
    NotchCoefs {
        b0: 0.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

/// One `.FX()` stage: a complete effects chain of its own.
///
/// Stereo rather than per-channel, because `pan` sits in the middle of this
/// chain and needs both channels at once.
///
/// Modulators targeting this stage's `fxi` contribute through `adds`.
#[inline]
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_fx_stage(
    left: f32,
    right: f32,
    state: &mut FxStageState,
    t: f32,
    // The hap's end, which is where a filter's envelope is scheduled to.
    hap_duration_secs: f32,
    block_n: f32,
    sample_rate: f32,
    quantum_secs: f64,
    // The transient shaper's frame-based begin and end gate.
    transient_live: bool,
    // Tremolo and phaser share the gate with f32-rounded begin and end times.
    // See `lfo_quantum_open`.
    lfo_live: bool,
    // What modulators naming this stage's `fxi` contributed this sample.
    adds: &FxStageAdds,
) -> (f32, f32) {
    let stage = &state.controls;
    let stereo = state.filters_right.is_some();
    let mut left = left;
    // A mono source's right channel is a copy. Mirrored here as well as after
    // the filters because stretch and the transient shaper run BEFORE them and
    // are stateful per channel: feeding one of them a stale right channel
    // desynchronises its envelope from the left's.
    let mut right = if stereo { right } else { left };

    if let Some(kabel) = &mut state.kabel {
        (left, right) = kabel.process(left, quantum_secs, f64::from(sample_rate));
    }

    // Stretch and the transient shaper precede the gain stage
    if let (Some(vocoder), Some(factor)) = (state.stretch.as_mut(), stage.stretch) {
        left = vocoder[0].process(left, factor);
        if stereo {
            right = vocoder[1].process(right, factor);
        }
    }
    if let Some(transient) = state.transient.as_mut() {
        // Outside the gate, leave the shaper's state unchanged and feed
        // silence to the remaining stages.
        if transient_live {
            let (l, r) = transient.process(left, right);
            left = l;
            right = r;
        } else {
            left = 0.0;
            right = 0.0;
        }
    }

    let stage_gain = stage.gain * adds.gain_factor();
    left *= stage_gain;
    right *= stage_gain;

    let filter_adds = [adds.lowpass_hz, adds.highpass_hz, adds.bandpass_hz];
    let filter_q_adds = [adds.lowpass_q, adds.highpass_q, adds.band_q];
    left = state
        .filters
        .process_modulated(left, t, hap_duration_secs, filter_adds, filter_q_adds);
    if let Some(filters_right) = state.filters_right.as_mut() {
        right = filters_right.process_modulated(
            right,
            t,
            hap_duration_secs,
            filter_adds,
            filter_q_adds,
        );
    }

    let worklets = FxWorkletControls {
        vowel: stage.vowel.as_ref(),
        coarse: stage.coarse,
        crush: stage.crush,
        shape: stage.shape,
    };
    let vowel_coefs = state.vowel_coefs;
    left = voice_fx_pre_distort(
        left,
        0,
        worklets,
        &vowel_coefs,
        &mut state.vowel_filters,
        &mut state.coarse_hold,
        block_n,
        &FxParamHold::default(),
    );
    if stereo {
        right = voice_fx_pre_distort(
            right,
            1,
            worklets,
            &vowel_coefs,
            &mut state.vowel_filters,
            &mut state.coarse_hold,
            block_n,
            &FxParamHold::default(),
        );
    }

    if let Some(distort) = stage.distort {
        let curve = distort.curve(distort.shape());
        left = curve.apply(left);
        if stereo {
            right = curve.apply(right);
        }
    }
    // Everything above is per-channel and a mono source has no right chain, so
    // its right channel is a copy. Everything below can split the two: the
    // pan stage upmixes mono, and after it the channels differ. The copy must
    // happen here. A `(left, left)` copy at the end would discard the pan.
    if !stereo {
        right = left;
    }

    // Tremolo is an amplitude gain shared by both channels: one LFO into one
    // GainNode, not one per channel.
    if let Some(tr) = stage.tremolo {
        let base = tr.gain_floor();
        let gain = if lfo_live {
            let phase = *state
                .tremolo_phase
                .get_or_insert_with(|| lfo_phase0(tr.time_secs, tr.frequency_hz, tr.phase_offset));
            let raw = lfo_waveshape(tr.shape, phase, tr.skew) * tr.depth;
            let next = wrap_phase(phase + f64::from(tr.frequency_hz) / f64::from(sample_rate));
            state.tremolo_phase = Some(next);
            let gain = base + raw.powf(1.5).clamp(0.0, 1.0);
            // A NaN sum gives the gain its default.
            if gain.is_nan() { 1.0 } else { gain }
        } else {
            base
        };
        left *= gain;
        right *= gain;
    }

    if let (Some(compressor), Some(controls)) = (state.compressor.as_mut(), stage.compressor) {
        let (l, r) = compressor.process(left, right, &controls, sample_rate);
        left = l;
        right = r;
    }

    // StereoPanner has two laws, selected by its input's channel count. For
    // a mono input it is `L = in*cos(x), R = in*sin(x)` with
    // `x = (pan+1)*pi/4`, so a centred pan attenuates by 0.707. For a stereo
    // input it is the pass-through law that `stereo_pan` implements, which
    // is the identity at centre. The stereo law on a mono voice is too
    // loud, by sqrt(2) at centre.
    if let Some(pan_x) = stage.pan_x {
        let x = pan_x.clamp(-1.0, 1.0);
        if state.source_is_mono {
            let (lg, rg) = ScalarBackend::channel_gains(Some((x + 1.0) * 0.5));
            right = left * rg;
            left *= lg;
        } else {
            let (l, r) = stereo_pan(left, right, x);
            left = l;
            right = r;
        }
    }

    // Phaser: one notch whose centre is swept by a triangle LFO, the detune
    // clamped to +/- sweep CENTS. `getPhaser` builds its LFO with depth =
    // sweep*2 and passes no phase offset, anchored at the note's begin.
    if let Some(ph) = stage.phaser {
        let lfo_depth = (2.0 * ph.sweep_cents).max(0.0);
        let sweep = lfo_depth * 0.5;
        // Before the gate opens and after it closes the LFO is silent, so
        // the notch sits at its static centre.
        let detune = if lfo_live {
            let phase = *state
                .phaser_phase
                .get_or_insert_with(|| lfo_phase0(ph.time_secs, ph.rate_hz, 0.0));
            let raw = (lfo_waveshape(0, phase, 0.5) - 0.5) * lfo_depth;
            let next = wrap_phase(phase + f64::from(ph.rate_hz) / f64::from(sample_rate));
            state.phaser_phase = Some(next);
            js_clamp(raw, -sweep, sweep)
        } else {
            0.0
        };
        let frequency = (ph.center_hz + 282.0) * (detune / 1200.0).exp2();
        // phaserdepth names the notch's Q, not a wet/dry depth.
        let q = 2.0 - (ph.depth * 2.0).clamp(0.0, 1.9);
        let coefs = notch_coefs(frequency, q, sample_rate);
        left = state.phaser_filters[0].process(&coefs, left);
        right = state.phaser_filters[1].process(&coefs, right);
    }

    // Inline feedback delay, last in the stage. The dry
    // leg is summed with the wet one rather than replaced:
    // `out = signal·dry + delay(signal)·wet`.
    if let (Some(line), Some(delay)) = (state.delay.as_mut(), stage.delay) {
        let frames = delay.time_secs * sample_rate;
        let wet_l = OrbitDelay::read(&line.left, line.write, frames);
        let wet_r = OrbitDelay::read(&line.right, line.write, frames);
        // What goes INTO the line is the signal plus the delayed output fed
        // back, which is the DelayNode -> feedbackGain -> DelayNode cycle.
        line.left[line.write] = storable(left + wet_l * delay.feedback);
        line.right[line.write] = storable(right + wet_r * delay.feedback);
        line.write = (line.write + 1) % line.left.len();
        left = left * stage.dry + wet_l * delay.wet;
        right = right * stage.dry + wet_r * delay.wet;
    }

    // Inline convolution reverb, last in the stage.
    // Fed one frame at a time: the convolver already stages input to its own
    // 128-frame hop and drains a wet FIFO, which is what lets a live host hand
    // it buffers that are not multiples of 128. The cost is that the wet
    // stream runs one hop (<=2.9 ms) late, which for a reverb tail is not
    // audible.
    if let (Some(reverb), Some(room)) = (state.room.as_mut(), stage.room) {
        let mut wet = [0.0f32; 1];
        let mut wet_r = [0.0f32; 1];
        reverb.process_block(&[left], &[right], &mut wet, &mut wet_r);
        left = left * stage.dry + wet[0] * room.wet;
        right = right * stage.dry + wet_r[0] * room.wet;
    }

    (left, right)
}

/// `2^(crush - 1)`, the quantiser's step count. A whole-number crush is an
/// exact power of two, built without the library call.
#[inline]
fn crush_levels(crush: f64) -> f64 {
    let exponent = crush - 1.0;
    let whole = exponent as u64;
    if whole <= 1023 && whole as f64 == exponent {
        f64::from_bits((whole + 1023) << 52)
    } else {
        exponent.exp2()
    }
}

/// Post-filter, pre-distort stages in the FX chain order:
/// vowel -> coarse -> crush -> shape. A modulator add only lands when the
/// matching control is set: an LFO on `crush` with no `crush` in the pattern
/// has no stage to reach and does nothing. `block_n` is the CONTEXT
/// block-local index (abs % 128) - the coarse hold pattern is block-local,
/// so it re-anchors every quantum.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn voice_fx_pre_distort(
    mut sample: f32,
    ch: usize,
    controls: FxWorkletControls,
    vowel_coefs: &[NotchCoefs; 5],
    vowel_filters: &mut [[NotchState; 5]; 2],
    coarse_hold: &mut [f32; 2],
    block_n: f32,
    fx_mod: &FxParamHold,
) -> f32 {
    if let Some(vw) = controls.vowel {
        let mut sum = 0.0;
        for k in 0..5 {
            sum += vowel_filters[ch][k].process(&vowel_coefs[k], sample) * vw.gains[k];
        }
        sample = sum * 8.0;
    }
    if let Some(coarse) = controls.coarse {
        let coarse = (coarse + fx_mod.coarse).max(1.0);
        if block_n % coarse < 1.0 {
            coarse_hold[ch] = sample;
        }
        sample = coarse_hold[ch];
    }
    if let Some(crush) = controls.crush {
        let crush = f64::from(crush + fx_mod.crush).max(1.0);
        let x = crush_levels(crush);
        // Round half-up (floor(x + 0.5)), which differs from f64::round on
        // negative halves.
        sample = ((f64::from(sample) * x + 0.5).floor() / x) as f32;
    }
    if let Some(sh) = controls.shape {
        let s0 = f64::from(sh.shape + fx_mod.shape);
        let s0 = if s0 < 1.0 { s0 } else { 1.0 - 4e-10 };
        let k = (2.0 * s0) / (1.0 - s0);
        let post = f64::from(sh.postgain + fx_mod.shape_vol).clamp(0.001, 1.0);
        let x = f64::from(sample);
        sample = ((1.0 + k) * x / (1.0 + k * x.abs()) * post) as f32;
    }
    sample
}

impl TransientState {
    /// `timeToCoeff(t) = 1 - exp(-1/(sr·t))`.
    pub(super) fn new(controls: crate::backend::TransientControls, sample_rate: f32) -> Self {
        let coeff = |t: f32| 1.0 - (-1.0 / (sample_rate * t)).exp();
        // Only attack and sustain are passed; the rest are the
        // processor's own defaults, already clamped there.
        Self {
            attack_env: [0.0; 2],
            sustain_env: [0.0; 2],
            avg_gain: 1.0,
            attack_coeff: coeff(0.003),
            sustain_coeff: coeff(0.08),
            gain_coeff: coeff(0.2),
            attack_amt: controls.attack.clamp(-1.0, 1.0),
            sustain_amt: controls.sustain.clamp(-1.0, 1.0),
            scaling: 0.5 + 5.0 * 0.1_f32.clamp(0.0, 1.0),
            mix: 1.0,
        }
    }

    /// One channel. `avg_gain` is shaper state rather than per-channel, so
    /// it is threaded through instead of read from `self`: channel 1
    /// continues from where channel 0 left it.
    fn process_channel(&mut self, ch: usize, sample: f32, avg_gain: &mut f32) -> f32 {
        let x = sample.abs();
        let att = lerp(self.attack_env[ch], x, self.attack_coeff);
        let sus = lerp(self.sustain_env[ch], x, self.sustain_coeff);
        self.attack_env[ch] = att;
        self.sustain_env[ch] = sus;
        let peakiness = js_clamp(self.scaling * (att - sus) / (sus + 1e-6), -1.5, 1.5);
        let att_scale = peakiness.max(0.0);
        let sus_scale = (-peakiness).max(0.0);
        let attack_gain = db_to_lin(self.attack_amt * att_scale * 18.0);
        let sustain_gain = db_to_lin(self.sustain_amt * sus_scale * 36.0);
        let gain = js_clamp(attack_gain * sustain_gain, 0.0, 8.0);
        *avg_gain = lerp(*avg_gain, gain, self.gain_coeff);
        let makeup = if *avg_gain > 1e-3 {
            1.0 / *avg_gain
        } else {
            1.0
        };
        let wet = sample * gain * makeup;
        let y = lerp(sample, wet, self.mix);
        y / (1.0 + y.abs())
    }

    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let mut avg_gain = self.avg_gain;
        let l = self.process_channel(0, left, &mut avg_gain);
        let r = self.process_channel(1, right, &mut avg_gain);
        self.avg_gain = avg_gain;
        (l, r)
    }

    /// A source that has not been upmixed yet reaches the shaper as ONE
    /// channel and runs exactly one follower - not the stereo pair with a
    /// duplicated signal.
    pub(super) fn process_mono(&mut self, sample: f32) -> f32 {
        let mut avg_gain = self.avg_gain;
        let out = self.process_channel(0, sample, &mut avg_gain);
        self.avg_gain = avg_gain;
        out
    }
}

#[inline]
fn lerp(a: f32, b: f32, n: f32) -> f32 {
    n * (b - a) + a
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

impl DjfState {
    #[inline]
    pub(super) fn process(&mut self, ch: usize, input: f32, sample_rate: f64) -> f32 {
        let value = (self.value + self.modulation).clamp(0.0, 1.0);
        let (hipass, v) = if value > 0.51 {
            (true, (value - 0.5) * 2.0)
        } else if value < 0.49 {
            (false, value * 2.0)
        } else {
            return input;
        };
        let cutoff = f64::from(v * 11.0)
            .powi(4)
            .clamp(0.0, sample_rate * 0.5 - 1.0);
        let c = (2.0 * (cutoff * std::f64::consts::PI / sample_rate).sin()).clamp(0.0, 1.14);
        // resonance fixed at 0.1: r = 0.5^(8·0.1 + 1)
        let r = 0.5f64.powf(1.8);
        let mrc = 1.0 - r * c;
        let state = &mut self.s[ch];
        let x = f64::from(input);
        state[0] = mrc * state[0] - c * state[1] + c * x;
        state[1] = mrc * state[1] + c * state[0];
        if hipass {
            (x - state[1]) as f32
        } else {
            state[1] as f32
        }
    }
}

fn compressor_db_to_lin(db: f32) -> f32 {
    10.0f32.powf(0.05 * db)
}

fn compressor_lin_to_db(x: f32) -> f32 {
    // `log10f(0)` is -inf - deliberately let through and caught with
    // `finite_or` at each use site.
    20.0 * x.log10()
}

fn finite_or(x: f32, default: f32) -> f32 {
    if x.is_finite() { x } else { default }
}

impl CompressorState {
    /// A pool entry before any voice has leased it. The contents are
    /// irrelevant: every lease overwrites with [`Self::new`].
    pub(super) fn cold() -> Self {
        Self {
            ring: [[0.0; 2]; COMPRESSOR_RING_FRAMES],
            read: 0,
            write: 0,
            delay_frames: 0,
            frame: 0,
            detector_average: 0.0,
            compressor_gain: 1.0,
            db_max_attack_diff: -1.0,
            envelope_rate: 1.0,
            scaled_desired_gain: 1.0,
            cached_params: (f32::NAN, f32::NAN, f32::NAN),
            k: 5.0,
            linear_threshold: 0.0,
            knee_threshold: 0.0,
            db_knee_threshold: 0.0,
            db_yknee_threshold: 0.0,
            slope: 1.0,
            linear_post_gain: 1.0,
        }
    }

    pub(super) fn new(
        sample_rate: f32,
        c: &crate::backend::CompressorControls,
        onset_frame: u64,
    ) -> Self {
        let mut state = Self::cold();
        state.delay_frames = ((sample_rate * 0.006) as usize).min(COMPRESSOR_RING_FRAMES - 1);
        state.write = state.delay_frames;
        state.update_static_curve(c);
        state.preroll(onset_frame, c, sample_rate);
        state
    }

    /// `KneeCurve`: linear up to the threshold, then 1st-derivative matched
    /// and asymptotically approaching `linear_threshold + 1/k`.
    fn knee_curve(&self, x: f32, k: f32) -> f32 {
        if x < self.linear_threshold {
            return x;
        }
        self.linear_threshold + (1.0 - f64::from(-k * (x - self.linear_threshold)).exp() as f32) / k
    }

    /// `Saturate`: the knee curve below the knee threshold, constant ratio
    /// above it.
    fn saturate(&self, x: f32) -> f32 {
        if x < self.knee_threshold {
            return self.knee_curve(x, self.k);
        }
        let db_x = compressor_lin_to_db(x);
        let db_y = self.db_yknee_threshold + self.slope * (db_x - self.db_knee_threshold);
        compressor_db_to_lin(db_y)
    }

    /// `KAtSlope`: bisect for the knee sharpness whose slope in dB/dB at the
    /// knee's end matches `1/ratio`. Fifteen geometric-mean steps, verbatim.
    fn k_at_slope(&self, desired_slope: f32) -> f32 {
        let db_x = self.cached_params.0 + self.cached_params.1;
        let x = compressor_db_to_lin(db_x);
        let (mut x2, mut db_x2) = (1.0f32, 0.0f32);
        if x >= self.linear_threshold {
            x2 = x * 1.001;
            db_x2 = compressor_lin_to_db(x2);
        }
        let mut min_k = 0.1f32;
        let mut max_k = 10_000.0f32;
        let mut k = 5.0f32;
        let mut slope = 1.0f32;
        for _ in 0..15 {
            if x >= self.linear_threshold {
                let db_y = compressor_lin_to_db(self.knee_curve(x, k));
                let db_y2 = compressor_lin_to_db(self.knee_curve(x2, k));
                slope = (db_y2 - db_y) / (db_x2 - db_x);
            }
            if slope < desired_slope {
                max_k = k;
            } else {
                min_k = k;
            }
            k = (min_k * max_k).sqrt();
        }
        k
    }

    /// `UpdateStaticCurveParameters` + the makeup gain derived from it.
    fn update_static_curve(&mut self, c: &crate::backend::CompressorControls) {
        let params = (c.threshold_db, c.knee_db, c.ratio);
        if params == self.cached_params {
            return;
        }
        self.cached_params = params;
        self.linear_threshold = compressor_db_to_lin(c.threshold_db);
        self.slope = 1.0 / c.ratio;
        self.k = self.k_at_slope(self.slope);
        self.db_knee_threshold = c.threshold_db + c.knee_db;
        self.knee_threshold = compressor_db_to_lin(self.db_knee_threshold);
        self.db_yknee_threshold =
            compressor_lin_to_db(self.knee_curve(self.knee_threshold, self.k));
        // Makeup gain with the perceptual 0.6 power - empirical, keep it.
        self.linear_post_gain = (1.0 / self.saturate(1.0)).powf(0.6);
    }

    /// The per-division envelope update, run at every 32nd frame.
    fn division(&mut self, c: &crate::backend::CompressorControls, sample_rate: f32) {
        self.detector_average = finite_or(self.detector_average, 1.0);
        let desired_gain = self.detector_average;
        // Pre-warp so the `sin` warp in `process` lands on the desired gain.
        let scaled = desired_gain.asin() / std::f32::consts::FRAC_PI_2;
        self.scaled_desired_gain = scaled;
        let is_releasing = scaled > self.compressor_gain;
        let db_compression_diff = if scaled == 0.0 {
            if is_releasing { -1.0 } else { 1.0 }
        } else {
            compressor_lin_to_db(self.compressor_gain / scaled)
        };
        if is_releasing {
            self.db_max_attack_diff = -1.0;
            let diff = finite_or(db_compression_diff, -1.0);
            // Adaptive release - deeper compression releases faster. The
            // polynomial passes through the four release-zone fractions of
            // `release_frames` at x = 0..3, x spanning -12 -> 0 dB.
            let x = (diff.clamp(-12.0, 0.0) + 12.0) * 0.25;
            let release_frames = sample_rate * c.release_secs;
            let mut coeffs = [0.0f32; 5];
            for (slot, row) in coeffs.iter_mut().zip(COMPRESSOR_POLY) {
                let mut sum = 0.0f64;
                for (weight, zone) in row.iter().zip(COMPRESSOR_RELEASE_ZONES) {
                    sum += weight * f64::from(zone);
                }
                *slot = release_frames * sum as f32;
            }
            let x2 = x * x;
            let calc_release_frames = coeffs[0]
                + coeffs[1] * x
                + coeffs[2] * x2
                + coeffs[3] * x2 * x
                + coeffs[4] * x2 * x2;
            const DB_SPACING: f32 = 5.0;
            self.envelope_rate = compressor_db_to_lin(DB_SPACING / calc_release_frames);
        } else {
            let diff = finite_or(db_compression_diff, 1.0);
            // Attack keeps using the largest difference seen this attack.
            if self.db_max_attack_diff == -1.0 || self.db_max_attack_diff < diff {
                self.db_max_attack_diff = diff;
            }
            let eff_atten_diff = self.db_max_attack_diff.max(0.5);
            let x = 0.25 / eff_atten_diff;
            let attack_frames = c.attack_secs.max(0.001) * sample_rate;
            self.envelope_rate = 1.0 - x.powf(1.0 / attack_frames);
        }
    }

    /// Replay `frames` of the silence the compressor processed between graph
    /// build and the note's onset.
    ///
    /// Frame-by-frame in f32, not closed forms. The iteration stalls an ulp
    /// or two short of its limits once the per-frame step drops below f32
    /// resolution. `asin` is steep near 1, so the difference between the
    /// stall value and an exact 1.0 is audible through the makeup gain as a
    /// small transient on every prerolled onset. The stall also ends the
    /// loop: a division that changes nothing is a true fixed point of the
    /// iteration, and the rest of the wait is a counter update.
    fn preroll(&mut self, frames: u64, c: &crate::backend::CompressorControls, sample_rate: f32) {
        if frames == 0 {
            return;
        }
        // Silence keeps the detector's inputs constant: attenuation is 1, its
        // dB floor is the 2 dB minimum, so the release rate is fixed.
        let sat_release_frames = COMPRESSOR_SAT_RELEASE_SECS * sample_rate;
        let detector_rate = compressor_db_to_lin(2.0 / sat_release_frames) - 1.0;
        let mut done = 0u64;
        while done < frames {
            let before = (
                self.detector_average.to_bits(),
                self.compressor_gain.to_bits(),
                self.db_max_attack_diff.to_bits(),
            );
            self.division(c, sample_rate);
            let span = (frames - done).min(32);
            for _ in 0..span {
                // attenuation 1 > detector while the detector is below 1.
                if self.detector_average < 1.0 {
                    self.detector_average += (1.0 - self.detector_average) * detector_rate;
                }
                self.detector_average = finite_or(self.detector_average.min(1.0), 1.0);
                if self.envelope_rate < 1.0 {
                    self.compressor_gain +=
                        (self.scaled_desired_gain - self.compressor_gain) * self.envelope_rate;
                } else {
                    self.compressor_gain = (self.compressor_gain * self.envelope_rate).min(1.0);
                }
            }
            done += span;
            self.frame += span;
            let after = (
                self.detector_average.to_bits(),
                self.compressor_gain.to_bits(),
                self.db_max_attack_diff.to_bits(),
            );
            if span == 32 && after == before {
                self.frame += frames - done;
                break;
            }
        }
    }

    #[inline]
    pub(super) fn process(
        &mut self,
        l: f32,
        r: f32,
        c: &crate::backend::CompressorControls,
        sample_rate: f32,
    ) -> (f32, f32) {
        // Params are read once per render quantum; the static curve and
        // envelope refresh at the division boundary.
        if self.frame & 31 == 0 {
            // Division locals are flushed to zero when subnormal; at
            // extreme thresholds the gain genuinely reaches that range.
            if self.detector_average.abs() < f32::MIN_POSITIVE {
                self.detector_average = 0.0;
            }
            if self.compressor_gain.abs() < f32::MIN_POSITIVE {
                self.compressor_gain = 0.0;
            }
            self.update_static_curve(c);
            self.division(c, sample_rate);
        }
        self.frame += 1;

        // The detector runs on the UNDELAYED input while the gain applies to
        // the delayed signal - the pre-delay is lookahead.
        let compressor_input = l.abs().max(r.abs());
        self.ring[self.write] = [l, r];

        let shaped_input = self.saturate(compressor_input);
        let attenuation = if compressor_input <= 1e-4 {
            1.0
        } else {
            shaped_input / compressor_input
        };
        let db_attenuation = (-compressor_lin_to_db(attenuation)).max(2.0);
        let sat_release_frames = COMPRESSOR_SAT_RELEASE_SECS * sample_rate;
        let sat_release_rate = compressor_db_to_lin(db_attenuation / sat_release_frames) - 1.0;
        let rate = if attenuation > self.detector_average {
            sat_release_rate
        } else {
            1.0
        };
        self.detector_average += (attenuation - self.detector_average) * rate;
        self.detector_average = finite_or(self.detector_average.min(1.0), 1.0);

        if self.envelope_rate < 1.0 {
            // Attack - pull the gain down toward the pre-warped target.
            self.compressor_gain +=
                (self.scaled_desired_gain - self.compressor_gain) * self.envelope_rate;
        } else {
            // Release - multiplicative climb back toward 1.
            self.compressor_gain = (self.compressor_gain * self.envelope_rate).min(1.0);
        }

        // Warp the gain to smooth the exponential transition points. In f64
        // on purpose: the reference computes exactly this one in double.
        let post_warp =
            (std::f64::consts::FRAC_PI_2 * f64::from(self.compressor_gain)).sin() as f32;
        let total_gain = self.linear_post_gain * post_warp;

        let delayed = self.ring[self.read];
        self.read = (self.read + 1) & (COMPRESSOR_RING_FRAMES - 1);
        self.write = (self.write + 1) & (COMPRESSOR_RING_FRAMES - 1);
        (delayed[0] * total_gain, delayed[1] * total_gain)
    }
}

/// Notch coefficients: normalized frequency clamped to [0, 1]; at the
/// edges nothing gets notched (pass-through). Q stays > 0 here (the phaser
/// clamps depth so Q ∈ [0.1, 2]).
pub(super) fn notch_coefs(frequency_hz: f32, q: f32, sample_rate: f32) -> NotchCoefs {
    let f = (frequency_hz / (sample_rate * 0.5)).clamp(0.0, 1.0);
    if f <= 0.0 || f >= 1.0 {
        // Frequency pinned to an edge: the z-transform is 1 (pass-through).
        return NotchCoefs {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
    }
    if q <= 0.0 {
        // The response is defined as zero when Q ≤ 0.
        return NotchCoefs {
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
    }
    let w0 = std::f32::consts::PI * f;
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();
    let a0 = 1.0 + alpha;
    NotchCoefs {
        b0: 1.0 / a0,
        b1: -2.0 * cos_w0 / a0,
        b2: 1.0 / a0,
        a1: -2.0 * cos_w0 / a0,
        a2: (1.0 - alpha) / a0,
    }
}

impl ScalarBackend {
    pub(super) fn ensure_fx_stage_state_pool(&mut self, capacity: usize) {
        let leased = self
            .voices
            .iter()
            .filter(|voice| voice.fx_stage_state.is_some())
            .count();
        let target = capacity.min(MAX_ACTIVE_VOICES);
        let available = self.fx_stage_state_pool.len().saturating_add(leased);
        self.fx_stage_state_pool
            .extend((available..target).map(|_| Box::new(std::array::from_fn(|_| None))));
    }

    /// Install (or replace) one orbit's reverb, returning the displaced one
    /// for the CALLER to free. Live path: the producer generates and ships
    /// through the device's channel; offline renders generate inline.
    pub fn install_reverb(
        &mut self,
        orbit: usize,
        mut reverb: Box<crate::reverb::OrbitReverb>,
    ) -> Option<Box<crate::reverb::OrbitReverb>> {
        if self.orbit_reverbs.is_empty() {
            self.orbit_reverbs = (0..MAX_ORBITS).map(|_| None).collect();
        }
        reverb.set_dispatch(self.dispatch);
        self.orbit_reverbs[orbit.min(MAX_ORBITS - 1)].replace(reverb)
    }

    /// The reverb an orbit currently has, for producer-side param checks.
    pub fn orbit_reverb_params(&self, orbit: usize) -> Option<crate::reverb::ReverbParams> {
        self.orbit_reverbs
            .get(orbit)?
            .as_ref()
            .map(|reverb| reverb.params())
    }

    /// Live-path policy switch: forbid IR synthesis at activation time.
    pub fn forbid_inline_reverb(&mut self) {
        self.allow_inline_reverb = false;
    }

    /// Marks this backend as driven by an offline render, which waits for
    /// inserts that render ahead. The live callback never waits.
    pub fn render_offline(&mut self) {
        self.offline = true;
    }

    /// Hand a stage reverb, generated off the audio thread, to the pool.
    /// Returns the box when the byte budget refuses it so the CALLER can free
    /// off the audio thread - dropping here would free IR partitions under the
    /// callback deadline.
    pub fn install_fx_reverb(
        &mut self,
        mut reverb: Box<crate::reverb::OrbitReverb>,
    ) -> Option<Box<crate::reverb::OrbitReverb>> {
        let bytes = reverb.approx_bytes();
        if bytes <= MAX_FX_REVERB_BYTES.saturating_sub(self.fx_reverb_bytes) {
            // Init reserves these slots before callback delivery. Pre-init
            // installation is also supported and prepares the same capacity.
            reserve_fx_reverb_returns(&mut self.fx_reverb_pool, self.fx_reverb_inline_count);
            reverb.set_dispatch(self.dispatch);
            self.fx_reverb_bytes += bytes;
            self.fx_reverb_pool.push(reverb);
            None
        } else {
            Some(reverb)
        }
    }

    /// Live installs may retire idle stage reverbs to make room for new
    /// parameters. Leased reverbs stay with their voices until retirement.
    /// The caller returns displaced boxes to the producer for destruction.
    pub fn install_fx_reverb_evicting(
        &mut self,
        reverb: Box<crate::reverb::OrbitReverb>,
        max_retire: usize,
        mut retire: impl FnMut(Box<crate::reverb::OrbitReverb>),
    ) -> Option<Box<crate::reverb::OrbitReverb>> {
        let bytes = reverb.approx_bytes();
        if bytes > MAX_FX_REVERB_BYTES {
            return Some(reverb);
        }
        let mut retired = 0;
        while bytes > MAX_FX_REVERB_BYTES.saturating_sub(self.fx_reverb_bytes)
            && retired < max_retire
            && !self.fx_reverb_pool.is_empty()
        {
            // Pointer movement only. Vec::remove would shift the whole pool
            // under the callback deadline; swap_remove does no allocation.
            let old = self.fx_reverb_pool.swap_remove(0);
            self.fx_reverb_bytes -= old.approx_bytes();
            retire(old);
            retired += 1;
        }
        self.install_fx_reverb(reverb)
    }

    pub(crate) fn fx_reverb_resident_bytes(&self) -> usize {
        self.fx_reverb_bytes
    }

    /// Reverb params `.FX()` stages asked for and did not get. Drained by the
    /// producer, which generates them and installs the result.
    pub fn take_wanted_fx_reverbs(&mut self) -> Vec<crate::reverb::ReverbParams> {
        std::mem::take(&mut self.fx_reverb_wanted)
    }

    /// Onsets that played dry because their orbit reverb was not ready.
    pub fn missing_reverb_events(&self) -> u64 {
        self.missing_reverb_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod feedback_line_tests {
        use super::{OrbitDelay, storable};

        /// A feedback line must not store a `NaN`. The write is `input +
        /// feedback * delayed`, so a stored non-finite sample never decays.
        #[test]
        fn a_feedback_line_forgets_a_bad_sample_instead_of_circulating_it_forever() {
            assert_eq!(storable(f32::NAN), 0.0);
            assert_eq!(storable(f32::INFINITY), 0.0);
            assert_eq!(storable(f32::NEG_INFINITY), 0.0);
            assert_eq!(storable(-0.75), -0.75, "an ordinary sample is untouched");

            // Run the line the way the mixer does, with one poisoned sample in
            // the middle of an otherwise silent input.
            let frames = 512;
            let mut line = vec![0.0f32; frames];
            let mut write = 0usize;
            let feedback = 0.98;
            let delay_frames = 200.0;
            for frame in 0..4_000 {
                let input = if frame == 10 { f32::NAN } else { 0.0 };
                let delayed = OrbitDelay::read(&line, write, delay_frames);
                line[write] = storable(input + feedback * delayed);
                write = (write + 1) % frames;
            }
            assert!(
                line.iter().all(|sample| sample.is_finite()),
                "the line is still carrying something that is not a number"
            );

            // And it can carry sound again afterwards, which is what "it never
            // plays again" was really about.
            for frame in 0..600 {
                let input = if frame < 64 { 0.5 } else { 0.0 };
                let delayed = OrbitDelay::read(&line, write, delay_frames);
                line[write] = storable(input + feedback * delayed);
                write = (write + 1) % frames;
            }
            assert!(
                line.iter().any(|sample| sample.abs() > 0.1),
                "the line stayed dead after the bad sample"
            );
        }

        #[test]
        fn an_orbit_delay_stays_energized_through_its_silent_gap_until_overwritten() {
            let mut delay = OrbitDelay {
                left: vec![0.0; 4],
                right: vec![0.0; 4],
                active: true,
                ..OrbitDelay::default()
            };
            delay.store(0.5, 0.0);
            assert_eq!(delay.energized, 1);

            for _ in 0..3 {
                delay.store(0.0, 0.0);
                assert_eq!(
                    delay.energized, 1,
                    "silence before the echo must not look like a drained line"
                );
            }
            delay.store(0.0, 0.0);
            assert_eq!(delay.energized, 0, "the last audible cell was overwritten");
        }
    }

    mod compressor_tests {
        use crate::backend::CompressorControls;

        #[test]
        fn an_impulse_emerges_exactly_predelay_frames_later() {
            let controls = CompressorControls {
                threshold_db: -20.0,
                ratio: 10.0,
                knee_db: 20.0,
                attack_secs: 0.002,
                release_secs: 0.02,
            };
            for (rate, expected) in [
                (48_000.0_f32, 288_usize),
                (44_100.0, 264),
                (96_000.0, 576),
                // The 0.006 s pre-delay clamps at 1023 frames.
                (176_400.0, 1023),
                (192_000.0, 1023),
            ] {
                let mut state = super::CompressorState::new(rate, &controls, 0);
                assert_eq!(
                    state.delay_frames, expected,
                    "pre-delay at {rate} Hz should be {expected} frames (0.006 s)"
                );
                assert!(
                    !state.delay_frames.is_multiple_of(128),
                    "0.006 s is not a whole number of quanta at {rate} Hz; \
                     landing on one means the rounding came back"
                );
                let mut first = None;
                for frame in 0..(expected + 64) {
                    let input = if frame == 0 { 1.0 } else { 0.0 };
                    let (left, _) = state.process(input, input, &controls, rate);
                    if first.is_none() && left.abs() > 1e-9 {
                        first = Some(frame);
                    }
                }
                assert_eq!(
                    first,
                    Some(expected),
                    "at {rate} Hz the impulse should surface at frame {expected}"
                );
            }
        }
    }

    mod compressor_preroll_tests {
        use crate::backend::CompressorControls;

        /// `new` replays the silence between graph build and the note's onset.
        /// The replay must be bit-identical to processing that silence,
        /// including the f32 stall an ulp short of the limits.
        #[test]
        fn a_prerolled_compressor_matches_one_that_lived_through_the_silence() {
            let rate = 48_000.0_f32;
            let controls = CompressorControls {
                threshold_db: -100.0, // the deep threshold is the sensitive case
                ratio: 10.0,
                knee_db: 10.0,
                attack_secs: 0.002,
                release_secs: 0.02,
            };
            for silence in [32u64, 800, 24_000, 100_000] {
                let mut lived = super::CompressorState::new(rate, &controls, 0);
                for _ in 0..silence {
                    lived.process(0.0, 0.0, &controls, rate);
                }
                let mut prerolled = super::CompressorState::new(rate, &controls, silence);
                // Feed both a burst and compare every output sample.
                for frame in 0..2_000u64 {
                    let x = if frame % 700 < 350 { 0.9 } else { 0.001 };
                    let a = lived.process(x, x, &controls, rate);
                    let b = prerolled.process(x, x, &controls, rate);
                    assert_eq!(
                        a, b,
                        "after {silence} frames of silence, outputs split at frame {frame}"
                    );
                }
            }
        }
    }

    #[test]
    fn notch_kills_center_and_passes_extremes() {
        let sr = 48_000.0;
        let c = notch_coefs(1_000.0, 1.0, sr);
        let peak = |frequency: f32| {
            let mut state = NotchState::default();
            let mut tail = 0.0f32;
            for n in 0..48_000 {
                let x = (std::f32::consts::TAU * frequency * n as f32 / sr).sin();
                let y = state.process(&c, x);
                if n > 40_000 {
                    tail = tail.max(y.abs());
                }
            }
            tail
        };
        let tail = peak(1_000.0);
        assert!(tail < 0.01, "notch leaked {tail} at center");
        let tail = peak(100.0);
        assert!(tail > 0.9, "notch attenuated far-field to {tail}");
        let edge = notch_coefs(0.0, 1.0, sr);
        assert_eq!(edge.b0, 1.0);
        assert_eq!(edge.a1, 0.0);
    }

    #[test]
    fn crush_levels_match_exp2_bit_for_bit() {
        let whole = (1..=1025).map(f64::from);
        for crush in whole.chain([1.5, 4.25, 12.75, 2000.0, f64::INFINITY]) {
            let want = (crush - 1.0).exp2();
            assert_eq!(crush_levels(crush).to_bits(), want.to_bits(), "{crush}");
        }
    }
}
