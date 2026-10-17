//! Sounds placed in time.
//!
//! Every sound of the phonetic string becomes one segment, and a pause
//! becomes a silent one. Lengths follow the classic rule scheme for
//! synthetic speech: each sound has an inherent length and a minimum, and
//! its context scales the part above the minimum.
//!
//! ```text
//! length = MIN + (INH - MIN) x factors
//! ```
//!
//! The factors shorten a vowel that does not end its phrase, that does not
//! end its word, that sits in a word of several syllables, or that carries
//! no stress, and lengthen the vowel that ends a phrase. The table lengths
//! and the factors are fitted to the original engine's output at speed 150.

use super::table::{Class, target};
use crate::phonetic::{Item, Ph};

/// A stretch of the timeline: one sound, or silence.
#[derive(Clone, Copy, Debug)]
pub(super) struct Seg {
    /// The sound, or None for a pause.
    pub ph: Option<Ph>,
    /// The stress digit, 0 when unstressed.
    pub stress: u8,
    /// Start and end in ms.
    pub t0: f32,
    pub t1: f32,
    /// The mark that ends the phrase this segment belongs to, if any.
    pub ends: Option<char>,
    /// True for the first sound of its word.
    pub word_start: bool,
}

/// A vowel not in the last syllable of its phrase.
const NOT_PHRASE_FINAL: f32 = 0.499;
/// A vowel not in the last syllable of its word.
const NOT_WORD_FINAL: f32 = 0.57;
/// A vowel in a word of several syllables.
const POLYSYLLABIC: f32 = 0.769;
/// The vowel of a phrase's last syllable.
const PHRASE_FINAL: f32 = 1.582;
/// An unstressed vowel: its factor, and the share of its minimum it keeps.
const UNSTRESSED: f32 = 0.577;
const UNSTRESSED_MIN: f32 = 0.673;
/// A consonant that does not start its word.
const NOT_WORD_INITIAL: f32 = 0.569;
/// A consonant after the last vowel of its phrase.
const PHRASE_FINAL_CONSONANT: f32 = 2.427;
/// The vowel before a clause break `)` lengthens by this factor.
const BEFORE_CLAUSE: f32 = 1.6;
/// Pause lengths in ms at speed 150.
const COMMA_MS: f32 = 212.0;
const STOP_MS: f32 = 326.0;

/// Inherent and minimum length of a sound, ms.
fn lengths(ph: Ph) -> (f32, f32) {
    use Ph::*;
    match ph {
        Iy => (156.0, 63.0),
        Ih => (147.0, 61.0),
        Eh => (165.0, 86.0),
        Ae => (230.0, 90.0),
        Aa => (255.0, 115.0),
        Ah => (174.0, 88.0),
        Ao => (273.0, 117.0),
        Ow => (240.0, 90.0),
        Oh => (173.0, 65.0),
        Uh => (196.0, 64.0),
        Uw => (241.0, 90.0),
        Er => (175.0, 81.0),
        Ax => (118.0, 35.0),
        Ix => (108.0, 36.0),
        Ey => (195.0, 107.0),
        Ay => (227.0, 135.0),
        Oy => (298.0, 153.0),
        Aw => (208.0, 79.0),
        Ul => (264.0, 119.0),
        In => (210.0, 95.0),
        Un => (190.0, 86.0),
        Um | Il | Im => (200.0, 90.0),
        B => (76.0, 53.0),
        Ch | C => (104.0, 75.0),
        D => (75.0, 50.0),
        Dh => (50.0, 30.0),
        Dx => (20.0, 20.0),
        F => (100.0, 80.0),
        G => (80.0, 60.0),
        H | Hh => (80.0, 20.0),
        Kh => (100.0, 60.0),
        J => (62.0, 44.0),
        K => (54.0, 40.0),
        L | Lx => (77.0, 38.0),
        M => (70.0, 60.0),
        N => (60.0, 50.0),
        Nx => (95.0, 45.0),
        P => (90.0, 50.0),
        Q => (40.0, 20.0),
        Qx => (40.0, 20.0),
        R | Rx => (69.0, 26.0),
        S => (81.0, 46.0),
        Sh => (105.0, 80.0),
        T => (52.0, 35.0),
        Th => (90.0, 60.0),
        V => (73.0, 49.0),
        W | Wh => (76.0, 57.0),
        Y => (50.0, 25.0),
        Z => (75.0, 40.0),
        Zh => (70.0, 40.0),
    }
}

/// Where a sound sits: its word and phrase, by index into the items.
struct Place {
    vowels_in_word: usize,
    word_final_syllable: bool,
    phrase_final_syllable: bool,
    word_initial: bool,
    vowel_follows_in_word: bool,
    after_vowel: bool,
}

