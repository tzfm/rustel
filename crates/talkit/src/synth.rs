//! The formant synthesizer: spans and a pitch plan become samples.
//!
//! Each sample runs through the classic cascade arrangement:
//!
//! ```text
//! glottal source ──> R1 ──> R2 ──> R3 ──> R4 ──> R5 ──┐
//!        │                                            +──> out
//!        └──> nasal resonator ──(nasal mix)───────────┤
//! white noise ──> frication resonator ──(amp) ────────┘
//! ```
//!
//! Formant frequencies, voicing, frication and the nasal amount glide from
//! each span's target to the next over [`TRANSITION_MS`], which is what turns
//! the target table into connected speech.

use crate::params::Effort;
use crate::prosody::Plan;
use crate::utterance::{Span, Utterance};

use crate::RATE;

/// How long the glide from one span's target to the next takes, in ms.
const TRANSITION_MS: f32 = 40.0;
/// Fixed upper formants of the cascade, in Hz with their bandwidths.
const F4: (f32, f32) = (3350.0, 250.0);
const F5: (f32, f32) = (3850.0, 340.0);
const NASAL: (f32, f32) = (270.0, 120.0);
/// Open quotient of the glottal pulse: the share of each cycle the folds are
/// open. Breathy speech holds them open longer.
const OPEN_QUOTIENT: f32 = 0.62;
const OPEN_QUOTIENT_BREATHY: f32 = 0.8;
/// Overall gain before the render pass normalizes the peak.
const GAIN: f32 = 0.6;
/// The noise branch against the voiced one. It puts a sibilant about 12 dB
/// under the vowels around it, as in natural speech, where an equal level
/// sounds like a hiss on top of the voice.
const FRICATION_GAIN: f32 = 0.5;

/// One two-pole resonator.
struct Resonator {
    a: f32,
    b: f32,
    c: f32,
    y1: f32,
    y2: f32,
    /// The center and bandwidth the coefficients hold, so a held target
    /// costs no recomputation.
    tuned: (f32, f32),
}

impl Resonator {
    fn new() -> Self {
        Self {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            y1: 0.0,
            y2: 0.0,
            tuned: (f32::NAN, f32::NAN),
        }
    }

