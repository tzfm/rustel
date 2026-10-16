use super::*;

impl FxStageAdds {
    /// `gain` is relative to the value the modulator was built against, the
    /// same ratio the main chain uses.
    pub(super) fn gain_factor(&self) -> f32 {
        if self.gain_base == 0.0 {
            1.0
        } else {
            (self.gain_base + self.gain) / self.gain_base
        }
    }
}

impl ModAdds {
    /// The compressor with this frame's modulation folded in. Each param
    /// clamps to its own range, so a modulator cannot drive the ratio below
    /// 1 or the knee negative however deep it swings.
    pub(super) fn compressor(
        &self,
        base: crate::backend::CompressorControls,
    ) -> crate::backend::CompressorControls {
        if self.comp_threshold == 0.0
            && self.comp_ratio == 0.0
            && self.comp_knee == 0.0
            && self.comp_attack == 0.0
            && self.comp_release == 0.0
        {
            return base;
        }
        crate::backend::CompressorControls {
            threshold_db: (base.threshold_db + self.comp_threshold).clamp(-100.0, 0.0),
            ratio: (base.ratio + self.comp_ratio).clamp(1.0, 20.0),
            knee_db: (base.knee_db + self.comp_knee).clamp(0.0, 40.0),
            attack_secs: (base.attack_secs + self.comp_attack).clamp(0.0, 1.0),
            release_secs: (base.release_secs + self.comp_release).clamp(0.0, 1.0),
        }
    }
}

impl ModAdds {
    /// The block-rate subset, snapshotted at a quantum start.
    pub(super) fn fx_hold(&self) -> FxParamHold {
        FxParamHold {
            coarse: self.coarse,
            crush: self.crush,
            shape: self.shape,
            shape_vol: self.shape_vol,
            phaser_rate: self.phaser_rate,
            phaser_sweep: self.phaser_sweep,
            trem_rate: self.trem_rate,
            trem_skew: self.trem_skew,
            trem_shape: self.trem_shape,
        }
    }

    /// Route one modulator's contribution, either to the main chain or to the
    /// `.FX()` stage it named.
    ///
    /// A stage is a fixed chain rather than the full param surface, so a
    /// target it cannot build is dropped here rather than silently landing on
    /// the main chain's bucket of the same name - which would modulate a
    /// different filter than the score asked for.
    pub(super) fn bucket_at(
        &mut self,
        fxi: Option<u8>,
        target: crate::backend::ModTarget,
        value: f32,
        base: f32,
    ) {
        use crate::backend::ModTarget;
        let Some(stage) = fxi else {
            return self.bucket(target, value, base);
        };
        let Some(adds) = self.fx_stage.get_mut(usize::from(stage)) else {
            return;
        };
        match target {
            ModTarget::LowpassFreq => adds.lowpass_hz += value,
            ModTarget::HighpassFreq => adds.highpass_hz += value,
            ModTarget::BandFreq => adds.bandpass_hz += value,
            ModTarget::LowpassQ => adds.lowpass_q += value,
            ModTarget::HighpassQ => adds.highpass_q += value,
            ModTarget::BandQ => adds.band_q += value,
            ModTarget::Gain => {
                adds.gain += value;
                adds.gain_base = base;
            }
            _ => {}
        }
    }

