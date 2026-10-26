//! Parameter frames to samples.
//!
//! ```text
//! glottis x AV ─> tilt ──┐
//!                        +──> F1 ─> F2 ─> F3 ─> F4 ─> F5 ──┐
//! noise x AH ────────────┘                                 +──> 8-bit out
//! noise x AF ──> frication resonators ─────────────────────┘
//! ```
//!
//! The resonators have unity gain at 0 Hz, the form the source tables were
//! measured through. The output is in the original engine's units: one unit
//! is one step of its 8-bit samples, and it saturates the same way.

use std::f32::consts::PI;

use super::glottis::Glottis;
use super::table::Hiss;
use crate::RATE;

/// Everything the synthesizer needs at one sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub f0: f32,
    /// Voicing, aspiration and frication amplitudes, linear.
    pub av: f32,
    pub ah: f32,
    pub af: f32,
    /// Cascade formants and bandwidths, Hz.
    pub f: [f32; 3],
    pub b: [f32; 3],
    /// The color of the frication noise.
    pub hiss: Hiss,
}

/// One two-pole resonator with unity gain at 0 Hz.
#[derive(Clone, Copy, Default)]
struct Resonator {
    a: f32,
    b: f32,
    c: f32,
    y1: f32,
    y2: f32,
    tuned: (f32, f32),
}

impl Resonator {
    fn tune(&mut self, hz: f32, bw: f32) {
        if self.tuned == (hz, bw) {
            return;
        }
        self.tuned = (hz, bw);
        let r = (-PI * bw / RATE as f32).exp();
        self.b = 2.0 * r * (2.0 * PI * hz / RATE as f32).cos();
        self.c = -r * r;
        self.a = 1.0 - self.b - self.c;
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Deterministic noise: the same text renders the same wave everywhere.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / 8_388_608.0 - 1.0
    }
}

/// The synthesizer of one utterance.
pub(super) struct Synth {
    glottis: Glottis,
    cascade: [Resonator; 3],
    upper: [Resonator; 2],
    hiss: [Resonator; 3],
    /// One noise for the voice's breath, one per hiss path and one for the
    /// plain hiss, so their powers add.
    noise: [Noise; 5],
    /// The last hiss sum, for its first difference.
    last_hiss: f32,
    /// The spectral tilt of the voice: a one-pole lowpass coefficient, and
    /// its state.
    tilt: f32,
    tilted: f32,
    /// The tract's factor on the hiss frequencies.
    hiss_scale: f32,
}

impl Synth {
    /// A synthesizer with glottal source `glottis`, the fixed upper pair of
    /// resonances (Hz, bandwidth), a voice tilt from the voice record and
    /// the tract's factor on the hiss frequencies.
    ///
    /// A tilt of 2 or more leaves the voice as it is. Each step below 2 adds
    /// 0.1 to the coefficient of a lowpass on the voice alone, so a tilt of
    /// -4 takes about 9 dB off at 3 kHz, as in the original engine.
    pub(super) fn new(glottis: u8, upper: [(f32, f32); 2], tilt: i8, hiss_scale: f32) -> Self {
        let mut up = [Resonator::default(); 2];
        for (r, (hz, bw)) in up.iter_mut().zip(upper) {
            r.tune(hz, bw);
        }
        Self {
            glottis: Glottis::new(glottis),
            cascade: [Resonator::default(); 3],
            upper: up,
            hiss: [Resonator::default(); 3],
            noise: [0x5eed, 0x9e37, 0x7f4a, 0x2c1b, 0x6d2e].map(Noise),
            last_hiss: 0.0,
            tilt: (0.1 * f32::from(2 - i16::from(tilt).min(2))).min(0.9),
            tilted: 0.0,
            hiss_scale,
        }
    }

    /// The next output sample, in 8-bit steps, before rounding.
    pub(super) fn next(&mut self, fr: &Frame) -> f32 {
        let voice = if fr.av > 0.0 && fr.f0 > 0.0 {
            self.glottis.next(fr.f0) * fr.av
        } else {
            // The oscillator keeps running through silence.
            self.glottis.advance(fr.f0);
            0.0
        };
        self.tilted = (1.0 - self.tilt) * voice + self.tilt * self.tilted;
        let mut x = self.tilted + self.noise[0].next() * fr.ah;
        for (r, k) in self.cascade.iter_mut().zip(0..3) {
            r.tune(fr.f[k], fr.b[k]);
            x = r.run(x);
        }
        for r in &mut self.upper {
            x = r.run(x);
        }
        if fr.af > 0.0 {
            let mut sum = fr.hiss.flat * self.noise[4].next();
            for (k, &(hz, bw, level)) in fr.hiss.peaks.iter().enumerate() {
                if level > 0.0 {
                    // A scaled peak stays under the Nyquist frequency.
                    self.hiss[k].tune((hz * self.hiss_scale).min(0.45 * RATE as f32), bw);
                    sum += self.hiss[k].run(self.noise[k + 1].next()) * level;
                }
            }
            x += (sum - self.last_hiss) * fr.af;
            self.last_hiss = sum;
        } else {
            self.last_hiss = 0.0;
        }
        x
    }
}

/// Cut to the engine's 8-bit steps, as it does: down to the step below,
/// so a sound sits half a step under zero on average. Silence stays exactly
/// zero. Scaled to the -1 to 1 range.
pub(super) fn to_output(x: f32, silent: bool) -> f32 {
    if silent && x.abs() < 1.0 {
        return 0.0;
    }
    x.floor().clamp(-128.0, 127.0) / 128.0
}