    fn set(&mut self, hz: f32, bw: f32) {
        if self.tuned == (hz, bw) {
            return;
        }
        self.tuned = (hz, bw);
        let r = (-std::f32::consts::PI * bw / RATE as f32).exp();
        let theta = std::f32::consts::TAU * hz / RATE as f32;
        self.c = -r * r;
        self.b = 2.0 * r * theta.cos();
        self.a = 1.0 - self.b - self.c;
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.a.mul_add(x, self.b.mul_add(self.y1, self.c * self.y2));
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Deterministic white noise source. Fixed seed: the same text renders the
/// same wave on every machine.
struct Noise(u32);

impl Noise {
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        ((self.0 >> 9) as f32 / 8388608.0) - 1.0
    }
}

/// Render the utterance. Returns mono f32 samples at [`crate::RATE`].
pub fn synthesize(utterance: &Utterance, plan: &Plan, effort: Effort) -> Vec<f32> {
    let frames = (utterance.total_ms * RATE as f32 / 1000.0) as usize + 1;
    let mut out = Vec::with_capacity(frames);
    let spans = &utterance.spans;

    let mut r: [Resonator; 5] = std::array::from_fn(|_| Resonator::new());
    r[3].set(F4.0, F4.1);
    r[4].set(F5.0, F5.1);
    let mut nasal_res = Resonator::new();
    let mut fric_res = Resonator::new();

    let mut phase = 0.0f32;
    let mut noise = Noise(0x5eed);

    for n in 0..frames {
        let ms = n as f32 * 1000.0 / RATE as f32;
        let i = span_at(spans, ms);
        let f1 = track(spans, i, ms, |f| f.f1);
        let f2 = track(spans, i, ms, |f| f.f2);
        let f3 = track(spans, i, ms, |f| f.f3);
        let voice = track(spans, i, ms, |f| f.voice);
        let fric_hz = track(spans, i, ms, |f| f.fric_hz);
        let fric_amp = track(spans, i, ms, |f| f.fric_amp);
        let nasal = track(spans, i, ms, |f| f.nasal);

        // Voiced source.
        let mut source = 0.0;
        if voice > 0.0 {
            phase += plan.f0_at(ms) / RATE as f32;
            if phase >= 1.0 {
                phase -= phase.floor();
            }
            source = match effort {
                Effort::Normal => pulse(phase, OPEN_QUOTIENT),
                Effort::Breathy => {
                    pulse(phase, OPEN_QUOTIENT_BREATHY) * 0.75 + noise.next_f32() * 0.3
                }
                Effort::Whispered => noise.next_f32() * 0.9,
            } * voice;
        }

        r[0].set(f1, 70.0);
        r[1].set(f2, 105.0);
        r[2].set(f3, 160.0);
        let mut voiced = source;
        for res in &mut r {
            voiced = res.run(voiced);
        }

        if nasal > 0.0 {
            nasal_res.set(NASAL.0, NASAL.1);
            let nas = nasal_res.run(source);
            voiced = voiced.mul_add(1.0 - 0.65 * nasal, nas * 1.9 * nasal);
        }

        let frication = if fric_hz > 0.0 && fric_amp > 0.0 {
            fric_res.set(fric_hz, fric_hz.mul_add(0.25, 100.0));
            fric_res.run(noise.next_f32()) * fric_amp * FRICATION_GAIN
        } else {
            0.0
        };

        out.push((voiced + frication) * GAIN);
    }
    out
}

/// The Rosenberg-style glottal pulse: a sine bump over the open part of each
/// cycle, silence while the folds are closed.
fn pulse(phase: f32, open: f32) -> f32 {
    if phase < open {
        (std::f32::consts::PI * phase / open).sin() * 1.7
    } else {
        0.0
    }
}

/// The index of the span `ms` falls in. The tail after the last span stays on
/// that span, holding its target.
fn span_at(spans: &[Span], ms: f32) -> usize {
    match spans.binary_search_by(|s| s.start.partial_cmp(&ms).unwrap()) {
        Ok(i) => i,
        Err(0) => 0,
        Err(i) => (i - 1).min(spans.len() - 1),
    }
}

/// A frame parameter at `ms`: gliding in from the previous span's target over
/// the transition window, then held.
fn track(spans: &[Span], i: usize, ms: f32, pick: impl Fn(&crate::phoneme::Frame) -> f32) -> f32 {
    let span = &spans[i];
    let target = pick(&span.frame);
    let window = (span.end - span.start).min(TRANSITION_MS);
    let into = ms - span.start;
    if window <= 0.0 || into >= window {
        return target;
    }
    let prev = if i == 0 {
        target
    } else {
        pick(&spans[i - 1].frame)
    };
    prev + (target - prev) * (into / window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;
    use crate::params::{Effort, Quality};
    use crate::prosody::Plan;
    use crate::utterance::{Terminal, build};

    fn samples(text: &str, effort: Effort) -> Vec<f32> {
        let params = crate::params::Params {
            effort,
            ..Default::default()
        };
        let phones = Language::English.transcribe(text);
        let utterance = build(&phones, params.speed);
        let nuclei: Vec<(f32, f32)> = utterance
            .spans
            .iter()
            .filter(|s| s.nucleus)
            .map(|s| (s.start, s.end))
            .collect();
        let plan = Plan::new(
            params.pitch,
            Quality::Natural,
            &nuclei,
            utterance.total_ms,
            Terminal::None,
        );
        synthesize(&utterance, &plan, effort)
    }

    #[test]
    fn renders_sound_and_changes_with_effort() {
        let normal = samples("hello there", Effort::Normal);
        assert!(!normal.is_empty());
        let peak = normal.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "expected audible samples, peak {peak}");

        let whisper = samples("hello there", Effort::Whispered);
        assert_ne!(normal, whisper);
        let breathy = samples("hello there", Effort::Breathy);
        assert_ne!(normal, breathy);
    }
}