/// The place of the sound at `at`. A syllable runs from its vowel up to the
/// next vowel of the word, so the consonants after the last vowel of a word
/// belong to the word's last syllable.
fn place(items: &[Item], at: usize) -> Place {
    let is_sound = |i: &Item| matches!(i, Item::Sound { .. });
    let is_vowel = |i: &Item| matches!(i, Item::Sound { ph, .. } if ph.is_vowel());
    let word_start = items[..at]
        .iter()
        .rposition(|i| !is_sound(i))
        .map_or(0, |p| p + 1);
    let word_end = items[at..]
        .iter()
        .position(|i| !is_sound(i))
        .map_or(items.len(), |p| at + p);
    let word = &items[word_start..word_end];
    let vowels_in_word = word.iter().filter(|i| is_vowel(i)).count();
    let later_vowel_in_word = items[at + 1..word_end].iter().any(is_vowel);
    let phrase_end = items[at..]
        .iter()
        .position(|i| matches!(i, Item::Mark(_)))
        .map_or(items.len(), |p| at + p);
    let later_vowel_in_phrase = items[at + 1..phrase_end].iter().any(is_vowel);
    Place {
        vowels_in_word,
        word_final_syllable: !later_vowel_in_word,
        phrase_final_syllable: !later_vowel_in_phrase,
        word_initial: at == word_start,
        vowel_follows_in_word: later_vowel_in_word,
        after_vowel: items[word_start..at].iter().any(is_vowel),
    }
}

/// The timeline of `items` at `speed`, with vowels lengthened by `vowels`
/// percent.
pub(super) fn build(items: &[Item], speed: u32, vowels: i16) -> Vec<Seg> {
    // The original engine reads the speed in steps of two.
    let scale = 150.0 / (speed & !1).max(2) as f32;
    let vowel_scale = f32::from(vowels) / 100.0;
    let mut segs: Vec<Seg> = Vec::new();
    let mut t = 0.0;
    for (at, item) in items.iter().enumerate() {
        match *item {
            Item::Sound { ph, stress } => {
                let (inh, mut min) = lengths(ph);
                let pl = place(items, at);
                let mut p = 1.0;
                if ph.is_vowel() {
                    p *= if pl.phrase_final_syllable {
                        PHRASE_FINAL
                    } else {
                        NOT_PHRASE_FINAL
                    };
                    if !pl.word_final_syllable {
                        p *= NOT_WORD_FINAL;
                    }
                    if pl.vowels_in_word > 1 {
                        p *= POLYSYLLABIC;
                    }
                    if stress == 0 {
                        p *= UNSTRESSED;
                        min *= UNSTRESSED_MIN;
                    }
                } else {
                    if !pl.word_initial {
                        p *= NOT_WORD_INITIAL;
                    }
                    if pl.phrase_final_syllable && !pl.vowel_follows_in_word && pl.after_vowel {
                        p *= PHRASE_FINAL_CONSONANT;
                    }
                }
                let mut ms = min + (inh - min) * p;
                if ph.is_vowel() {
                    ms *= vowel_scale;
                } else if target(ph).class == Class::Glottal {
                    ms *= 0.6;
                }
                let dur = ms * scale;
                segs.push(Seg {
                    ph: Some(ph),
                    stress,
                    t0: t,
                    t1: t + dur,
                    ends: None,
                    word_start: pl.word_initial,
                });
                t += dur;
            }
            Item::Mark(')') => {
                // A clause break: the vowel before it lengthens, with no
                // pause and no change of phrase.
                if let Some(v) = segs.iter().rposition(|s| s.ph.is_some_and(Ph::is_vowel)) {
                    let extra = (segs[v].t1 - segs[v].t0) * (BEFORE_CLAUSE - 1.0);
                    segs[v].t1 += extra;
                    for seg in &mut segs[v + 1..] {
                        seg.t0 += extra;
                        seg.t1 += extra;
                    }
                    t += extra;
                }
            }
            Item::Mark(mark) => {
                let ms = match mark {
                    ',' | '-' | ':' => COMMA_MS,
                    '.' | '?' if at + 1 < items.len() => STOP_MS,
                    _ => 0.0,
                };
                // The phrase that ends here: its segments take the mark.
                for seg in segs.iter_mut().rev() {
                    if seg.ends.is_some() {
                        break;
                    }
                    seg.ends = Some(mark);
                }
                if ms > 0.0 {
                    let dur = ms * scale;
                    segs.push(Seg {
                        ph: None,
                        stress: 0,
                        t0: t,
                        t1: t + dur,
                        ends: Some(mark),
                        word_start: false,
                    });
                    t += dur;
                }
            }
            Item::Word => {}
        }
    }
    segs
}
