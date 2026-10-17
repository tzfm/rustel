//! The utterance timeline: phones placed in time as acoustic spans.
//!
//! Each phone becomes one span holding its [`Frame`] target. Three sounds
//! need more than that, and this is where they come apart:
//!
//! - a stop expands into a closure span followed by a short burst span,
//! - a diphthong into its head vowel followed by the glide target,
//! - the Spanish trill into alternating voiced and damped spans, which the
//!   interpolation turns into amplitude flutter.
//!
//! After the expansion every span is handled the same way, so the synthesizer
//! knows nothing about phonemes.

use crate::lang::Phone;
use crate::phoneme::{Frame, Phoneme, frame};

/// How the utterance ends, which sets the pitch of its last syllable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    None,
    /// Period or exclamation mark: the voice falls.
    Fall,
    /// Question mark: the voice rises.
    Rise,
}

/// One stretch of time with one acoustic target.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub frame: Frame,
    /// Start and end in ms from the utterance start.
    pub start: f32,
    pub end: f32,
    /// True while the span is a syllable nucleus the prosody plan pitches.
    pub nucleus: bool,
}

impl Span {
    fn new(frame: Frame, start: f32, end: f32, nucleus: bool) -> Self {
        Self {
            frame,
            start,
            end,
            nucleus,
        }
    }
}

/// The whole spoken timeline.
pub struct Utterance {
    pub spans: Vec<Span>,
    /// Total length in ms, including the closing pad.
    pub total_ms: f32,
    pub terminal: Terminal,
}

/// Pause lengths in ms, at any speed: they carry no speech to stretch.
const GAP_MS: f32 = 40.0;
const COMMA_MS: f32 = 220.0;
const END_MS: f32 = 420.0;
/// The burst a stop release adds, in ms before speed scaling.
const BURST_MS: f32 = 12.0;
/// The closing pause after the last sound, long enough for it to glide out.
const TAIL_MS: f32 = 60.0;
/// A stretched vowel is held longer than its unstressed form.
const STRETCH: f32 = 1.3;

/// Build the timeline for `phones`. `speed` is scaled against the nominal
/// 150, so 300 speaks twice as fast as the nominal and 75 half as fast.
pub fn build(phones: &[Phone], speed: u32) -> Utterance {
    let stretch = 150.0 / speed as f32;
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor = 0.0f32;
    let mut terminal = Terminal::None;

    for phone in phones {
        match phone {
            Phone::Gap => push_span(
                &mut spans,
                &mut cursor,
                frame(Phoneme::Pause),
                GAP_MS,
                false,
            ),
            Phone::Comma => push_span(
                &mut spans,
                &mut cursor,
                frame(Phoneme::Pause),
                COMMA_MS,
                false,
            ),
            Phone::Period => {
                push_span(
                    &mut spans,
                    &mut cursor,
                    frame(Phoneme::Pause),
                    END_MS,
                    false,
                );
                terminal = Terminal::Fall;
            }
            Phone::Question => {
                push_span(
                    &mut spans,
                    &mut cursor,
                    frame(Phoneme::Pause),
                    END_MS,
                    false,
                );
                terminal = Terminal::Rise;
            }
            Phone::Sound(ph) => expand(&mut spans, &mut cursor, *ph, stretch, 1.0),
            Phone::Stressed(ph) => expand(&mut spans, &mut cursor, *ph, stretch, STRETCH),
        }
    }

    // The closing pad is a pause, so the last sound glides out to silence
    // over the transition instead of holding: a final `s` stops hissing.
    push_span(
        &mut spans,
        &mut cursor,
        frame(Phoneme::Pause),
        TAIL_MS,
        false,
    );
    let total_ms = cursor;
    Utterance {
        spans,
        total_ms,
        terminal,
    }
}

/// Append one span of `ms` milliseconds, never shorter than 4 ms so every
/// span survives the speed scaling with a visible target.
fn push_span(spans: &mut Vec<Span>, cursor: &mut f32, frame: Frame, ms: f32, nucleus: bool) {
    let end = *cursor + ms.max(4.0);
    spans.push(Span::new(frame, *cursor, end, nucleus));
    *cursor = end;
}

/// Append the spans of one speech sound, at `stretch` for speed and `stress`
/// for emphasis.
fn expand(spans: &mut Vec<Span>, cursor: &mut f32, ph: Phoneme, stretch: f32, stress: f32) {
    let base = frame(ph);
    let dur = base.ms as f32 * stretch * stress;
    let nucleus = ph.is_nucleus();

    if ph.is_stop() {
        // Closure: the frame's locus formants with its voice bar, no noise.
        let mut closure = base;
        closure.fric_amp = 0.0;
        push_span(spans, cursor, closure, dur, false);
        // Burst: the same formants with only the frication active.
        let mut burst = base;
        burst.voice = 0.0;
        push_span(spans, cursor, burst, BURST_MS * stretch, false);
    } else if let Some(tail) = ph.diphthong_tail() {
        let tail_frame = frame(tail);
        push_span(spans, cursor, base, dur * 0.62, true);
        push_span(spans, cursor, tail_frame, dur * 0.38, false);
    } else if ph == Phoneme::Rr {
        // Four alternating spans: full voicing, damped, repeated. The
        // interpolation between them flutters the amplitude near 30 Hz.
        let quarter = dur / 4.0;
        for k in 0..4 {
            let mut part = base;
            part.voice = if k % 2 == 0 { 1.0 } else { 0.15 };
            push_span(spans, cursor, part, quarter, false);
        }
    } else {
        push_span(spans, cursor, base, dur, nucleus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;

    #[test]
    fn a_stop_becomes_closure_then_burst() {
        let phones = Language::English.transcribe("to");
        let u = build(&phones, 150);
        // "to" is the exception [T, UW]: T closure, T burst, then UW.
        assert!(u.spans.len() >= 3);
        assert_eq!(u.spans[0].frame.voice, 0.0);
        assert_eq!(u.spans[0].frame.fric_amp, 0.0);
        assert!(u.spans[1].frame.fric_amp > 0.0);
        assert_eq!(u.spans[1].frame.voice, 0.0);
        assert!(u.spans[2].nucleus);
    }

    #[test]
    fn speed_scales_durations_and_pauses_stay_fixed() {
        let phones = Language::English.transcribe("ah");
        let vowel = |speed| {
            let u = build(&phones, speed);
            let span = u.spans.iter().find(|span| span.nucleus).copied().unwrap();
            let tail = u.spans.last().unwrap();
            (span.end - span.start, tail.end - tail.start)
        };
        let (slow_vowel, slow_tail) = vowel(75);
        let (fast_vowel, fast_tail) = vowel(300);
        assert!(slow_vowel > fast_vowel * 3.5);
        assert_eq!(slow_tail, fast_tail, "the closing pause keeps its length");
    }
}
