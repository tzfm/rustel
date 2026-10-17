//! The voice source: the buzz of the vocal folds, before the vocal tract
//! shapes it.
//!
//! Each of the eight glottal sources is a measured spectrum. `glottis.bin`
//! holds the harmonics of every source at 13 anchor pitches, a third of an
//! octave apart from 50 to 800 Hz, up to 5.4 kHz: a level byte and a phase
//! byte per harmonic. The spectrum changes with the pitch, so the generator
//! blends the two anchors around the current pitch, then adds up the
//! harmonics. The source has no energy past the anchors' top, so it does not
//! alias.
//!
//! The phase is counted from the glottal closure. The oscillator starts half
//! a period before the first closure.

use std::f32::consts::TAU;

const DATA: &[u8] = include_bytes!("glottis.bin");
pub(super) const ANCHORS: [f32; 13] = [
    50.0, 63.0, 79.4, 100.0, 126.0, 158.7, 200.0, 252.0, 317.5, 400.0, 504.0, 635.0, 800.0,
];
const TOP_HZ: f32 = 5400.0;
/// The harmonic amplitudes are recomputed when the pitch moves this much.
const RETUNE: f32 = 0.002;

fn harmonics_at(anchor: usize) -> usize {
    (TOP_HZ / ANCHORS[anchor]).floor() as usize
}

/// The byte offset of one source's anchor in [`DATA`].
fn offset(kind: usize, anchor: usize) -> usize {
    let per_kind: usize = (0..ANCHORS.len()).map(harmonics_at).sum();
    let before: usize = (0..anchor).map(harmonics_at).sum();
    2 * (kind * per_kind + before)
}

/// The level in dB and the phase in radians of harmonic `k` (from 1) of
/// one anchor.
fn stored(kind: usize, anchor: usize, k: usize) -> (f32, f32) {
    let at = offset(kind, anchor) + 2 * (k - 1);
    (
        DATA[at] as f32 / 2.0 - 70.0,
        DATA[at + 1] as f32 / 256.0 * TAU,
    )
}

/// Level and phase part way from `a` to `b`: the level in dB, the phase
/// the short way round.
fn blend(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    let turn = (b.1 - a.1 + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI;
    (a.0 + (b.0 - a.0) * t, a.1 + turn * t)
}

/// One anchor's spectrum at frequency `hz`, between its harmonics: level
/// in dB and phase.
fn anchor_spectrum(kind: usize, anchor: usize, hz: f32) -> (f32, f32) {
    let count = harmonics_at(anchor);
    let x = (hz / ANCHORS[anchor]).clamp(1.0, count as f32);
    let k = (x.floor() as usize).clamp(1, count.max(2) - 1);
    blend(
        stored(kind, anchor, k),
        stored(kind, anchor, (k + 1).min(count)),
        x - k as f32,
    )
}

/// A periodic glottal source.
pub(super) struct Glottis {
    kind: usize,
    /// Phase in turns from the last closure.
    phase: f64,
    tuned: f32,
    /// Per harmonic: in-phase and quadrature amplitude.
    amps: Vec<(f32, f32)>,
}

impl Glottis {
    /// Glottal source 1 to 8, as the voice record numbers them.
    pub(super) fn new(kind: u8) -> Self {
        Self {
            kind: usize::from(kind.clamp(1, 8)) - 1,
            phase: 0.5,
            tuned: 0.0,
            amps: Vec::new(),
        }
    }

    fn retune(&mut self, f0: f32) {
        self.tuned = f0;
        let f = f0.clamp(ANCHORS[0], ANCHORS[ANCHORS.len() - 1]);
        let upper = ANCHORS.partition_point(|&a| a <= f).min(ANCHORS.len() - 1);
        let lower = upper.saturating_sub(1);
        let w = if upper == lower {
            0.0
        } else {
            (f / ANCHORS[lower]).ln() / (ANCHORS[upper] / ANCHORS[lower]).ln()
        };
        let count = ((TOP_HZ.min(crate::RATE as f32 / 2.0 - 50.0)) / f0).floor() as usize;
        self.amps.clear();
        for k in 1..=count {
            let hz = k as f32 * f0;
            let (db, phase) = blend(
                anchor_spectrum(self.kind, lower, hz),
                anchor_spectrum(self.kind, upper, hz),
                w,
            );
            let amp = 10f32.powf(db / 20.0);
            self.amps.push((amp * phase.cos(), amp * phase.sin()));
        }
    }

    /// Move one sample on without sounding.
    pub(super) fn advance(&mut self, f0: f32) {
        self.phase = (self.phase + f64::from(f0.max(0.0)) / f64::from(crate::RATE)).fract();
    }

    /// The next sample at fundamental `f0` Hz, in output LSB units.
    pub(super) fn next(&mut self, f0: f32) -> f32 {
        if (f0 - self.tuned).abs() > self.tuned * RETUNE {
            self.retune(f0);
        }
        self.advance(f0);
        let theta = (self.phase * std::f64::consts::TAU) as f32;
        // Harmonics by rotation: one complex multiply each.
        let (s1, c1) = theta.sin_cos();
        let (mut c, mut s) = (c1, s1);
        let mut sum = 0.0;
        for &(re, im) in &self.amps {
            sum += re * c - im * s;
            let next_c = c * c1 - s * s1;
            s = s * c1 + c * s1;
            c = next_c;
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_has_a_full_table() {
        assert_eq!(offset(8, 0), DATA.len());
    }
}
