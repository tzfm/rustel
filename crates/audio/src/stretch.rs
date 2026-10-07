//! `stretch` - a phase vocoder that shifts pitch without changing duration.
//!
//! Classic overlap-add framing: a 2048-sample analysis block advanced 128 at
//! a time, a Hann window applied on the way in and again on the way out,
//! spectral peaks found and translated by the pitch factor while their
//! surrounding bins ride along, and the overlapping results summed and
//! divided by the overlap count.
//!
//! The window carries a deliberate 1.62 amplitude factor, applied twice -
//! that scaling is part of the effect's sound. Do not normalize it away.

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// Analysis window and FFT length.
const BLOCK: usize = 2048;

/// Hop size - one 128-frame render quantum, the only hop this framing
/// supports.
const HOP: usize = 128;

/// `BLOCK / HOP`: how many analysis windows cover any one output sample, and
/// therefore what the summed result is divided by.
const OVERLAPS: usize = BLOCK / HOP;

/// Frames by which this vocoder's output trails that of one run a render
/// quantum at a time. That one analyses a window when the quantum completing
/// it arrives and emits the window's first frame at once, `BLOCK - HOP`
/// frames after that frame came in; this one takes a frame at a time and
/// emits it once the window's last frame is in, `BLOCK - 1` frames after.
pub const QUANTUM_LAG_FRAMES: u32 = HOP as u32 - 1;

/// One channel of phase-vocoded audio.
///
/// Clone gives a FRESH vocoder rather than a copy of one mid-flight: the FFT
/// plans are shared, and the buffers start empty. A voice is cloned before it
/// sounds, so there is no state worth carrying, and copying a half-filled
/// analysis window into a second voice would leak one into the other.
pub struct Stretch {
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// The last BLOCK samples of input, oldest first.
    input: Vec<f32>,
    /// Output waiting to be emitted, summed across overlapping windows.
    output: Vec<f32>,
    /// Samples taken in since the last analysis.
    filled: usize,
    spectrum: Vec<Complex32>,
    shifted: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Working space rustfft needs for an in-place transform.
    ///
    /// Without it, `Fft::process` allocates this buffer on every call: once
    /// per hop, per voice, inside the audio callback.
    fft_scratch: Vec<Complex32>,
    magnitudes: Vec<f32>,
    peaks: Vec<usize>,
    /// Advances one hop per analysis; the phase correction is relative to it,
    /// so a shifted partial stays coherent across windows.
    time_cursor: f32,
}

impl Clone for Stretch {
    fn clone(&self) -> Self {
        Self {
            forward: Arc::clone(&self.forward),
            inverse: Arc::clone(&self.inverse),
            fft_scratch: vec![Complex32::default(); self.fft_scratch.len()],
            window: self.window.clone(),
            input: vec![0.0; BLOCK],
            output: vec![0.0; BLOCK],
            filled: 0,
            spectrum: vec![Complex32::default(); BLOCK],
            shifted: vec![Complex32::default(); BLOCK],
            scratch: vec![Complex32::default(); BLOCK],
            magnitudes: vec![0.0; BLOCK / 2 + 1],
            peaks: Vec::with_capacity(BLOCK / 2 + 1),
            time_cursor: 0.0,
        }
    }
}

impl std::fmt::Debug for Stretch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stretch")
            .field("time_cursor", &self.time_cursor)
            .field("peaks", &self.peaks.len())
            .finish_non_exhaustive()
    }
}

impl Stretch {
    pub fn new(planner: &mut FftPlanner<f32>) -> Self {
        let forward = planner.plan_fft_forward(BLOCK);
        let inverse = planner.plan_fft_inverse(BLOCK);
        // Ask the plans how much working space they need rather than guessing
        // BLOCK: a radix that needs more would allocate the difference on
        // every transform, which is the fault this buffer exists to remove.
        let scratch_len = forward
            .get_inplace_scratch_len()
            .max(inverse.get_inplace_scratch_len());
        Self {
            fft_scratch: vec![Complex32::default(); scratch_len],
            forward,
            inverse,
            window: (0..BLOCK)
                .map(|i| {
                    let phase = std::f32::consts::TAU * i as f32 / BLOCK as f32;
                    0.5 * (1.0 - phase.cos())
                })
                .collect(),
            input: vec![0.0; BLOCK],
            output: vec![0.0; BLOCK],
            filled: 0,
            spectrum: vec![Complex32::default(); BLOCK],
            shifted: vec![Complex32::default(); BLOCK],
            scratch: vec![Complex32::default(); BLOCK],
            magnitudes: vec![0.0; BLOCK / 2 + 1],
            peaks: Vec::with_capacity(BLOCK / 2 + 1),
            time_cursor: 0.0,
        }
    }