    fn bucket(&mut self, target: crate::backend::ModTarget, value: f32, base: f32) {
        use crate::backend::ModTarget;
        match target {
            ModTarget::LowpassFreq => self.lowpass_hz += value,
            ModTarget::HighpassFreq => self.highpass_hz += value,
            ModTarget::BandFreq => self.bandpass_hz += value,
            ModTarget::VowelFreq => self.vowel_hz += value,
            ModTarget::Gain => {
                self.gain += value;
                self.gain_base = base;
            }
            ModTarget::Frequency => self.frequency_hz += value,
            ModTarget::Pan => self.pan += value,
            ModTarget::Postgain => {
                self.postgain += value;
                self.postgain_base = base;
            }
            ModTarget::LowpassQ => self.lowpass_q += value,
            ModTarget::HighpassQ => self.highpass_q += value,
            ModTarget::BandQ => self.band_q += value,
            ModTarget::Coarse => self.coarse += value,
            ModTarget::Crush => self.crush += value,
            ModTarget::Shape => self.shape += value,
            ModTarget::ShapeVol => self.shape_vol += value,
            ModTarget::Distort => self.distort += value,
            ModTarget::DistortVol => self.distort_vol += value,
            ModTarget::DelaySend => self.delay_send += value,
            ModTarget::RoomSend => self.room_send += value,
            ModTarget::PhaserRate => self.phaser_rate += value,
            ModTarget::PhaserSweep => self.phaser_sweep += value,
            ModTarget::PhaserCenter => self.phaser_center += value,
            ModTarget::PhaserDepth => self.phaser_depth += value,
            ModTarget::CompressorThreshold => self.comp_threshold += value,
            ModTarget::CompressorRatio => self.comp_ratio += value,
            ModTarget::CompressorKnee => self.comp_knee += value,
            ModTarget::CompressorAttack => self.comp_attack += value,
            ModTarget::CompressorRelease => self.comp_release += value,
            ModTarget::TremoloRate => self.trem_rate += value,
            ModTarget::TremoloDepth => self.trem_depth += value,
            ModTarget::TremoloSkew => self.trem_skew += value,
            ModTarget::TremoloShape => self.trem_shape += value,
            ModTarget::VibratoRate => self.vib_rate += value,
            ModTarget::VibratoDepth => self.vib_depth += value,
            ModTarget::PulseWidth => self.pulse_width += value,
            ModTarget::PulseWidthLfoRate => self.mod_params.pulse_width.rate += value,
            ModTarget::PulseWidthLfoDepth => self.mod_params.pulse_width.depth += value,
            ModTarget::Dry => self.dry += value,
            ModTarget::Djf => self.djf += value,
            ModTarget::DelayTime => self.delay_time += value,
            ModTarget::DelayFeedback => self.delay_feedback += value,
            ModTarget::FmIndex(slot) => {
                if let Some(add) = self.fm_index.get_mut(usize::from(slot)) {
                    *add += value;
                }
            }
            ModTarget::FmFreq(slot) => {
                if let Some(add) = self.fm_freq.get_mut(usize::from(slot)) {
                    *add += value;
                }
            }
            ModTarget::WavetablePosition => self.position += value,
            ModTarget::SourceFreqspread => self.freqspread += value,
            ModTarget::SourcePanspread => self.panspread += value,
            ModTarget::WtLfoRate => self.mod_params.wt.rate += value,
            ModTarget::WtLfoDepth => self.mod_params.wt.depth += value,
            ModTarget::WtLfoSkew => self.mod_params.wt.skew += value,
            ModTarget::WavetableWarp => self.warp += value,
            ModTarget::WarpLfoRate => self.mod_params.warp.rate += value,
            ModTarget::WarpLfoDepth => self.mod_params.warp.depth += value,
            ModTarget::WarpLfoSkew => self.mod_params.warp.skew += value,
            ModTarget::FilterLfoDepth(kind) => self.mod_params.filter[kind.index()].depth += value,
            ModTarget::FilterLfoDc(kind) => self.mod_params.filter[kind.index()].dc += value,
            ModTarget::FilterLfoSkew(kind) => self.mod_params.filter[kind.index()].skew += value,
            ModTarget::LfoParam(id, param) => {
                if let Some(add) = self.mod_params.lfo.get_mut(usize::from(id)) {
                    use crate::backend::ModulatorParam;
                    match param {
                        ModulatorParam::Rate => add.rate += value,
                        ModulatorParam::Depth => add.depth += value,
                        ModulatorParam::Skew => add.skew += value,
                        ModulatorParam::Curve => add.curve += value,
                        ModulatorParam::Dcoffset => add.dcoffset += value,
                    }
                }
            }
            ModTarget::EnvParam(id, param) => {
                if let Some(add) = self.mod_params.env.get_mut(usize::from(id)) {
                    use crate::backend::EnvelopeParam;
                    match param {
                        EnvelopeParam::Depth => add.depth += value,
                        EnvelopeParam::Attack => add.attack += value,
                        EnvelopeParam::Decay => add.decay += value,
                        EnvelopeParam::Sustain => add.sustain += value,
                        EnvelopeParam::Release => add.release += value,
                    }
                }
            }
        }
    }

