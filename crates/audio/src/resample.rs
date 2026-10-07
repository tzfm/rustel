//! Windowed-sinc sample-rate conversion for decoded PCM.
//!
//! [`crate::DecodedSample::resampled_to`] converts samples to the render rate
//! before playback. This separates sample-rate conversion from the linear
//! interpolation used to change playback pitch, preserving high frequencies
//! while suppressing resampling images.
//!
//! The kernel follows Chromium's sample-rate converter: 32 taps, 32
//! sub-sample phases, Blackman-windowed sinc with the cutoff at 0.9 of the
//! lower Nyquist frequency, and a linear blend between the two phases
//! bracketing each output position.

use crate::sample::{format_sample_bytes, sample_pcm_ceiling};

/// Taps per phase.
const KERNEL_SIZE: usize = 32;
/// Sub-sample phases. A phase for offset 1.0 is stored as well, so the blend
/// between phase `i` and `i + 1` never needs a bounds check.
const KERNEL_PHASES: usize = 32;

/// Half the kernel, i.e. how far back of an output position the taps reach.
const KERNEL_HALF: usize = KERNEL_SIZE / 2;

/// One windowed-sinc kernel per sub-sample phase, laid out phase-major.
struct Kernels {
    taps: Vec<f32>,
}

impl Kernels {
    fn new(scale_factor: f64) -> Self {
        // Blackman: a0 - a1·cos(2πx) + a2·cos(4πx) with alpha 0.16.
        const ALPHA: f64 = 0.16;
        let (a0, a1, a2) = (0.5 * (1.0 - ALPHA), 0.5, 0.5 * ALPHA);
        // The normalized cutoff. Downsampling moves it to the DESTINATION
        // Nyquist; the 0.9 keeps the window's transition band below it
        // instead of letting the skirt alias back.
        let mut cutoff = if scale_factor > 1.0 {
            1.0 / scale_factor
        } else {
            1.0
        };
        cutoff *= 0.9;

        let mut taps = vec![0.0f32; KERNEL_SIZE * (KERNEL_PHASES + 1)];
        for phase in 0..=KERNEL_PHASES {
            let offset = phase as f64 / KERNEL_PHASES as f64;
            for tap in 0..KERNEL_SIZE {
                let x = tap as f64 - KERNEL_HALF as f64 - offset;
                let s = cutoff * std::f64::consts::PI * x;
                let sinc = if s == 0.0 { 1.0 } else { s.sin() / s } * cutoff;
                // The window follows the sinc's own offset, so both stay
                // centred on the same fractional position.
                let w = (tap as f64 - offset) / KERNEL_SIZE as f64;
                let window = a0 - a1 * (std::f64::consts::TAU * w).cos()
                    + a2 * (2.0 * std::f64::consts::TAU * w).cos();
                taps[phase * KERNEL_SIZE + tap] = (sinc * window) as f32;
            }
        }
        Self { taps }
    }

    #[inline]
    fn phase(&self, index: usize) -> &[f32] {
        &self.taps[index * KERNEL_SIZE..index * KERNEL_SIZE + KERNEL_SIZE]
    }
}

/// Output frame count, truncating any fractional final frame.
pub fn converted_frames(frames: usize, from_rate: u32, to_rate: u32) -> usize {
    if from_rate == to_rate {
        return frames;
    }
    let ratio = f64::from(from_rate) / f64::from(to_rate);
    (frames as f64 / ratio) as usize
}