    /// The buffers this vocoder owns, by capacity. The FFT plans are shared
    /// by the whole pool and not counted here.
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        (self.window.capacity()
            + self.input.capacity()
            + self.output.capacity()
            + self.magnitudes.capacity())
            * size_of::<f32>()
            + (self.spectrum.capacity()
                + self.shifted.capacity()
                + self.scratch.capacity()
                + self.fft_scratch.capacity())
                * size_of::<Complex32>()
            + self.peaks.capacity() * size_of::<usize>()
    }

    /// Clear analysis history so a pooled vocoder can be leased to another
    /// voice. FFT plans and the Hann window stay put; only the running state
    /// is wiped. Called when a voice retires, never while it is sounding.
    pub fn reset(&mut self) {
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.filled = 0;
        self.time_cursor = 0.0;
        self.peaks.clear();
    }

    /// Take one sample and return one. The transform runs once per hop, so
    /// most calls only move a sample through the buffers.
    pub fn process(&mut self, sample: f32, pitch_factor: f32) -> f32 {
        self.input.copy_within(1.., 0);
        self.input[BLOCK - 1] = sample;

        // Analyse and THEN emit: the block going out must already carry the
        // window just transformed. Emitting first would put the output a
        // whole hop late.
        self.filled += 1;
        if self.filled >= HOP {
            self.filled = 0;
            self.analyse(pitch_factor);
        }

        let out = self.output[0];
        self.output.copy_within(1.., 0);
        self.output[BLOCK - 1] = 0.0;
        out
    }

    fn analyse(&mut self, pitch_factor: f32) {
        // Negative factors are quartered before the +1, so `stretch(-1)` is a
        // slow downward shift rather than an inversion.
        let factor = if pitch_factor < 0.0 {
            pitch_factor * 0.25
        } else {
            pitch_factor
        };
        let factor = (factor + 1.0).max(0.0);

        for (bin, (sample, window)) in self.input.iter().zip(&self.window).enumerate() {
            self.scratch[bin] = Complex32::new(sample * window * 1.62, 0.0);
        }
        self.spectrum.copy_from_slice(&self.scratch);
        self.forward
            .process_with_scratch(&mut self.spectrum, &mut self.fft_scratch);

        for (magnitude, bin) in self.magnitudes.iter_mut().zip(&self.spectrum) {
            // Peak finding only compares magnitudes, so the square root is
            // wasted work.
            *magnitude = bin.re * bin.re + bin.im * bin.im;
        }
        self.find_peaks();
        self.shift_peaks(factor);

        // A real signal's spectrum is conjugate-symmetric; the shifted half
        // has to be mirrored before the inverse transform.
        for bin in 1..BLOCK / 2 {
            self.shifted[BLOCK - bin] = self.shifted[bin].conj();
        }
        self.inverse
            .process_with_scratch(&mut self.shifted, &mut self.fft_scratch);

        let scale = 1.0 / BLOCK as f32;
        for (i, (out, window)) in self.output.iter_mut().zip(&self.window).enumerate() {
            let _ = i;
            *out += self.shifted[i].re * scale * window * 1.62 / OVERLAPS as f32;
        }
        self.time_cursor += HOP as f32;
    }

    /// A bin is a peak when it stands above its two neighbours on each side.
    /// Stepping two past a hit keeps the pair around a peak from both counting.
    fn find_peaks(&mut self) {
        self.peaks.clear();
        let mut i = 2;
        let end = self.magnitudes.len() - 2;
        while i < end {
            let magnitude = self.magnitudes[i];
            if self.magnitudes[i - 1] >= magnitude || self.magnitudes[i - 2] >= magnitude {
                i += 1;
                continue;
            }
            if self.magnitudes[i + 1] >= magnitude || self.magnitudes[i + 2] >= magnitude {
                i += 1;
                continue;
            }
            self.peaks.push(i);
            i += 2;
        }
    }

    /// Move each peak to `peak * factor`, carrying the bins around it - its
    /// region of influence, bounded halfway to each neighbouring peak - and
    /// rotating them by the phase the move implies at the current time.
    fn shift_peaks(&mut self, factor: f32) {
        self.shifted.fill(Complex32::default());
        let half = self.magnitudes.len();
        for index in 0..self.peaks.len() {
            let peak = self.peaks[index];
            let shifted_peak = (peak as f32 * factor + 0.5).floor() as usize;
            if shifted_peak > half {
                break;
            }
            let start = if index > 0 {
                let previous = self.peaks[index - 1];
                peak - (((peak - previous) as f32 / 2.0) + 0.5).floor() as usize
            } else {
                0
            };
            // `floor(x + 1)`, not `ceil(x)`: at an even gap the bin halfway
            // rides with both neighbouring peaks.
            let end = if index + 1 < self.peaks.len() {
                let next = self.peaks[index + 1];
                peak + (((next - peak) as f32 / 2.0) + 1.0).floor() as usize
            } else {
                BLOCK
            };

            let omega_delta =
                std::f32::consts::TAU / BLOCK as f32 * (shifted_peak as f32 - peak as f32);
            let rotation = Complex32::new(
                (omega_delta * self.time_cursor).cos(),
                (omega_delta * self.time_cursor).sin(),
            );

            for offset in (start as isize - peak as isize)..(end as isize - peak as isize) {
                let bin = peak as isize + offset;
                let bin_shifted = shifted_peak as isize + offset;
                if bin < 0 || bin_shifted < 0 {
                    continue;
                }
                let (bin, bin_shifted) = (bin as usize, bin_shifted as usize);
                if bin_shifted >= half || bin >= self.spectrum.len() {
                    break;
                }
                self.shifted[bin_shifted] += self.spectrum[bin] * rotation;
            }
        }
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;

    /// Each peak carries the bins up to halfway to its neighbours, rounding the
    /// far bound as `floor(x + 1)`: at an even gap the bin halfway between two
    /// peaks rides with both.
    #[test]
    fn the_bin_halfway_between_two_peaks_rides_with_both() {
        let mut stretch = Stretch::new(&mut FftPlanner::new());
        for (bin, value) in stretch.spectrum.iter_mut().enumerate() {
            *value = Complex32::new(bin as f32 + 1.0, 0.0);
        }
        stretch.peaks.extend([100, 104]);
        // A factor of one moves no peak, and at time zero turns no phase.
        stretch.shift_peaks(1.0);
        assert_eq!(stretch.shifted[101], stretch.spectrum[101]);
        assert_eq!(stretch.shifted[102], stretch.spectrum[102] * 2.0);
        assert_eq!(stretch.shifted[103], stretch.spectrum[103]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A factor of 0 is the identity shift, so the vocoder must return what it
    /// was given - delayed by its own window, and at its own level.
    #[test]
    fn an_unshifted_tone_survives_the_round_trip() {
        let mut planner = FftPlanner::new();
        let mut stretch = Stretch::new(&mut planner);
        let tone: Vec<f32> = (0..BLOCK * 4)
            .map(|n| (std::f32::consts::TAU * 440.0 * n as f32 / 48_000.0).sin())
            .collect();
        let out: Vec<f32> = tone.iter().map(|s| stretch.process(*s, 0.0)).collect();

        // Past the priming window the output is periodic and non-trivial.
        let tail = &out[BLOCK * 2..];
        let peak = tail.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak > 0.1, "vocoder produced near-silence: peak {peak}");
        let crossings = tail
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let expected = 2.0 * 440.0 * tail.len() as f32 / 48_000.0;
        assert!(
            (crossings as f32 - expected).abs() < expected * 0.2,
            "pitch moved without being asked: {crossings} crossings, expected ~{expected:.0}"
        );
    }

    /// Doubling the factor should raise the pitch, not merely change the level.
    #[test]
    fn a_positive_factor_raises_the_pitch() {
        let mut planner = FftPlanner::new();
        let mut count = |factor: f32| {
            let mut stretch = Stretch::new(&mut planner);
            let out: Vec<f32> = (0..BLOCK * 4)
                .map(|n| {
                    let s = (std::f32::consts::TAU * 220.0 * n as f32 / 48_000.0).sin();
                    stretch.process(s, factor)
                })
                .collect();
            let tail = &out[BLOCK * 2..];
            tail.windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count()
        };
        let plain = count(0.0);
        let raised = count(1.0);
        assert!(
            raised > plain + plain / 4,
            "a factor of 1 should roughly double the pitch: {plain} -> {raised}"
        );
    }
}

#[cfg(test)]
mod pitch_property {
    use super::*;

    /// The dominant frequency of a rendered tone, by finding the largest DFT
    /// bin over the band a shifted 440 Hz partial can land in. A plain
    /// Goertzel-free scan is enough: one strong partial, no need for an FFT.
    fn dominant_hz(signal: &[f32], sample_rate: f32) -> f32 {
        let n = signal.len();
        let mut best_power = 0.0f32;
        let mut best_hz = 0.0f32;
        let mut hz = 100.0;
        while hz <= 3000.0 {
            let w = std::f32::consts::TAU * hz / sample_rate;
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, &s) in signal.iter().enumerate() {
                re += s * (w * i as f32).cos();
                im += s * (w * i as f32).sin();
            }
            let power = (re * re + im * im) / (n * n) as f32;
            if power > best_power {
                best_power = power;
                best_hz = hz;
            }
            hz += 1.0;
        }
        best_hz
    }

    /// `stretch` shifts pitch by `factor + 1` and keeps the duration. The
    /// vocoder moves spectral peaks by whole 23.4 Hz bins, so a fractional
    /// factor can be half a bin off: about 30 cents near 660 Hz.
    #[test]
    fn a_tone_comes_out_shifted_by_the_factor() {
        let sample_rate = 48_000.0;
        let input_hz = 440.0;
        for (factor, expected_hz, cents_tol) in
            [(0.0, 440.0, 20.0), (1.0, 880.0, 20.0), (0.5, 660.0, 40.0)]
        {
            let mut planner = FftPlanner::new();
            let mut stretch = Stretch::new(&mut planner);
            let frames = BLOCK * 8;
            let tone: Vec<f32> = (0..frames)
                .map(|n| (std::f32::consts::TAU * input_hz * n as f32 / sample_rate).sin())
                .collect();
            let out: Vec<f32> = tone.iter().map(|s| stretch.process(*s, factor)).collect();

            // Same number of samples out as in: duration is preserved.
            assert_eq!(out.len(), tone.len());

            // Measure past the priming window, where the output is steady.
            let tail = &out[BLOCK * 3..];
            let peak = tail.iter().fold(0.0f32, |a, s| a.max(s.abs()));
            assert!(peak > 0.05, "factor {factor}: near-silent, peak {peak}");

            let measured = dominant_hz(tail, sample_rate);
            let cents = 1200.0 * (measured / expected_hz).log2();
            assert!(
                cents.abs() < cents_tol,
                "factor {factor}: expected {expected_hz} Hz, got {measured} Hz ({cents:+.1} cents)"
            );
        }
    }

    /// A negative factor shifts DOWN. `stretch(-1)` quarters the factor before
    /// the +1, so the ratio is 0.75: a 440 Hz tone comes out near 330 Hz. This
    /// pins the direction and the negative-factor scaling together.
    #[test]
    fn a_negative_factor_shifts_the_tone_down() {
        let sample_rate = 48_000.0;
        let mut planner = FftPlanner::new();
        let mut stretch = Stretch::new(&mut planner);
        let tone: Vec<f32> = (0..BLOCK * 8)
            .map(|n| (std::f32::consts::TAU * 440.0 * n as f32 / sample_rate).sin())
            .collect();
        let out: Vec<f32> = tone.iter().map(|s| stretch.process(*s, -1.0)).collect();
        let measured = dominant_hz(&out[BLOCK * 3..], sample_rate);
        let cents = 1200.0 * (measured / 330.0).log2();
        assert!(
            cents.abs() < 40.0,
            "stretch(-1): expected ~330 Hz, got {measured} Hz ({cents:+.1} cents)"
        );
    }
}