    /// True when this target aims at another MODULATOR rather than at the
    /// voice. Those have to be summed BEFORE the modulators they aim at are
    /// evaluated, the way a node feeding an AudioParam is processed before the
    /// node that owns it.
    ///
    /// One level deep. A modulator of a modulator of a modulator would need
    /// a real topological order, which two passes do not provide;
    /// three-deep chains are refused.
    pub(super) fn targets_another_modulator(target: crate::backend::ModTarget) -> bool {
        use crate::backend::ModTarget;
        matches!(
            target,
            ModTarget::FilterLfoDepth(_)
                | ModTarget::FilterLfoDc(_)
                | ModTarget::FilterLfoSkew(_)
                | ModTarget::LfoParam(..)
                | ModTarget::EnvParam(..)
                | ModTarget::WtLfoRate
                | ModTarget::WtLfoDepth
                | ModTarget::WtLfoSkew
                | ModTarget::WarpLfoRate
                | ModTarget::WarpLfoDepth
                | ModTarget::WarpLfoSkew
                | ModTarget::PulseWidthLfoRate
                | ModTarget::PulseWidthLfoDepth
        )
    }

    /// Multiplier the modulated gain param applies relative to its base
    /// (the base is already baked into the voice amplitude).
    pub(super) fn gain_factor(&self) -> f32 {
        if self.gain == 0.0 || self.gain_base <= 0.0 {
            1.0
        } else {
            ((self.gain_base + self.gain) / self.gain_base).max(0.0)
        }
    }

    /// Same contract for the post gain stage.
    pub(super) fn postgain_factor(&self) -> f32 {
        if self.postgain == 0.0 || self.postgain_base <= 0.0 {
            1.0
        } else {
            ((self.postgain_base + self.postgain) / self.postgain_base).max(0.0)
        }
    }
}