/// Convert interleaved `channels`-channel PCM from `from_rate` to `to_rate`.
///
/// Equal rates copy. Channels convert independently, as separate mono buses.
///
/// A file may declare any rate its header fits, and the conversion scales the
/// body by the ratio of the two: kilobytes at 1 Hz are gigabytes at 48 kHz. A
/// failed Rust allocation is an abort no caller can catch, so a conversion
/// whose output passes the sample ceiling is refused rather than attempted.
pub fn interleaved(
    pcm: &[f32],
    channels: u16,
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<f32>, String> {
    let channels = usize::from(channels.max(1));
    let frames = pcm.len() / channels;
    if from_rate == to_rate || frames == 0 {
        return Ok(pcm.to_vec());
    }
    let ratio = f64::from(from_rate) / f64::from(to_rate);
    let out_frames = (frames as f64 / ratio) as usize;
    if out_frames == 0 {
        return Ok(Vec::new());
    }
    // Saturating, not wrapping: a product that wrapped could come back under
    // the ceiling and ask for the impossible anyway, while a saturated one is
    // usize::MAX, past every ceiling the policy allows.
    let out_samples = out_frames.saturating_mul(channels);
    let out_bytes = out_samples.saturating_mul(size_of::<f32>());
    let ceiling = sample_pcm_ceiling();
    if out_bytes > ceiling {
        return Err(format!(
            "converting {from_rate}Hz to {to_rate}Hz would need {}, past the {} one sound can hold",
            format_sample_bytes(out_bytes),
            format_sample_bytes(ceiling),
        ));
    }
    let kernels = Kernels::new(ratio);
    let mut out = vec![0.0f32; out_samples];
    // The taps reach KERNEL_HALF frames either side of an output position, and
    // the reference reads zeros past both ends of the source, so the channel is
    // copied into a padded scratch buffer rather than bounds-checked per tap.
    let mut scratch = vec![0.0f32; frames + KERNEL_SIZE * 2];
    for channel in 0..channels {
        scratch.fill(0.0);
        for frame in 0..frames {
            scratch[KERNEL_HALF + frame] = pcm[frame * channels + channel];
        }
        for (index, slot) in out.iter_mut().skip(channel).step_by(channels).enumerate() {
            let position = index as f64 * ratio;
            let frame = position as usize;
            let fraction = (position - frame as f64) * KERNEL_PHASES as f64;
            let phase = fraction as usize;
            let blend = (fraction - phase as f64) as f32;
            let window = &scratch[frame..frame + KERNEL_SIZE];
            let (lower, upper) = (kernels.phase(phase), kernels.phase(phase + 1));
            // Accumulated in f32, like the reference convolution.
            let mut low = 0.0f32;
            let mut high = 0.0f32;
            for tap in 0..KERNEL_SIZE {
                low += window[tap] * lower[tap];
                high += window[tap] * upper[tap];
            }
            *slot = (1.0 - blend) * low + blend * high;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine well inside both passbands must survive with its amplitude and
    /// phase intact - the kernel is a lowpass, and 1 kHz is nowhere near it.
    #[test]
    fn a_passband_sine_keeps_its_amplitude() {
        let frames = 4096;
        let source: Vec<f32> = (0..frames)
            .map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / 44_100.0).sin() as f32)
            .collect();
        let out = interleaved(&source, 1, 44_100, 48_000).expect("ordinary conversion");
        assert_eq!(out.len(), converted_frames(frames, 44_100, 48_000));
        // Skip the kernel's lead-in and tail, where the zero padding shows.
        for (i, value) in out.iter().enumerate().take(out.len() - 64).skip(64) {
            let want = (std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0).sin() as f32;
            assert!(
                (value - want).abs() < 2e-3,
                "frame {i}: {value} vs {want} (sine must pass unchanged)"
            );
        }
    }

    /// Amplitude of a steady tone, from its energy rather than its peak: at
    /// 12 kHz a period is four samples wide and no sample need land on the
    /// crest, so a peak reading understates a filter that is in fact flat.
    fn amplitude(x: &[f32]) -> f32 {
        let body = &x[128..x.len() - 128];
        let mean_square: f64 = body
            .iter()
            .map(|v| f64::from(*v) * f64::from(*v))
            .sum::<f64>()
            / body.len() as f64;
        (mean_square.sqrt() * std::f64::consts::SQRT_2) as f32
    }

    fn tone(hz: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    /// What playback used to do, kept as the control the tests below measure
    /// against: one linear interpolation covering the rate change.
    fn linear(source: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
        let ratio = f64::from(from_rate) / f64::from(to_rate);
        (0..(source.len() as f64 / ratio) as usize)
            .map(|frame| {
                let position = frame as f64 * ratio;
                let index = position as usize;
                let t = (position - index as f64) as f32;
                let a = source.get(index).copied().unwrap_or(0.0);
                let b = source.get(index + 1).copied().unwrap_or(a);
                a + (b - a) * t
            })
            .collect()
    }

    /// The band a set actually lives in comes through flat, which linear
    /// interpolation does not manage: at 12 kHz it is already 0.6 dB down, and
    /// that shortfall grows through the range every hi-hat occupies.
    #[test]
    fn the_passband_comes_through_flat() {
        for hz in [1000.0, 6000.0, 12000.0] {
            let source = tone(hz, 44_100, 8192);
            let got =
                amplitude(&interleaved(&source, 1, 44_100, 48_000).expect("ordinary conversion"));
            assert!(
                (got - 1.0).abs() < 0.01,
                "{hz} Hz came through at {got}, not flat"
            );
        }
        let dull = amplitude(&linear(&tone(12_000.0, 44_100, 8192), 44_100, 48_000));
        assert!(
            dull < 0.94,
            "the control no longer separates the two filters"
        );
    }

    /// Downsampling rejects what the destination cannot carry. Left in, 23 kHz
    /// folds back to 21.1 kHz as an audible whistle, which is exactly what one
    /// linear interpolation does with it.
    #[test]
    fn content_past_the_destination_nyquist_is_rejected() {
        let source = tone(23_000.0, 48_000, 8192);
        let got = amplitude(&interleaved(&source, 1, 48_000, 44_100).expect("ordinary conversion"));
        assert!(got < 0.05, "23 kHz survived downsampling at {got}");
        // Half amplitude left where nothing should be: loud enough to hear as a
        // whistle once it folds back.
        let aliased = amplitude(&linear(&source, 48_000, 44_100));
        assert!(
            aliased > 0.4,
            "the control no longer separates the two filters"
        );
    }

    #[test]
    fn equal_rates_copy_and_channels_stay_interleaved() {
        let pcm = vec![0.25, -0.5, 0.75, -1.0];
        assert_eq!(
            interleaved(&pcm, 2, 48_000, 48_000).expect("equal rates copy"),
            pcm
        );
        // A constant channel stays constant across the conversion; an
        // interleaving mistake would mix the two levels together.
        let stereo: Vec<f32> = (0..2048).flat_map(|_| [1.0f32, -1.0f32]).collect();
        let out = interleaved(&stereo, 2, 44_100, 48_000).expect("ordinary conversion");
        for frame in out
            .as_chunks::<2>()
            .0
            .iter()
            .skip(64)
            .take(out.len() / 2 - 128)
        {
            assert!((frame[0] - 1.0).abs() < 1e-3, "left drifted: {}", frame[0]);
            assert!((frame[1] + 1.0).abs() < 1e-3, "right drifted: {}", frame[1]);
        }
    }

    /// The allocation guard itself: converting away from an absurd declared rate
    /// must return an error, not request memory that cannot succeed - a failed
    /// Rust allocation is an abort, which no caller's `catch_unwind` can hold.
    #[test]
    fn a_conversion_past_the_ceiling_is_refused_before_allocating() {
        let source = vec![0.0f32; 4_096];
        // 4_096 frames at 1 Hz become 1.6 billion of them at 384 kHz - gigabytes
        // out of a kilobyte body, past the highest ceiling the policy allows.
        let error = interleaved(&source, 1, 1, 384_000).unwrap_err();
        assert!(error.contains("one sound can hold"), "{error}");
        // The same source converts as usual at ordinary rates.
        let out = interleaved(&source, 1, 44_100, 48_000).expect("ordinary conversion");
        assert_eq!(out.len(), converted_frames(4_096, 44_100, 48_000));
    }
}
