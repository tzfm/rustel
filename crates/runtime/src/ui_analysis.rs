//! Bounded audio reduction for editor visualizers.
//!
//! The device callback publishes mono samples into an allocation-free atomic
//! tap. This module runs later on the ordinary live producer thread: the FFT,
//! windowing and JSON-sized reduction can therefore allocate at
//! construction without weakening the real-time callback contract.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

pub const ANALYSIS_INPUT_SAMPLES: usize = rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES;
pub const UI_SCOPE_SAMPLES: usize = 512;
pub const UI_SPECTRUM_BINS: usize = 512;
/// Frames of left and right shipped with the master's analysis, for the
/// vectorscope.
pub const UI_SIDES_SAMPLES: usize = rustel_audio::LIVE_ANALYSIS_SIDES_SAMPLES;

#[derive(Clone, Debug, PartialEq)]
pub struct UiAudioAnalysisFrame {
    pub scope: [f32; UI_SCOPE_SAMPLES],
    /// Unsmoothed, linearly spaced magnitudes in dB. Renderer-specific
    /// logarithmic spacing, min/max dB, and temporal smoothing belong to each
    /// visual instance so two `.spectrum(...)` calls can differ.
    pub spectrum: [f32; UI_SPECTRUM_BINS],
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiAudioAnalysisSet {
    pub master: UiAudioAnalysisFrame,
    pub visuals: Vec<(u8, UiAudioAnalysisFrame)>,
    /// The newest frames of the mix as left and right, oldest first -
    /// [`UI_SIDES_SAMPLES`] of them, or none when the device has no
    /// stereo picture to give.
    pub sides: Vec<(f32, f32)>,
}

pub struct UiAudioAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    input: Vec<Complex32>,
    scratch: Vec<Complex32>,
    window: Vec<f32>,
    fft_scale: f32,
}

impl std::fmt::Debug for UiAudioAnalyzer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiAudioAnalyzer")
            .field("input_samples", &self.input.len())
            .field("spectrum_bins", &UI_SPECTRUM_BINS)
            .finish_non_exhaustive()
    }
}

impl Default for UiAudioAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl UiAudioAnalyzer {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(ANALYSIS_INPUT_SAMPLES);
        let scratch_len = fft.get_inplace_scratch_len();
        let denominator = (ANALYSIS_INPUT_SAMPLES - 1) as f32;
        let window = (0..ANALYSIS_INPUT_SAMPLES)
            .map(|index| {
                let phase = index as f32 / denominator;
                0.5 - 0.5 * (std::f32::consts::TAU * phase).cos()
            })
            .collect::<Vec<_>>();
        let fft_scale = 2.0 / window.iter().sum::<f32>();
        Self {
            fft,
            input: vec![Complex32::default(); ANALYSIS_INPUT_SAMPLES],
            scratch: vec![Complex32::default(); scratch_len],
            window,
            fft_scale,
        }
    }

    /// Reduce the newest post-mix mono window into two fixed-size UI views.
    ///
    /// `samples` may be shorter during device startup; missing history is
    /// silence. Extra history is ignored in favour of the newest window.
    pub fn analyze(&mut self, samples: &[f32]) -> UiAudioAnalysisFrame {
        let mut mono = [0.0f32; ANALYSIS_INPUT_SAMPLES];
        let copied = samples.len().min(ANALYSIS_INPUT_SAMPLES);
        mono[ANALYSIS_INPUT_SAMPLES - copied..]
            .copy_from_slice(&samples[samples.len().saturating_sub(copied)..]);

        let scope = scope_history(&mono);
        for ((slot, sample), window) in self.input.iter_mut().zip(mono).zip(&self.window) {
            *slot = Complex32::new(sample * *window, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.input, &mut self.scratch);

        let half = ANALYSIS_INPUT_SAMPLES / 2;
        let mut spectrum = [0.0; UI_SPECTRUM_BINS];
        for (display_bin, output) in spectrum.iter_mut().enumerate() {
            // Preserve linear frequency spacing in transport. A terminal can
            // then choose logarithmic or linear columns without double-warping
            // bins that were already reduced with a display policy.
            let low = (display_bin * half / UI_SPECTRUM_BINS).max(1);
            let high = (((display_bin + 1) * half / UI_SPECTRUM_BINS).max(low + 1)).min(half);
            let magnitude = self.input[low..high]
                .iter()
                .map(|value| value.norm() * self.fft_scale)
                .fold(0.0f32, f32::max);
            *output = (20.0 * magnitude.max(1e-6).log10()).clamp(-120.0, 12.0);
        }

        UiAudioAnalysisFrame { scope, spectrum }
    }
}

fn scope_history(samples: &[f32; ANALYSIS_INPUT_SAMPLES]) -> [f32; UI_SCOPE_SAMPLES] {
    // Uniform interpolated history keeps roughly 85 ms at 48 kHz, rather than
    // shipping only the final 2--3 ms. Trigger/alignment remains a per-scope
    // renderer choice. The endpoint-preserving mapping is deterministic.
    let mut scope = [0.0; UI_SCOPE_SAMPLES];
    for (index, output) in scope.iter_mut().enumerate() {
        let source =
            index as f32 * (ANALYSIS_INPUT_SAMPLES - 1) as f32 / (UI_SCOPE_SAMPLES - 1) as f32;
        let low = source.floor() as usize;
        let high = (low + 1).min(ANALYSIS_INPUT_SAMPLES - 1);
        let blend = source - low as f32;
        *output = samples[low] + (samples[high] - samples[low]) * blend;
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_finite_and_at_the_floor() {
        let frame = UiAudioAnalyzer::new().analyze(&[0.0; ANALYSIS_INPUT_SAMPLES]);
        assert!(frame.scope.iter().all(|sample| *sample == 0.0));
        assert!(frame.spectrum.iter().all(|bin| *bin == -120.0));
    }

    #[test]
    fn sine_wave_produces_a_stable_scope_and_spectral_peak() {
        let mut samples = [0.0; ANALYSIS_INPUT_SAMPLES];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = (std::f32::consts::TAU * 32.0 * index as f32 / ANALYSIS_INPUT_SAMPLES as f32)
                .sin()
                * 0.8;
        }
        let frame = UiAudioAnalyzer::new().analyze(&samples);
        assert!(frame.scope.iter().any(|sample| sample.abs() > 0.7));
        let peak = frame
            .spectrum
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.partial_cmp(right.1).unwrap())
            .expect("spectrum peak");
        assert!(peak.1 > &-6.0, "sine peak was too quiet: {peak:?}");
        assert!(
            (7..10).contains(&peak.0),
            "unexpected display bin: {peak:?}"
        );
    }

    #[test]
    fn newest_samples_win_when_history_is_oversized() {
        let mut history = vec![1.0; ANALYSIS_INPUT_SAMPLES * 2];
        history[ANALYSIS_INPUT_SAMPLES..].fill(0.0);
        let frame = UiAudioAnalyzer::new().analyze(&history);
        assert!(frame.scope.iter().all(|sample| *sample == 0.0));
    }
}
