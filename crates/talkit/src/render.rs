//! Text and settings to finished samples.
//!
//! Text becomes the phonetic notation by the language's rules, and the
//! narrator speaks the notation. [`render_phonetic`] starts from the
//! notation itself.

use crate::params::Params;
use crate::phonetic::{Item, PhoneticError, parse};

/// Render `text` spoken with `params` as mono samples from -1 to 1 at
/// [`crate::RATE`]. Text with no speech sounds in it renders nothing.
pub fn render(text: &str, params: &Params) -> Vec<f32> {
    // The language rules write only valid notation.
    render_phonetic(&params.language.transcribe(text), params).unwrap_or_default()
}

/// Render a phonetic string, such as `/HEH5LOW`, spoken with `params`.
/// The language setting plays no part.
pub fn render_phonetic(text: &str, params: &Params) -> Result<Vec<f32>, PhoneticError> {
    let items = parse(text)?;
    if !items.iter().any(|i| matches!(i, Item::Sound { .. })) {
        return Ok(Vec::new());
    }
    Ok(crate::narrator::speak(&items, params))
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
        // The original engine's level: the plain voice peaks near half scale.
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.25, "peak {peak}");
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
