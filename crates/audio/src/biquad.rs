//! Biquad and ladder filter stages for the scalar renderer.
//!
//! Biquad coefficients and double-precision state updates preserve the
//! Web Audio response, including its cutoff and resonance boundary cases.

use crate::{FilterControls, FilterEnvelope, FilterStages, StaticBiquad};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Lowpass,
    Highpass,
    Bandpass,
}

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn new(kind: Kind, controls: StaticBiquad, sample_rate: u32) -> Self {
        let mut filter = Self::default();
        filter.configure(kind, controls, sample_rate);
        filter
    }

    fn configure(&mut self, kind: Kind, controls: StaticBiquad, sample_rate: u32) {
        let frequency =
            (f64::from(controls.frequency_hz) / (f64::from(sample_rate) * 0.5)).clamp(0.0, 1.0);
        let q = f64::from(controls.q);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            Kind::Lowpass if frequency == 1.0 => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            Kind::Lowpass if frequency > 0.0 => {
                let resonance = 10.0f64.powf(q / 20.0);
                let theta = std::f64::consts::PI * frequency;
                let alpha = theta.sin() / (2.0 * resonance);
                let cosine = theta.cos();
                let beta = (1.0 - cosine) * 0.5;
                (
                    beta,
                    2.0 * beta,
                    beta,
                    1.0 + alpha,
                    -2.0 * cosine,
                    1.0 - alpha,
                )
            }
            Kind::Lowpass => (0.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            Kind::Highpass if frequency == 1.0 => (0.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            Kind::Highpass if frequency > 0.0 => {
                let resonance = 10.0f64.powf(q / 20.0);
                let theta = std::f64::consts::PI * frequency;
                let alpha = theta.sin() / (2.0 * resonance);
                let cosine = theta.cos();
                let beta = (1.0 + cosine) * 0.5;
                (
                    beta,
                    -2.0 * beta,
                    beta,
                    1.0 + alpha,
                    -2.0 * cosine,
                    1.0 - alpha,
                )
            }
            Kind::Highpass => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            Kind::Bandpass if frequency > 0.0 && frequency < 1.0 && q > 0.0 => {
                let theta = std::f64::consts::PI * frequency;
                let alpha = theta.sin() / (2.0 * q);
                let cosine = theta.cos();
                (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cosine, 1.0 - alpha)
            }
            Kind::Bandpass if frequency > 0.0 && frequency < 1.0 => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            Kind::Bandpass => (0.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        };
        let inverse_a0 = a0.recip();
        self.b0 = b0 * inverse_a0;
        self.b1 = b1 * inverse_a0;
        self.b2 = b2 * inverse_a0;
        self.a1 = a1 * inverse_a0;
        self.a2 = a2 * inverse_a0;
    }

    fn process(&mut self, input: f32) -> f32 {
        let input = f64::from(input);
        let output = self.b0 * input + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        // Flush subnormals: without this, IIR feedback decays into them
        // after minutes of quiet filtered playback and the callback cost
        // spikes into audible glitches.
        self.y1 = if output.is_subnormal() { 0.0 } else { output };
        self.y1 as f32
    }
}

/// The ladder's rational tanh approximation - part of its sound, not a
/// shortcut; do not swap for `tanh`.
#[inline]
fn fast_tanh(x: f64) -> f64 {
    let x2 = x * x;
    (x * (27.0 + x2)) / (27.0 + 9.0 * x2)
}

/// Ladder filter: 4 cascaded one-pole tanh stages with feedback
/// `k = min(8, q·0.13)` from a 4-tap weighted output history, and makeup
/// gain `(1/drive)·min(1.75, 1 + k)`.
#[derive(Clone, Copy, Debug, Default)]
struct Ladder {
    p: [f64; 4],
    history: [f64; 3],
    /// Cutoff and Q latched at each 128-frame boundary, so modulation changes
    /// once per block. Until the first boundary, `None` selects static values.
    latch: Option<(f32, f32)>,
    until_block: u8,
}

impl Ladder {
    #[inline]
    fn process(&mut self, x: f32, cutoff_hz: f32, q: f32, drive: f64, sample_rate: u32) -> f32 {
        // A negative cutoff makes the four stages diverge.
        let cutoff_hz = f64::from(cutoff_hz.max(0.0));
        let cutoff = (cutoff_hz * std::f64::consts::TAU / f64::from(sample_rate)).min(1.0);
        // Below -7.69 the feedback gain passes 1 and the four stages diverge.
        let k = (f64::from(q.max(-7.0)) * 0.13).min(8.0);
        let makeup = (1.0 / drive) * (1.0 + k).min(1.75);
        let out = self.p[3] * 0.360891
            + self.history[0] * 0.41729
            + self.history[1] * 0.177896
            + self.history[2] * 0.0439725;
        self.history[2] = self.history[1];
        self.history[1] = self.history[0];
        self.history[0] = self.p[3];
        self.p[0] += (fast_tanh(f64::from(x) * drive - k * out) - fast_tanh(self.p[0])) * cutoff;
        self.p[1] += (fast_tanh(self.p[0]) - fast_tanh(self.p[1])) * cutoff;
        self.p[2] += (fast_tanh(self.p[1]) - fast_tanh(self.p[2])) * cutoff;
        self.p[3] += (fast_tanh(self.p[2]) - fast_tanh(self.p[3])) * cutoff;
        (out * makeup) as f32
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterStage {
    kind: Kind,
    controls: StaticBiquad,
    /// The static cutoff before any per-frame modulation, kept because the
    /// envelope path mutates `controls.frequency_hz` in place.
    base_hz: f32,
    /// The static Q before any per-frame modulation.
    base_q: f32,
    envelope: Option<FilterEnvelope>,
    first: Biquad,
    second: Option<Biquad>,
    /// Ladder model replaces the biquads entirely.
    ladder: Option<Ladder>,
    /// Precomputed `clamp(exp(drive), 0.1, 2000)` for the ladder.
    drive: f64,
    sample_rate: u32,
}

impl FilterStage {
    fn new(
        kind: Kind,
        controls: StaticBiquad,
        envelope: Option<FilterEnvelope>,
        stages: FilterStages,
        drive: f32,
        sample_rate: u32,
    ) -> Self {
        Self {
            kind,
            controls,
            base_hz: controls.frequency_hz,
            base_q: controls.q,
            envelope,
            first: Biquad::new(kind, controls, sample_rate),
            second: matches!(stages, FilterStages::Two)
                .then(|| Biquad::new(kind, controls, sample_rate)),
            ladder: matches!(stages, FilterStages::Ladder).then(Ladder::default),
            drive: f64::from(drive).exp().clamp(0.1, 2000.0),
            sample_rate,
        }
    }

    /// One exponential segment, `start · (end/start)^progress`.
    ///
    /// Nonzero endpoints with the same sign give a positive base for `powf`.
    /// Otherwise hold the start value until the segment ends, then step to
    /// the target. This preserves Web Audio's exponential-ramp boundary
    /// semantics; interpolating across zero would change the envelope.
    fn exponential_ramp(start: f64, end: f64, progress: f64) -> f64 {
        let progress = progress.clamp(0.0, 1.0);
        if !start.is_finite() || !end.is_finite() {
            // Keep non-finite envelope values out of the frequency path.
            return if end.is_finite() { end } else { 0.0 };
        }
        if start != 0.0 && end / start > 0.0 {
            start * (end / start).powf(progress)
        } else if progress >= 1.0 {
            end
        } else {
            start
        }
    }

    pub(crate) fn frequency_at(envelope: FilterEnvelope, t: f32, gate_secs: f32) -> f32 {
        let min = if envelope.min_hz == 0.0 {
            0.001
        } else {
            envelope.min_hz
        };
        let max = if envelope.max_hz == 0.0 {
            0.001
        } else {
            envelope.max_hz
        };
        let attack = f64::from(envelope.attack_secs);
        let decay = f64::from(envelope.decay_secs);
        let release = f64::from(envelope.release_secs);
        // Replace zero targets with 0.001 so exponential segments can ramp
        // to them instead of holding. Compute the target in f64: pitch
        // envelope bounds can cancel exactly, and f32 rounding can leave a
        // residue that bypasses this replacement.
        let non_zero = |value: f64| if value == 0.0 { 0.001 } else { value };
        let sustain = non_zero(min + envelope.sustain * (max - min));
        let gate = f64::from(gate_secs.max(0.0));
        let t = f64::from(t.max(0.0));

        let gate_value = non_zero(if gate < attack {
            min + (max - min) * gate / attack
        } else if gate < attack + decay {
            max + (sustain - max) * (gate - attack) / decay
        } else {
            sustain
        });

        let value = if t >= gate {
            if release <= 0.0 || t >= gate + release {
                min
            } else {
                Self::exponential_ramp(gate_value, min, (t - gate) / release)
            }
        } else if gate < attack {
            Self::exponential_ramp(min, gate_value, t / gate.max(f64::MIN_POSITIVE))
        } else if t < attack {
            Self::exponential_ramp(min, max, t / attack)
        } else if gate < attack + decay {
            Self::exponential_ramp(max, gate_value, (t - attack) / (gate - attack))
        } else if t < attack + decay {
            Self::exponential_ramp(max, sustain, (t - attack) / decay)
        } else {
            sustain
        };
        value as f32
    }

    /// `add_hz`/`add_q` are the summed LFO/env modulator outputs riding the
    /// frequency and Q params this frame; modulation ADDS to a param's
    /// intrinsic value.
    fn process(&mut self, mut sample: f32, t: f32, gate_secs: f32, add_hz: f32, add_q: f32) -> f32 {
        let modulated = self.envelope.is_some() || add_hz != 0.0 || add_q != 0.0;
        if let Some(ladder) = &mut self.ladder {
            if ladder.until_block == 0 {
                let frequency = if modulated {
                    let base = match self.envelope {
                        Some(envelope) => Self::frequency_at(envelope, t, gate_secs),
                        None => self.base_hz,
                    };
                    // A NaN sum gives the worklet param its default.
                    let sum = base + add_hz;
                    if sum.is_nan() { 500.0 } else { sum }
                } else {
                    self.base_hz
                };
                ladder.latch = Some((frequency, self.base_q + add_q));
                ladder.until_block = 128;
            }
            ladder.until_block -= 1;
            let (frequency, q) = ladder.latch.unwrap_or((self.base_hz, self.base_q));
            return ladder.process(sample, frequency, q, self.drive, self.sample_rate);
        }
        if modulated {
            let base = match self.envelope {
                Some(envelope) => Self::frequency_at(envelope, t, gate_secs),
                None => self.base_hz,
            };
            // A NaN sum gives the BiquadFilterNode param its default.
            let sum = base + add_hz;
            self.controls.frequency_hz = if sum.is_nan() { 350.0 } else { sum };
            self.controls.q = self.base_q + add_q;
            self.first
                .configure(self.kind, self.controls, self.sample_rate);
            if let Some(second) = &mut self.second {
                second.configure(self.kind, self.controls, self.sample_rate);
            }
        } else if self.controls.frequency_hz != self.base_hz || self.controls.q != self.base_q {
            // Restore static coefficients when modulation ends. A filter's
            // own LFO stops at the note's hold end, while the voice can keep
            // sounding through its release tail.
            self.controls.frequency_hz = self.base_hz;
            self.controls.q = self.base_q;
            self.first
                .configure(self.kind, self.controls, self.sample_rate);
            if let Some(second) = &mut self.second {
                second.configure(self.kind, self.controls, self.sample_rate);
            }
        }
        sample = self.first.process(sample);
        if let Some(second) = &mut self.second {
            sample = second.process(sample);
        }
        sample
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FilterChain {
    lowpass: Option<FilterStage>,
    highpass: Option<FilterStage>,
    bandpass: Option<FilterStage>,
}

impl FilterChain {
    pub(crate) fn new(controls: FilterControls, sample_rate: u32) -> Self {
        Self {
            lowpass: controls.lowpass.map(|filter| {
                FilterStage::new(
                    Kind::Lowpass,
                    filter,
                    controls.lowpass_envelope,
                    controls.stages,
                    controls.drive,
                    sample_rate,
                )
            }),
            highpass: controls.highpass.map(|filter| {
                FilterStage::new(
                    Kind::Highpass,
                    filter,
                    controls.highpass_envelope,
                    controls.stages,
                    controls.drive,
                    sample_rate,
                )
            }),
            bandpass: controls.bandpass.map(|filter| {
                FilterStage::new(
                    Kind::Bandpass,
                    filter,
                    controls.bandpass_envelope,
                    controls.stages,
                    controls.drive,
                    sample_rate,
                )
            }),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.lowpass.is_some() || self.highpass.is_some() || self.bandpass.is_some()
    }

    /// Move the intrinsic cutoff while retaining the filter's delay state.
    /// Once the ramp settles, the ordinary static path stops rebuilding its
    /// coefficients. Envelopes own their cutoff and are excluded by binding.
    pub(crate) fn set_lowpass_frequency(&mut self, frequency: f32) {
        if let Some(filter) = &mut self.lowpass
            && filter.envelope.is_none()
        {
            filter.base_hz = frequency;
        }
    }

    /// Sets the base resonance. A cutoff envelope does not change the
    /// resonance.
    pub(crate) fn set_lowpass_q(&mut self, q: f32) {
        if let Some(filter) = &mut self.lowpass {
            filter.base_q = q;
        }
    }

    /// Align block-rate behaviour to the render clock: `first_frame` is the
    /// absolute frame this chain first processes.
    pub(crate) fn set_block_phase(&mut self, first_frame: u64) {
        let until = ((128 - (first_frame % 128)) % 128) as u8;
        for stage in [
            self.lowpass.as_mut(),
            self.highpass.as_mut(),
            self.bandpass.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(ladder) = &mut stage.ladder {
                ladder.until_block = until;
            }
        }
    }

    /// Apply low-pass, high-pass and band-pass stages in order. `adds` and
    /// `q_adds` hold their summed cutoff (Hz) and resonance modulation.
    pub(crate) fn process_modulated(
        &mut self,
        mut sample: f32,
        t: f32,
        gate_secs: f32,
        adds: [f32; 3],
        q_adds: [f32; 3],
    ) -> f32 {
        for (filter, add, q_add) in [
            (self.lowpass.as_mut(), adds[0], q_adds[0]),
            (self.highpass.as_mut(), adds[1], q_adds[1]),
            (self.bandpass.as_mut(), adds[2], q_adds[2]),
        ] {
            if let Some(filter) = filter {
                sample = filter.process(sample, t, gate_secs, add, q_add);
            }
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When a modulator ends before its voice, the param returns to its
    /// intrinsic value and does not hold the last modulated value. A filter's
    /// own LFO ends at the note's hold end, so every release tail is such a
    /// case.
    #[test]
    fn a_filter_returns_to_its_base_when_modulation_stops() {
        let make = || {
            FilterStage::new(
                Kind::Lowpass,
                StaticBiquad {
                    frequency_hz: 700.0,
                    q: 1.0,
                },
                None,
                FilterStages::One,
                0.0,
                48_000,
            )
        };
        let mut modulated = make();
        let mut plain = make();

        // A saw at 55 Hz, so there is real content either side of the cutoff.
        let input = |n: usize| ((n as f32 * 55.0 / 48_000.0).fract() * 2.0 - 1.0) * 0.5;

        // 4000 samples with the cutoff swung a long way, then it stops dead.
        for n in 0..4_000 {
            let add = (n as f32 * 0.01).sin() * 240.0;
            modulated.process(input(n), 0.0, 1.0, add, 0.0);
            plain.process(input(n), 0.0, 1.0, 0.0, 0.0);
        }

        // Both filters now see identical input at identical cutoffs. Whatever
        // state the swing left behind decays in a few hundred samples; if the
        // coefficients stayed detuned the two never converge at all.
        let mut worst = 0.0f32;
        for n in 4_000..8_000 {
            let a = modulated.process(input(n), 0.0, 1.0, 0.0, 0.0);
            let b = plain.process(input(n), 0.0, 1.0, 0.0, 0.0);
            if n >= 5_000 {
                worst = worst.max((a - b).abs());
            }
        }
        assert!(
            worst < 1e-4,
            "a filter whose modulation stopped never came back to its base: worst {worst}"
        );
    }

    #[test]
    fn a_ladder_with_a_negative_cutoff_or_resonance_stays_finite() {
        for (add_hz, add_q) in [(-5_000.0, 0.0), (0.0, -5_000.0)] {
            let mut ladder = FilterStage::new(
                Kind::Lowpass,
                StaticBiquad {
                    frequency_hz: 800.0,
                    q: 1.0,
                },
                None,
                FilterStages::Ladder,
                0.0,
                48_000,
            );
            for n in 0..48_000 {
                let input = ((n as f32 * 55.0 / 48_000.0).fract() * 2.0 - 1.0) * 0.5;
                let output = ladder.process(input, 0.0, 1.0, add_hz, add_q);
                assert!(
                    output.is_finite(),
                    "add_hz {add_hz}, add_q {add_q}: sample {n} is {output}"
                );
            }
        }
    }

    #[test]
    fn webaudio_frequency_boundaries_are_not_generic_clamps() {
        let mut low_at_zero = Biquad::new(
            Kind::Lowpass,
            StaticBiquad {
                frequency_hz: 0.0,
                q: 1.0,
            },
            48_000,
        );
        let mut high_at_zero = Biquad::new(
            Kind::Highpass,
            StaticBiquad {
                frequency_hz: 0.0,
                q: 1.0,
            },
            48_000,
        );
        let mut band_at_nyquist = Biquad::new(
            Kind::Bandpass,
            StaticBiquad {
                frequency_hz: 24_000.0,
                q: 1.0,
            },
            48_000,
        );
        assert_eq!(low_at_zero.process(1.0), 0.0);
        assert_eq!(high_at_zero.process(1.0), 1.0);
        assert_eq!(band_at_nyquist.process(1.0), 0.0);
    }

    #[test]
    fn long_quiet_lowpass_does_not_enter_subnormals() {
        let mut low = Biquad::new(
            Kind::Lowpass,
            StaticBiquad {
                frequency_hz: 200.0,
                q: 1.0,
            },
            48_000,
        );
        let _ = low.process(1.0);
        for _ in 0..200_000 {
            let _ = low.process(0.0);
        }
        assert!(
            !low.y1.is_subnormal() && !low.y2.is_subnormal(),
            "biquad feedback decayed into subnormals: y1={} y2={}",
            low.y1,
            low.y2
        );
    }

    /// `penv(12).pdecay(0.2).pcurve(1)`: `min + sustain*(max-min)` cancels to
    /// exactly zero, so the `val || 0.001` rewrite applies and the decay runs
    /// from 1198.8 to 0.001. In f32 the sum can miss zero and skip the rewrite.
    #[test]
    fn an_exponential_pitch_envelope_decays_to_the_rewritten_floor() {
        let cents = 1200.0_f64;
        let panchor = 0.001_f64; // psustain's floor, which panchor defaults to
        let envelope = FilterEnvelope {
            attack_secs: 0.001,
            decay_secs: 0.2,
            sustain: 0.001,
            release_secs: 0.01,
            min_hz: 0.0 - cents * panchor,
            max_hz: cents - cents * panchor,
        };
        assert_eq!(
            envelope.min_hz + envelope.sustain * (envelope.max_hz - envelope.min_hz),
            0.0,
            "the decay target must cancel exactly, or the 0.001 rewrite cannot fire"
        );

        let max = envelope.max_hz;
        for progress in [0.25_f64, 0.5, 0.75] {
            let t = (0.001 + progress * 0.2) as f32;
            let got = f64::from(FilterStage::frequency_at(envelope, t, 0.5));
            let want = max * (0.001 / max).powf(progress);
            assert!(
                (got - want).abs() < 0.2 * want,
                "at {progress} of the decay expected ~{want:.4} cents, got {got:.6}"
            );
        }

        // Halfway is the geometric mean, about 1.09 cents. An inexact
        // cancellation gives about 0.008 instead, two orders of magnitude low.
        let midpoint = f64::from(FilterStage::frequency_at(envelope, 0.101, 0.5));
        assert!(
            midpoint > 0.5,
            "the decay collapsed to the floor far too early: {midpoint}"
        );
    }

    #[test]
    fn exponential_pitch_envelope_across_zero_stays_finite() {
        let envelope = FilterEnvelope {
            attack_secs: 0.2,
            decay_secs: 0.001,
            sustain: 1.0,
            release_secs: 0.001,
            // penv(7), panchor defaulting to sustain: min = -700, max = 0
            // with the `max == 0 → 0.001` rewrite already applied.
            min_hz: -700.0,
            max_hz: 0.001,
        };
        let gate = 0.5_f32;
        let mut swept = false;
        for step in 0..=200 {
            let t = step as f32 * gate * 1.5 / 200.0;
            let cents = FilterStage::frequency_at(envelope, t, gate);
            assert!(
                cents.is_finite(),
                "pitch envelope produced {cents} at t={t}; a NaN here silences the voice"
            );
            // The ramp must actually travel, not sit pinned at one end.
            if cents > -650.0 {
                swept = true;
            }
        }
        assert!(swept, "envelope never left its floor, so penv did nothing");
    }

    /// Same-sign endpoints keep the true exponential shape, which is what
    /// the filters rely on - the fallback must not flatten those to linear.
    #[test]
    fn exponential_ramp_keeps_its_curve_for_same_sign_endpoints() {
        let midpoint = FilterStage::exponential_ramp(100.0, 10_000.0, 0.5);
        assert!(
            (midpoint - 1000.0).abs() < 1e-6,
            "expected the geometric mean 1000, got {midpoint}"
        );
        let linear_midpoint = 100.0 + (10_000.0 - 100.0) * 0.5;
        assert!(
            (midpoint - linear_midpoint).abs() > 1.0,
            "exponential ramp collapsed to the linear one"
        );
    }
}