/// Clamp that tolerates an inverted range. `f32::clamp` PANICS when
/// `min > max`, which a modulator reaches legitimately: `depth = depth ·
/// currentValue` goes negative on a negative param base (`lfo({c:'pan'})`
/// with pan < 0.5 gives base 2·pan−1 < 0), and the LFO's
/// `min = dcoffset·depth`, `max = dcoffset·depth + depth` then arrive
/// inverted. This form returns `max` in that case.
///
/// A NaN input gives a bound and does not propagate. The `lfo()` site tests
/// its sample for NaN before this clamp. See [`nan_param_default`].
#[inline]
pub(super) fn js_clamp(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

/// The default value of the AudioParam a modulator target names.
///
/// An `lfo()` sample is NaN when a fractional `curve` meets a negative value.
/// The sum on the target param is then NaN. WebAudio replaces a NaN param
/// value with the param default. `source` selects the param where the source
/// nodes differ.
///
/// The caller adds `default - base` in place of the NaN sample. The param
/// lands on the default when no other signal feeds the param. A second
/// modulator still adds. So do FM on a synth, vibrato and a pitch envelope on
/// a sample, and the LFO or envelope of a source param. An orbit param gets
/// one such sum from each voice. The five vowel filters share one base, so
/// the first one alone lands on 350 Hz.
///
/// The function returns NaN for a filter frequency and for the tremolo gain.
/// The sum stays NaN, and the reader of the sum takes the default alone.
#[cold]
pub(super) fn nan_param_default(target: crate::backend::ModTarget, source: &VoiceSource) -> f32 {
    use crate::backend::{EnvelopeParam, ModTarget, ModulatorParam};
    // The wavetable and the supersaw worklets have different spread defaults.
    let (freqspread, panspread) = if matches!(source, VoiceSource::Supersaw { .. }) {
        (0.2, 0.4)
    } else {
        (0.18, 0.7)
    };
    match target {
        // The filter stage takes 350 Hz, or the 500 Hz of the ladder worklet.
        // The tremolo takes 1.
        ModTarget::LowpassFreq
        | ModTarget::HighpassFreq
        | ModTarget::BandFreq
        | ModTarget::TremoloDepth => f32::NAN,
        // BiquadFilterNode `frequency`.
        ModTarget::VowelFreq | ModTarget::PhaserCenter => 350.0,
        // A sample has no `frequency` param. Its pitch modulator rides
        // `detune`.
        ModTarget::Frequency if matches!(source, VoiceSource::Sample { .. }) => 0.0,
        // The `frequency` of an OscillatorNode and of each synth worklet.
        ModTarget::Frequency | ModTarget::FmFreq(_) | ModTarget::VibratoRate => 440.0,
        // BiquadFilterNode `Q`.
        ModTarget::LowpassQ | ModTarget::HighpassQ | ModTarget::BandQ | ModTarget::PhaserDepth => {
            1.0
        }
        // GainNode `gain`.
        ModTarget::Gain
        | ModTarget::Postgain
        | ModTarget::Dry
        | ModTarget::DelaySend
        | ModTarget::RoomSend
        | ModTarget::DelayFeedback
        | ModTarget::VibratoDepth
        | ModTarget::FmIndex(_) => 1.0,
        // StereoPannerNode `pan` and DelayNode `delayTime`.
        ModTarget::Pan | ModTarget::DelayTime => 0.0,
        ModTarget::CompressorThreshold => -24.0,
        ModTarget::CompressorRatio => 12.0,
        ModTarget::CompressorKnee => 30.0,
        ModTarget::CompressorAttack => 0.003,
        ModTarget::CompressorRelease => 0.25,
        // The effect and source worklets.
        ModTarget::Coarse | ModTarget::ShapeVol | ModTarget::DistortVol | ModTarget::PulseWidth => {
            1.0
        }
        ModTarget::Crush
        | ModTarget::Shape
        | ModTarget::Distort
        | ModTarget::WavetablePosition
        | ModTarget::WavetableWarp => 0.0,
        ModTarget::Djf => 0.5,
        ModTarget::SourceFreqspread => freqspread,
        ModTarget::SourcePanspread => panspread,
        // The LFO worklet: `frequency` and `skew` 0.5, `depth` and `curve` 1,
        // `dcoffset` and `shape` 0.
        ModTarget::PhaserRate
        | ModTarget::TremoloRate
        | ModTarget::TremoloSkew
        | ModTarget::PulseWidthLfoRate
        | ModTarget::WtLfoRate
        | ModTarget::WtLfoSkew
        | ModTarget::WarpLfoRate
        | ModTarget::WarpLfoSkew
        | ModTarget::FilterLfoSkew(_)
        | ModTarget::LfoParam(_, ModulatorParam::Rate | ModulatorParam::Skew) => 0.5,
        ModTarget::PhaserSweep
        | ModTarget::PulseWidthLfoDepth
        | ModTarget::WtLfoDepth
        | ModTarget::WarpLfoDepth
        | ModTarget::FilterLfoDepth(_)
        | ModTarget::LfoParam(_, ModulatorParam::Depth | ModulatorParam::Curve) => 1.0,
        ModTarget::TremoloShape
        | ModTarget::FilterLfoDc(_)
        | ModTarget::LfoParam(_, ModulatorParam::Dcoffset) => 0.0,
        // The envelope worklet.
        ModTarget::EnvParam(_, EnvelopeParam::Depth) => 1.0,
        ModTarget::EnvParam(_, EnvelopeParam::Attack) => 0.005,
        ModTarget::EnvParam(_, EnvelopeParam::Decay) => 0.14,
        ModTarget::EnvParam(_, EnvelopeParam::Sustain) => 0.0,
        ModTarget::EnvParam(_, EnvelopeParam::Release) => 0.1,
    }
}

/// One-time LFO phase seeding: `frac(time · frequency + phaseoffset)`.
///
/// An LFO is anchored to a clock rather than to the note: it starts wherever
/// a free-running oscillator of that frequency would have been at `time`.
/// Two consequences that are easy to miss. The frequency is the one read on
/// the first run past the begin gate, so modulating the rate moves the
/// starting point too. And a note at `time` 0 seeds to 0 whatever the
/// frequency - which is why a mistake here hides in the first note and only
/// shows up in the second.
///
/// Computed in f64: `time` is a context timestamp, large enough that an f32
/// product loses the fraction that is the whole answer.
#[inline]
/// The accumulator has to hold f64 precision, not just the seed. An LFO
/// adds `frequency/sampleRate` once per sample for the whole life of the
/// voice, and in f32 that walk arrives at the wrap a sample early:
/// simulating `tremolo(6)` at 48 kHz, f64 crosses 1.0 after 8000 steps and
/// f32 after 8001. The wrap is a discontinuity in every shape but the sine,
/// so landing it on the wrong sample puts a spurious step in the gain -
/// measured as a 4.6x spike two samples wide, once per LFO cycle.
///
/// The wavetable phases are f64 for the same reason.
pub(super) fn lfo_phase0(time_secs: f32, frequency_hz: f32, phase_offset: f32) -> f64 {
    (f64::from(time_secs) * f64::from(frequency_hz) + f64::from(phase_offset)).rem_euclid(1.0)
}

/// Unipolar LFO waveshapes on phase in [0, 1]; phase stays f64. The walks
/// wrap on a strict `>`, so a phase of exactly 1.0 is stored and must read as
/// the start of the next cycle.
///
/// Casting it down first is not a rounding nicety: an f64 phase just under 1.0
/// does not wrap, but rounds to EXACTLY 1.0 in f32, and `tri` at the default
/// tremolo skew of 1 then takes its `phase >= skew` branch and divides by
/// `1 - skew`. `Infinity - Infinity` is NaN, and 14 of them reached the output
/// of `s("sawtooth").tremolo(4).tremolodepth(0.8)`.
pub(super) fn lfo_waveshape(shape: u8, phase: f64, skew: f32) -> f32 {
    lfo_waveshape_f64(shape, phase, skew) as f32
}

/// [`lfo_waveshape`] before the cast to f32.
pub(super) fn lfo_waveshape_f64(shape: u8, phase: f64, skew: f32) -> f64 {
    let skew = f64::from(skew);
    match shape {
        // tri
        0 => {
            let x = 1.0 - skew;
            // The same guards the f32 twin below carries. Keeping phase in
            // f64 stopped it ROUNDING to 1.0, but the walk wraps on a strict
            // `>`, so a rate whose ratio to the sample rate is dyadic
            // (375 Hz at 48 kHz = 2^-7) sums to EXACTLY 1.0 and stores it.
            // At the default tremolo skew of 1 that made `1/x - phase/x`
            // into `inf - inf` = NaN for one sample per LFO cycle, smeared
            // through everything downstream. The ramp's value at phase 1 is
            // its limit: 0.
            if phase >= skew {
                if x <= 0.0 { 0.0 } else { 1.0 / x - phase / x }
            } else if skew <= 0.0 {
                0.0
            } else {
                phase / skew
            }
        }
        // sine
        1 => (std::f64::consts::TAU * phase).sin() * 0.5 + 0.5,
        // ramp
        2 => phase,
        // saw
        3 => 1.0 - phase,
        // square
        4 => {
            if phase >= skew {
                0.0
            } else {
                1.0
            }
        }
        _ => 0.0,
    }
}

/// Envelope curve warp: curvature −1..1 bends the segment phase.
fn env_mod_warp(phase: f32, curvature: f32) -> f32 {
    const STRENGTH: f32 = 8.0;
    if phase == 0.0 || phase == 1.0 {
        return phase;
    }
    if curvature > 0.0 {
        let exp = 1.0 + STRENGTH * curvature;
        1.0 - (1.0 - phase).powf(exp)
    } else {
        let exp = 1.0 - STRENGTH * curvature;
        phase.powf(exp)
    }
}

/// `env()` modulator value at `t` seconds after the trigger, before
/// depth/min/max. Elapsed time divides by each state's CUMULATIVE threshold
/// (attack, attack+decay, susTime, susTime+release), so later segments
/// enter mid-phase with a value jump - deliberate, part of the sound.
pub(super) fn env_mod_value(env: &crate::backend::EnvMod, t: f32) -> f32 {
    let a = env.attack_secs;
    let d = env.decay_secs;
    let sus_time = env.sustain_secs;
    let r = env.release_secs;
    let seg = |start: f32, target: f32, time: f32, curve: f32| -> f32 {
        if time == 0.0 || start == target {
            target
        } else {
            let phase = (t / time).min(1.0);
            start + (target - start) * env_mod_warp(phase, curve)
        }
    };
    if t < a {
        seg(0.0, 1.0, a, env.a_curve)
    } else if t < a + d {
        seg(1.0, env.sustain, a + d, env.d_curve)
    } else if t < sus_time {
        env.sustain
    } else if t < sus_time + r {
        seg(env.sustain, 0.0, sus_time + r, env.r_curve)
    } else {
        0.0
    }
}

#[inline]
/// Whether the LFO begin gate is open for this quantum.
///
/// No output or phase advance occurs until the first quantum strictly past
/// the f32-rounded begin time. Comparing it with the f64 render clock retains
/// AudioParam timing semantics: rounding can decide whether the onset's own
/// quantum runs.
pub(super) fn lfo_quantum_open(quantum_start: u64, sample_rate: f64, begin_secs: f32) -> bool {
    (quantum_start as f64) / sample_rate > f64::from(begin_secs)
}

/// The first render quantum the begin gate opens on, as a frame.
///
/// For the one source that wants its gate precomputed rather than tested per
/// sample. Same rule as `lfo_quantum_open`, so it lands on the same quantum.
pub(super) fn first_open_quantum(begin_secs: f32, sample_rate: f64) -> u64 {
    let floor = (f64::from(begin_secs) * sample_rate / 128.0).max(0.0) as u64 * 128;
    if lfo_quantum_open(floor, sample_rate, begin_secs) {
        floor
    } else {
        floor + 128
    }
}

/// Whether this quantum begins before the f32-rounded LFO end time.
pub(super) fn lfo_quantum_alive(quantum_start: u64, sample_rate: f64, end_secs: f32) -> bool {
    (quantum_start as f64) / sample_rate < f64::from(end_secs)
}

/// LFO waveshapes, unipolar 0..1.
pub(super) fn lfo_shape(shape: u8, phase: f32, skew: f32) -> f32 {
    match shape {
        0 => {
            // tri with skew
            let x = 1.0 - skew;
            if phase >= skew {
                if x <= 0.0 { 0.0 } else { 1.0 / x - phase / x }
            } else if skew <= 0.0 {
                0.0
            } else {
                phase / skew
            }
        }
        1 => ((std::f32::consts::TAU * phase).sin()) * 0.5 + 0.5,
        2 => phase,
        3 => 1.0 - phase,
        4 => {
            if phase >= skew {
                0.0
            } else {
                1.0
            }
        }
        _ => phase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod lfo_waveshape_tests {
        /// `tremolo` defaults to skew 1, and `tri` divides by `1 - skew` on its
        /// `phase >= skew` branch. The phase reaches exactly 1.0 two ways: the
        /// walks wrap on a strict `>`, so a dyadic step (375 Hz at 48 kHz adds
        /// 2^-7) sums to 1.0 and stores it, and f32 rounds everything above
        /// `1 - 2^-25` up to 1.0. Either way the branch divided by zero and
        /// `Infinity - Infinity` put NaN into the output.
        #[test]
        fn a_phase_just_under_one_never_divides_by_a_zero_skew() {
            for phase in [
                1.0,
                1.0 - f64::EPSILON,
                0.999_999_999_9,
                0.999_999_999_999_999_9,
                1.0 - 1e-12,
                1.0 - 1e-15,
            ] {
                let value = super::lfo_waveshape(0, phase, 1.0);
                assert!(
                    value.is_finite(),
                    "tri({phase}, skew 1) produced {value}, which reaches the output"
                );
            }

            // Nothing else about the shape may move: skew 1 makes it a plain ramp.
            for phase in [0.0_f64, 0.25, 0.5, 0.75] {
                let value = super::lfo_waveshape(0, phase, 1.0);
                assert!(
                    (f64::from(value) - phase).abs() < 1e-6,
                    "tri({phase}, skew 1) should be the phase itself, got {value}"
                );
            }

            // And the other shapes stay finite wherever the phase can land.
            for shape in 0..5u8 {
                for phase in [0.0_f64, 0.5, 1.0 - f64::EPSILON, 1.0] {
                    for skew in [0.0_f32, 0.5, 1.0] {
                        let value = super::lfo_waveshape(shape, phase, skew);
                        assert!(
                            value.is_finite() || skew == 0.0,
                            "shape {shape} at phase {phase} skew {skew} gave {value}"
                        );
                    }
                }
            }
        }
    }

    mod lfo_quantum_gate_tests {
        /// The gate compares an f64 against an f32 begin. In `s("ht*3").tremolo(4)`
        /// at 48 kHz, five of the six onset quanta tie and are skipped. The sixth
        /// begin rounds down in f32, so its onset quantum runs.
        #[test]
        fn the_onset_quantum_runs_only_when_f32_rounds_the_begin_down() {
            let sr = 48_000.0;
            let opens_on_its_own_onset = |k: u64| {
                let begin = (2.0 * k as f64 / 3.0) as f32;
                super::lfo_quantum_open(k * 32_000, sr, begin)
            };
            for k in [0, 1, 2, 3, 4] {
                assert!(
                    !opens_on_its_own_onset(k),
                    "note {k}'s begin rounds up, so its onset quantum ties and is skipped"
                );
            }
            assert!(
                opens_on_its_own_onset(5),
                "note 5's begin rounds down, so its onset quantum is already past it"
            );
        }

        /// Whatever the tie does, the quantum after it always runs, and one
        /// before the note never does.
        #[test]
        fn the_gate_is_shut_before_the_note_and_open_after_it() {
            let sr = 48_000.0;
            let begin = (2.0_f64 / 3.0) as f32;
            assert!(!super::lfo_quantum_open(31_872, sr, begin));
            assert!(super::lfo_quantum_open(32_128, sr, begin));
        }

        /// The end is read the same way, and a sample ringing past it is not
        /// modulated.
        #[test]
        fn the_gate_shuts_at_the_f32_end() {
            let sr = 48_000.0;
            // A note at 0 lasting an eighth of a second, with the default 10 ms
            // release: 6480 frames.
            let end = 0.135_f32;
            assert!(super::lfo_quantum_alive(6_400, sr, end));
            assert!(!super::lfo_quantum_alive(6_528, sr, end));
        }
    }

    mod lfo_phase_tests {
        /// An LFO adds `frequency/sampleRate` once per sample for the whole life
        /// of the voice, so the accumulator's precision decides where the wrap
        /// lands - and the wrap is a discontinuity in every shape but the sine.
        #[test]
        fn an_lfo_wraps_on_the_exact_cycle_however_long_the_note_runs() {
            let sr = 48_000.0_f64;
            let freq = 6.0_f32; // tremolo(6): exactly 8000 samples per cycle

            let mut phase = super::lfo_phase0(0.0, freq, 0.0);
            assert_eq!(phase, 0.0, "a note at cycle 0 starts the LFO at phase 0");

            let mut wraps = Vec::new();
            for n in 0..48_000u32 {
                let next = phase + f64::from(freq) / sr;
                phase = if next > 1.0 {
                    wraps.push(n + 1);
                    next - 1.0
                } else {
                    next
                };
            }

            // Six cycles per second, and not one sample of drift by the last of
            // them. In f32 this walks off by a sample per cycle: simulated, f64
            // crosses 1.0 after 8000 steps and f32 after 8001, which showed up
            // against Chromium as a 4.6x gain spike two samples wide, once per
            // cycle, on `s("sawtooth").tremolo(6)`.
            assert_eq!(
                wraps,
                vec![8_000, 16_000, 24_000, 32_000, 40_000, 48_000],
                "the LFO drifted off its cycle"
            );
        }
    }

    mod modulator_tests {
        use super::{env_mod_value, env_mod_warp, lfo_waveshape};
        use crate::backend::{EnvMod, ModTarget};

        fn env(a: f32, d: f32, s: f32, r: f32, sus_time: f32) -> EnvMod {
            EnvMod {
                fxi: None,
                target: ModTarget::LowpassFreq,
                attack_secs: a,
                decay_secs: d,
                sustain: s,
                release_secs: r,
                a_curve: 0.0,
                d_curve: 0.0,
                r_curve: 0.0,
                depth: 1.0,
                min: -1e9,
                max: 1e9,
                sustain_secs: sus_time,
                param_base: 500.0,
                id: None,
            }
        }

        #[test]
        fn waveshapes_match_worklets() {
            // tri peaks at skew, hits 0 at both ends
            assert_eq!(lfo_waveshape(0, 0.0, 0.5), 0.0);
            assert_eq!(lfo_waveshape(0, 0.5, 0.5), 1.0);
            assert!((lfo_waveshape(0, 1.0, 0.5)).abs() < 1e-6);
            // skewed tri: peak moves to skew
            assert_eq!(lfo_waveshape(0, 0.25, 0.25), 1.0);
            // sine is 0.5 at phase 0, 1 at quarter
            assert!((lfo_waveshape(1, 0.0, 0.5) - 0.5).abs() < 1e-6);
            assert!((lfo_waveshape(1, 0.25, 0.5) - 1.0).abs() < 1e-6);
            // ramp/saw are complements
            assert_eq!(lfo_waveshape(2, 0.3, 0.5), 0.3);
            assert_eq!(lfo_waveshape(3, 0.3, 0.5), 0.7);
            // square: 1 below skew, 0 at/above
            assert_eq!(lfo_waveshape(4, 0.49, 0.5), 1.0);
            assert_eq!(lfo_waveshape(4, 0.5, 0.5), 0.0);
        }

        #[test]
        fn env_warp_is_identity_at_zero_curvature() {
            for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
                assert!((env_mod_warp(p, 0.0) - p).abs() < 1e-6);
            }
            // positive curvature = snappier (above identity mid-segment)
            assert!(env_mod_warp(0.5, 0.5) > 0.5);
            assert!(env_mod_warp(0.5, -0.5) < 0.5);
        }

        #[test]
        fn env_value_uses_cumulative_time_divisors() {
            // a=1 d=1 s=0.5, susTime=10: the envelope divides elapsed by the
            // CUMULATIVE threshold, so decay enters mid-phase with a jump.
            let e = env(1.0, 1.0, 0.5, 0.1, 10.0);
            assert!((env_mod_value(&e, 0.5) - 0.5).abs() < 1e-6); // attack midpoint
            // just after entering decay: phase = 1.0/(1+1) = 0.5 of the way
            // from 1 toward 0.5 already
            let just_in_decay = env_mod_value(&e, 1.0001);
            assert!((just_in_decay - 0.75).abs() < 1e-3);
            // decay endpoint lands on sustain
            assert!((env_mod_value(&e, 1.9999) - 0.5).abs() < 1e-3);
            // sustain holds to susTime
            assert!((env_mod_value(&e, 5.0) - 0.5).abs() < 1e-6);
            // past susTime+release: idle 0
            assert_eq!(env_mod_value(&e, 10.2), 0.0);
        }

        #[test]
        fn env_zero_attack_snaps_to_peak_then_decays() {
            let e = env(0.0, 1.0, 0.0, 0.1, 10.0);
            // t inside decay, cumulative divisor is a+d = 1
            assert!((env_mod_value(&e, 0.5) - 0.5).abs() < 1e-3);
            assert!(env_mod_value(&e, 0.9999) < 1e-3);
        }
    }
}
