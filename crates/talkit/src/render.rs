//! Text and settings to finished samples.
//!
//! This is the whole pipeline in one place: transcribe, place in time, plan
//! the pitch, synthesize, then normalize. Each stage lives in its own module.

use crate::params::Params;
use crate::prosody::Plan;
use crate::synth::synthesize;
use crate::utterance::build;

/// The level the loudest sample of an utterance is brought to.
const PEAK: f32 = 0.75;
/// Fade at each end, in ms, so playback starts and stops without a click.
const FADE_MS: f32 = 3.0;

/// Render `text` spoken with `params` as mono f32 samples at
/// [`crate::RATE`]. Text with no speech sounds in it renders nothing.
pub fn render(text: &str, params: &Params) -> Vec<f32> {
    let phones = params.language.transcribe(text);
    if !phones.iter().any(|p| {
        matches!(
            p,
            crate::lang::Phone::Sound(_) | crate::lang::Phone::Stressed(_)
        )
    }) {
        return Vec::new();
    }
    let utterance = build(&phones, params.speed, params.timbre.vowels);
    let plan = Plan::new(
        params.pitch,
        params.quality,
        &utterance.nuclei(),
        &utterance.phrases,
    );
    let mut samples = synthesize(&utterance, &plan, params.effort, params.timbre);
    finish(&mut samples);
    samples
}

/// Bring the peak to [`PEAK`] and fade both ends. Silence stays silent.
fn finish(samples: &mut [f32]) {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 1e-6 {
        return;
    }
    let gain = PEAK / peak;
    let fade = (FADE_MS * crate::RATE as f32 / 1000.0) as usize;
    let last = samples.len().saturating_sub(1);
    for (n, s) in samples.iter_mut().enumerate() {
        // One ramp in, one out, meeting in the middle of a short render.
        let into = (n as f32 / fade.max(1) as f32).min(1.0);
        let out_of = ((last - n) as f32 / fade.max(1) as f32).min(1.0);
        *s *= gain * into.min(out_of);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;
    use crate::params::{Effort, Quality};

    fn params() -> Params {
        Params::default()
    }

    #[test]
    fn renders_audible_bounded_speech() {
        let s = render("hello world", &params());
        assert!(!s.is_empty());
        assert!(s.iter().all(|x| x.abs() <= 1.0));
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.5, "peak {peak}");
    }

    /// A one-sided pulse leaves the wave on an offset that thumps each time
    /// voicing starts or stops.
    #[test]
    fn speech_carries_no_offset() {
        for voice in ["man", "big-robot", "child"] {
            let mut params = params();
            params.apply("personality", voice).unwrap();
            let s = render("my name is", &params);
            let mean = s.iter().sum::<f32>() / s.len() as f32;
            assert!(mean.abs() < 0.01, "{voice}: mean {mean}");
        }
    }

    #[test]
    fn empty_text_renders_nothing() {
        assert!(render("", &params()).is_empty());
        assert!(render("   ??  ", &params()).is_empty());
    }

    #[test]
    fn modes_and_languages_change_the_wave() {
        let plain = render("test me", &params());
        let monotone = render(
            "test me",
            &Params {
                quality: Quality::Monotone,
                ..params()
            },
        );
        let whisper = render(
            "test me",
            &Params {
                effort: Effort::Whispered,
                ..params()
            },
        );
        let spanish = render(
            "test me",
            &Params {
                language: Language::Spanish,
                ..params()
            },
        );
        assert_ne!(plain, monotone);
        assert_ne!(plain, whisper);
        assert_ne!(plain, spanish);
    }
}
