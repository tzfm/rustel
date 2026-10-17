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
use crate::prosody::Nucleus;

/// How a phrase ends, which sets the pitch of its last syllable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    /// The text simply stops: the pitch holds.
    None,
    /// Comma, semicolon or colon: the voice lifts a little to go on.
    Continue,
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
    /// True for the nucleus of a stressed syllable.
    pub accent: bool,
    /// The phrase the span belongs to, an index into
    /// [`Utterance::phrases`].
    pub phrase: usize,
}

impl Span {
    fn new(frame: Frame, start: f32, end: f32, nucleus: bool) -> Self {
        Self {
            frame,
            start,
            end,
            nucleus,
            accent: false,
            phrase: 0,
        }
    }
}

/// The whole spoken timeline.
pub struct Utterance {
    pub spans: Vec<Span>,
    /// Total length in ms, including the closing pad.
    pub total_ms: f32,
    /// How each phrase ends, in order. A pause ends a phrase, and the last
    /// phrase ends with the text.
    pub phrases: Vec<Terminal>,
}

impl Utterance {
    /// The syllable nuclei, in time order, as the pitch plan reads them.
    pub fn nuclei(&self) -> Vec<Nucleus> {
        self.spans
            .iter()
            .filter(|span| span.nucleus)
            .map(|span| Nucleus {
                start: span.start,
                end: span.end,
                accent: span.accent,
                phrase: span.phrase,
            })
            .collect()
    }
}

/// Pause lengths in ms, at any speed: they carry no speech to stretch.
const GAP_MS: f32 = 40.0;
const COMMA_MS: f32 = 160.0;
const END_MS: f32 = 200.0;
/// The original engine's pace: at the same speed its sounds last this share
/// of the table's nominal durations.
const PACE: f32 = 0.69;
/// The last vowel before a pause, or at the end, is held this much longer,
/// as the original engine holds it.
const FINAL_STRETCH: f32 = 1.9;
/// The burst a stop release adds, in ms before speed scaling.
const BURST_MS: f32 = 12.0;
/// The closing pause after the last sound, long enough for it to glide out.
const TAIL_MS: f32 = 60.0;
/// A stretched vowel is held longer than its unstressed form.
const STRETCH: f32 = 1.3;

/// Build the timeline for `phones`. `speed` is scaled against the nominal
/// 150, so 300 speaks twice as fast as the nominal and 75 half as fast.
/// `vowels` is the voice's own factor on vowel lengths.
pub fn build(phones: &[Phone], speed: u32, vowels: f32) -> Utterance {
    let stretch = 150.0 / speed as f32 * PACE;
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor = 0.0f32;
    let mut phrases = Vec::new();
    let finals = final_vowels(phones);

    for (at, phone) in phones.iter().enumerate() {
        let pause = match phone {
            Phone::Gap => Some((GAP_MS, None)),
            Phone::Comma => Some((COMMA_MS, Some(Terminal::Continue))),
            Phone::Period => Some((END_MS, Some(Terminal::Fall))),
            Phone::Question => Some((END_MS, Some(Terminal::Rise))),
            Phone::Sound(_) | Phone::Stressed(_) => None,
        };
        let from = spans.len();
        let mut ending = None;
        match (pause, phone) {
            (Some((ms, ends)), _) => {
                push_span(&mut spans, &mut cursor, frame(Phoneme::Pause), ms, false);
                ending = ends;
            }
            (None, Phone::Sound(ph) | Phone::Stressed(ph)) => {
                let stressed = matches!(phone, Phone::Stressed(_));
                let stress = if stressed { STRETCH } else { 1.0 };
                let mut hold = if ph.is_nucleus() { vowels } else { 1.0 };
                if finals.contains(&at) {
                    hold *= FINAL_STRETCH;
                }
                expand(&mut spans, &mut cursor, *ph, stretch, stress * hold);
                for span in &mut spans[from..] {
                    span.accent = span.nucleus && stressed;
                }
            }
            (None, _) => {}
        }
        for span in &mut spans[from..] {
            span.phrase = phrases.len();
        }
        // The pause that ends a phrase belongs to it; what follows starts
        // the next one.
        phrases.extend(ending);
    }
    phrases.push(Terminal::None);

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
        phrases,
    }
}

/// The positions of the last vowel before each pause and before the end.
fn final_vowels(phones: &[Phone]) -> Vec<usize> {
    let mut finals = Vec::new();
    let mut last = None;
    for (at, phone) in phones.iter().enumerate() {
        match phone {
            Phone::Sound(ph) | Phone::Stressed(ph) if ph.is_nucleus() => last = Some(at),
            Phone::Comma | Phone::Period | Phone::Question => finals.extend(last.take()),
            _ => {}
        }
    }
    finals.extend(last);
    finals
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
        let u = build(&phones, 150, 1.0);
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
            let u = build(&phones, speed, 1.0);
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
