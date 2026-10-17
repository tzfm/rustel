//! The pitch of an utterance over time.
//!
//! Natural pitch is a line through one or two anchor points per syllable,
//! the way the original engine builds it. Within a phrase:
//!
//! - Syllables before the first accent hold the starting level, a little
//!   under the pitch setting.
//! - An accent is a stressed vowel with digit 4 or more. The first one
//!   rises to a height set by its digit and by how many words its phrase
//!   has, higher again in a word of several syllables.
//! - Each later accent rises a smaller share of its height, so the accents
//!   step down through the phrase.
//! - After an accent that is not the last, each syllable falls a fixed share
//!   of the way down to a floor. The floor is higher when more accents
//!   follow.
//! - After the last accent, the voice falls in a straight line to the end
//!   of the phrase: low after a full stop, level before the final rise of a
//!   question or an open ending.
//! - A phrase with no accent slides from the starting level down to its end
//!   after a full stop, and holds before a final rise otherwise.
//! - A mid vowel lifts its syllable a little and a close vowel more.
//!
//! ```text
//!          accent
//!         ┌───┐
//!  start  │   └╮        last accent
//! ────────┘    ╰╮   ┌──┐
//!               ╰───┘  └───╮
//!                           ╰── end
//! ```
//!
//! Every level is a share of the pitch setting, so the shape moves with the
//! note. The voice's range stretches the line where it runs above the
//! setting and keeps it as it is below, and the shape is the same at every
//! speed. The values are fitted to the original engine's output.
//!
//! Monotone holds the pitch setting. Sung steps through a scale around it,
//! one note per syllable, picked by the syllable's first consonant. An
//! unsteady voice wanders slowly around its line in every quality.

use super::timing::Seg;
use crate::params::Quality;
use crate::personality::Timbre;
use crate::phonetic::Ph;

/// The level before the first accent.
const START: f32 = 0.955;
/// Accent height per stress digit 4 to 9, at range 100, in a phrase of one
/// word, of two to four words, and of five or more.
const HEIGHT: [[f32; 6]; 3] = [
    [0.056, 0.106, 0.12, 0.156, 0.195, 0.234],
    [0.121, 0.156, 0.177, 0.234, 0.247, 0.286],
    [0.156, 0.249, 0.289, 0.367, 0.425, 0.494],
];
/// The extra height of an accent in a word of several syllables, in a phrase
/// of two to four words and of five or more.
const SEVERAL: [f32; 2] = [0.045, 0.091];
/// Later accents rise a share of their height: `LATER` for the second, and
/// `DECAY` times less for each one after it. The last accent of a phrase
/// takes `LAST` of that share.
const LATER: f32 = 0.718;
const DECAY: f32 = 0.665;
const LAST: f32 = 0.641;
/// After an accent that is not the last, each syllable falls `STEP` of the
/// way from the accent to a floor, `FLOOR` with one accent to come and
/// `AHEAD` higher for each further one.
const STEP: f32 = 0.37;
const FLOOR: f32 = 0.888;
const AHEAD: f32 = 0.056;
/// Where a phrase ends after a full stop, with and without accents.
const FALL_ACCENTED: f32 = 0.695;
const FALL_PLAIN: f32 = 0.767;
/// The level the line reaches before the final rise of a question or an
/// open ending.
const BEFORE_QUESTION: f32 = 0.996;
const BEFORE_OPEN: f32 = 1.022;
/// The level of a phrase with no accent before a final rise.
const PLAIN_LEVEL: f32 = 0.97;
/// The final rise over the last syllable: after an accent, and without one.
const RISE_QUESTION: f32 = 0.276;
const RISE_OPEN: f32 = 0.057;
const RISE_QUESTION_PLAIN: f32 = 0.15;
const RISE_OPEN_PLAIN: f32 = 0.198;
/// An accent reaches its height this long after its syllable starts, ms at
/// speed 150.
const ACCENT_DELAY: f32 = 138.0;
/// The lift of the mid and the close vowels.
const MID_VOWEL: f32 = 0.048;
const CLOSE_VOWEL: f32 = 0.078;

/// One syllable as the pitch rules see it.
#[derive(Clone, Copy)]
struct Syllable {
    onset: f32,
    v0: f32,
    v1: f32,
    digit: u8,
    /// The number of the syllable's word in its phrase, from 1.
    word: usize,
    /// How far the vowel lifts the syllable.
    lift: f32,
}

/// How far a vowel lifts its syllable, as a share of the pitch setting at
/// range 100: the open vowels not at all, the close ones most.
fn lift(ph: Ph) -> f32 {
    use Ph::*;
    match ph {
        Aa | Ae | Ao | Ay | Aw => 0.0,
        Iy | Ih | Uh | Uw => CLOSE_VOWEL,
        _ => MID_VOWEL,
    }
}

impl Syllable {
    fn mid(&self) -> f32 {
        (self.v0 + self.v1) / 2.0
    }
}

pub(super) struct Pitch {
    /// The pitch outside natural intonation.
    held: f32,
    /// Natural anchor points in time order: ms, Hz.
    points: Vec<(f32, f32)>,
    next: usize,
    wander: Wander,
    /// How far the wander moves the pitch, as a share of it.
    depth: f32,
}

/// The phrases of a timeline: their syllables and their closing mark.
fn phrases(segs: &[Seg]) -> Vec<(Vec<Syllable>, char)> {
    let mut out = Vec::new();
    let mut syls: Vec<Syllable> = Vec::new();
    let mut onset: Option<f32> = None;
    let mut word = 0;
    for (i, seg) in segs.iter().enumerate() {
        if seg.word_start {
            word += 1;
        }
        match seg.ph {
            None => {
                onset = None;
                if !syls.is_empty() {
                    out.push((std::mem::take(&mut syls), seg.ends.unwrap_or(' ')));
                    word = 0;
                }
                continue;
            }
            Some(ph) if ph.is_vowel() => {
                syls.push(Syllable {
                    onset: onset.unwrap_or(seg.t0),
                    v0: seg.t0,
                    v1: seg.t1,
                    digit: seg.stress,
                    word: word.max(1),
                    lift: lift(ph),
                });
                onset = None;
            }
            Some(_) => {
                if onset.is_none() {
                    onset = Some(seg.t0);
                }
            }
        }
        let phrase_ends = segs.get(i + 1).is_none_or(|next| next.ends != seg.ends);
        if phrase_ends && !syls.is_empty() {
            out.push((std::mem::take(&mut syls), seg.ends.unwrap_or(' ')));
            onset = None;
            word = 0;
        }
    }
    if !syls.is_empty() {
        out.push((syls, ' '));
    }
    out
}

/// The height of the accent on syllable `i` of a phrase.
fn height(syls: &[Syllable], i: usize) -> f32 {
    let words = syls.last().map_or(1, |s| s.word);
    let class = match words {
        1 => 0,
        2..=4 => 1,
        _ => 2,
    };
    let digit = usize::from(syls[i].digit.clamp(4, 9) - 4);
    let several = syls.iter().filter(|s| s.word == syls[i].word).count() > 1;
    HEIGHT[class][digit]
        + if several && class > 0 {
            SEVERAL[class - 1]
        } else {
            0.0
        }
}

/// The anchor points of one phrase, as shares of the pitch setting at range
/// 100.
fn anchors(syls: &[Syllable], terminal: char, delay: f32, out: &mut Vec<(f32, f32)>) {
    let n = syls.len();
    let accents: Vec<usize> = (0..n).filter(|&i| syls[i].digit >= 4).collect();
    let mut vals = vec![0.0; n];
    match (accents.first(), accents.last()) {
        (Some(&first), Some(&last)) => {
            vals[..first].fill(START);
            let m = accents.len();
            for (j, &a) in accents.iter().enumerate() {
                let mut share = if j == 0 {
                    1.0
                } else {
                    LATER * DECAY.powi(j as i32 - 1)
                };
                if j > 0 && j == m - 1 {
                    share *= LAST;
                }
                vals[a] = 1.0 + height(syls, a) * share;
            }
            for (j, pair) in accents.windows(2).enumerate() {
                let (a, b) = (pair[0], pair[1]);
                let floor = (FLOOR + AHEAD * (m - j - 2) as f32).min(vals[a]);
                for (k, i) in (a + 1..b).enumerate() {
                    let w = (1.0 - STEP * (k + 1) as f32).max(0.0);
                    vals[i] = floor + (vals[a] - floor) * w;
                }
            }
            let from = (syls[last].v1, vals[last]);
            let end = match terminal {
                '.' => FALL_ACCENTED,
                '?' => BEFORE_QUESTION,
                _ => BEFORE_OPEN,
            };
            let span = (syls[n - 1].mid() - from.0).max(1.0);
            for i in last + 1..n {
                let x = ((syls[i].mid() - from.0) / span).clamp(0.0, 1.0);
                vals[i] = from.1 + (end - from.1) * x;
            }
        }
        _ => {
            for (i, v) in vals.iter_mut().enumerate() {
                *v = if terminal == '.' {
                    let span = syls[n - 1].mid() - syls[0].mid();
                    let x = if span > 0.0 {
                        (syls[i].mid() - syls[0].mid()) / span
                    } else {
                        0.0
                    };
                    START + (FALL_PLAIN - START) * x
                } else {
                    PLAIN_LEVEL
                };
            }
        }
    }
    for (v, syl) in vals.iter_mut().zip(syls) {
        *v += syl.lift;
    }
    let plain = accents.is_empty();
    for (i, syl) in syls.iter().enumerate() {
        let accented = syl.digit >= 4;
        let last = i == n - 1;
        let rise = match (terminal, plain) {
            ('.', _) => 0.0,
            ('?', true) => RISE_QUESTION_PLAIN,
            ('?', false) => RISE_QUESTION,
            (_, true) => RISE_OPEN_PLAIN,
            (_, false) => RISE_OPEN,
        };
        if accented {
            out.push((syl.onset + delay, vals[i]));
            let end = if !last {
                vals[i]
            } else if terminal == '.' {
                vals[i] + (FALL_ACCENTED - vals[i]) * 0.6
            } else {
                vals[i] + rise
            };
            out.push((syl.v1, end));
        } else if last && terminal != '.' {
            out.push((syl.v0, vals[i]));
            out.push((syl.v1, vals[i] + rise));
        } else {
            out.push((syl.mid(), vals[i]));
        }
    }
}

/// The range stretches a level above the setting and keeps one below it.
fn stretch(level: f32, range: f32) -> f32 {
    if level > 1.0 {
        1.0 + (level - 1.0) * range
    } else {
        level
    }
}

/// Sung speech steps through this scale, one note per syllable: the pitch
/// setting in whole Hz plus the step of the syllable's first sound, modulo
/// 8, picks the ratio. This is how the original engine sings.
const SUNG: [f32; 8] = [0.833, 0.94, 1.0, 1.112, 1.256, 1.405, 1.49, 0.74];
/// The voice glides to a new note this long from the start of the sound
/// that sets it, ms at speed 150.
const GLIDE_MS: f32 = 50.0;

/// The step a sound adds to the note of the syllable it sets, or None for
/// the sounds that keep the note: the dark `l`, the `r` after a vowel and
/// the silent closure.
fn step(ph: Ph) -> Option<usize> {
    use Ph::*;
    Some(match ph {
        Lx | Rx | Qx => return None,
        T | Sh | J | Ah => 1,
        F | H | Hh | Ih | Ao | Oy => 2,
        Th | M => 3,
        K | B | Z | N | Wh | Eh | Uh | Aw => 4,
        D | Zh | R | Nx | Ul => 5,
        V | L | Dx | Ae | Ow | Ax | Ey | Oh | Um | Un => 6,
        G | Dh | W | Q | Ix | Il | Im | In => 7,
        _ => 0,
    })
}

/// The notes of sung speech, ms and Hz.
///
/// A syllable that starts with a consonant takes its note from the first
/// consonant of its word when it starts the word, and inside a word from the
/// second consonant of a cluster, so `sister` takes its second note from
/// `t`. A syllable that starts with a vowel keeps the note, unless the vowel
/// before it is unstressed: then that vowel sets the note. After a full stop
/// the voice starts again from the pitch setting.
fn sung_points(segs: &[Seg], base: f32, glide: f32) -> Vec<(f32, f32)> {
    let index = base.round().max(0.0) as usize;
    let note = |step: usize| base * SUNG[(index + step) % SUNG.len()];
    let mut out: Vec<(f32, f32)> = vec![(0.0, note(0))];
    let mut last_vowel: Option<&Seg> = None;
    for (i, seg) in segs.iter().enumerate() {
        if seg.ph.is_none() && matches!(seg.ends, Some('.' | '?')) {
            last_vowel = None;
        }
        if !seg.ph.is_some_and(Ph::is_vowel) {
            continue;
        }
        // The consonants before the vowel, back to its word's start, a
        // vowel or a pause.
        let mut start = i;
        while start > 0
            && !segs[start].word_start
            && segs[start - 1].ph.is_some_and(|ph| !ph.is_vowel())
        {
            start -= 1;
        }
        let (at, step) = if start < i {
            // Inside a word, a cluster gives its first consonant to the
            // syllable before.
            let first = if i - start >= 2 && !segs[start].word_start {
                start + 1
            } else {
                start
            };
            (segs[first].t0, segs[first].ph.and_then(step))
        } else {
            let after_weak = last_vowel.filter(|v| v.stress == 0).and_then(|v| v.ph);
            let step = match last_vowel {
                None => Some(0),
                Some(_) => after_weak.and_then(step),
            };
            (seg.t0, step)
        };
        if let Some(step) = step {
            let &(last, prev) = out.last().unwrap_or(&(0.0, base));
            let at = at.max(last);
            out.push((at, prev));
            out.push((at + glide, note(step)));
        }
        last_vowel = Some(seg);
    }
    out
}

/// The wander of an unsteady voice: a new random target this often, ms.
const WANDER_MS: f32 = 250.0;
/// The wander's depth per unit of the record's perturbation.
const WANDER_DEPTH: f32 = 0.001;

/// A slow, smooth random walk around 0 that stays within -1 and 1.
/// Deterministic: the same utterance wanders the same way.
struct Wander {
    seed: u32,
    from: f32,
    to: f32,
    at: f32,
}

impl Wander {
    fn new() -> Self {
        let mut w = Self {
            seed: 0x2545_f491,
            from: 0.0,
            to: 0.0,
            at: 0.0,
        };
        w.to = w.draw();
        w
    }

    fn draw(&mut self) -> f32 {
        self.seed = self
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        (self.seed >> 8) as f32 / 8_388_608.0 - 1.0
    }

    fn at(&mut self, t: f32) -> f32 {
        while t >= self.at + WANDER_MS {
            self.at += WANDER_MS;
            self.from = self.to;
            self.to = self.draw();
        }
        let x = (t - self.at) / WANDER_MS;
        let ease = 0.5 - 0.5 * (std::f32::consts::PI * x).cos();
        self.from + (self.to - self.from) * ease
    }
}

impl Pitch {
    pub(super) fn plan(
        segs: &[Seg],
        quality: Quality,
        base: f32,
        speed: u32,
        timbre: &Timbre,
    ) -> Self {
        let range = f32::from(timbre.range) / 100.0;
        let scale = 150.0 / (speed & !1).max(2) as f32;
        let mut points = Vec::new();
        match quality {
            Quality::Natural => {
                for (syls, terminal) in phrases(segs) {
                    anchors(&syls, terminal, ACCENT_DELAY * scale, &mut points);
                }
                for p in &mut points {
                    p.1 = base * stretch(p.1, range);
                }
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
            Quality::Sung => points = sung_points(segs, base, GLIDE_MS * scale),
            Quality::Monotone => {}
        }
        Self {
            held: base,
            points,
            next: 0,
            wander: Wander::new(),
            depth: f32::from(timbre.perturb) * WANDER_DEPTH,
        }
    }

    /// The pitch at `t` ms. Times must not go back.
    pub(super) fn at(&mut self, t: f32) -> f32 {
        let line = if self.points.is_empty() {
            self.held
        } else {
            while self.next + 1 < self.points.len() && self.points[self.next + 1].0 <= t {
                self.next += 1;
            }
            let (t0, v0) = self.points[self.next];
            match self.points.get(self.next + 1) {
                Some(&(t1, v1)) if t > t0 && t1 > t0 => {
                    v0 + (v1 - v0) * ((t - t0) / (t1 - t0)).min(1.0)
                }
                _ => v0,
            }
        };
        if self.depth > 0.0 {
            line * (1.0 + self.depth * self.wander.at(t))
        } else {
            line
        }
    }
}

/// Outside monotone, the voice grows softer as the pitch falls below the
/// setting, in dB per octave. A rise lifts the voice by a level that it
/// reaches `LIFT_OCTAVES` above the setting. The level grows with the
/// setting, from `LIFT_LOW` at `LIFT_FROM` Hz to `LIFT_HIGH` at `LIFT_TO` Hz.
const FALL_DB: f32 = 9.8;
const LIFT_LOW: f32 = 1.0;
const LIFT_HIGH: f32 = 1.6;
const LIFT_FROM: f32 = 100.0;
const LIFT_TO: f32 = 150.0;
const LIFT_OCTAVES: f32 = 0.1;

/// The voicing gain at pitch `f0` for the setting `base`.
pub(super) fn loudness(f0: f32, base: f32) -> f32 {
    if f0 <= 0.0 || base <= 0.0 {
        return 1.0;
    }
    let octaves = (f0 / base).log2();
    let db = if octaves > 0.0 {
        let span = ((base / LIFT_FROM).log2() / (LIFT_TO / LIFT_FROM).log2()).clamp(0.0, 1.0);
        let lift = LIFT_LOW + (LIFT_HIGH - LIFT_LOW) * span;
        lift * (octaves / LIFT_OCTAVES).min(1.0)
    } else {
        FALL_DB * octaves
    };
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phonetic::parse;

    /// The original engine sings `SAA TAA KAA` at a setting of 146 Hz on
    /// three steps of its scale: S adds none, T one and K four.
    #[test]
    fn sung_notes_follow_the_first_consonant() {
        let items = parse("SAA5 TAA5 KAA5.").unwrap();
        let segs = super::super::timing::build(&items, 150, 100);
        let timbre = crate::personality::Personality::Man.timbre();
        let mut pitch = Pitch::plan(&segs, Quality::Sung, 146.0, 150, &timbre);
        let notes: Vec<f32> = segs
            .iter()
            .filter(|s| s.ph.is_some_and(Ph::is_vowel))
            .map(|s| pitch.at(s.t1 - 5.0))
            .collect();
        let want = [146.0, 146.0 * 1.112, 146.0 * 1.49];
        for (got, want) in notes.iter().zip(want) {
            assert!((got - want).abs() < 1.0, "{notes:?}");
        }
    }

    /// Natural pitch at the middle of each vowel, as shares of 100 Hz.
    fn natural(phon: &str, range: i16, speed: u32) -> Vec<f32> {
        let items = parse(phon).unwrap();
        let segs = super::super::timing::build(&items, speed, 100);
        let mut timbre = crate::personality::Personality::Man.timbre();
        timbre.range = range;
        timbre.perturb = 0;
        let mut pitch = Pitch::plan(&segs, Quality::Natural, 100.0, speed, &timbre);
        segs.iter()
            .filter(|s| s.ph.is_some_and(Ph::is_vowel))
            .map(|s| pitch.at((s.t0 + s.t1) / 2.0) / 100.0)
            .collect()
    }

    /// In the original engine a wider range stretches the line above the
    /// setting and keeps the fall below it, and the line keeps its shape at
    /// any speed.
    #[test]
    fn range_stretches_the_rise_and_speed_keeps_the_shape() {
        let phon = "SAA SAA5 SAA SAA4 SAA SAA SAA.";
        let (narrow, wide) = (natural(phon, 100, 150), natural(phon, 350, 150));
        for (n, w) in narrow.iter().zip(&wide) {
            let want = if *n > 1.0 { 1.0 + 3.5 * (n - 1.0) } else { *n };
            assert!((w - want).abs() < 0.01, "{narrow:?} {wide:?}");
        }
        let fast = natural(phon, 100, 300);
        for (a, b) in narrow.iter().zip(&fast) {
            assert!((a - b).abs() < 0.02, "{narrow:?} {fast:?}");
        }
    }

    /// In the original engine an accent rises higher in a phrase of more
    /// words, later accents step down, and a close vowel lifts its syllable.
    #[test]
    fn accents_follow_the_phrase_and_close_vowels_lift() {
        let short = natural("SAA5 SAA SAA.", 100, 150);
        let long = natural("SAA5 SAA SAA SAA SAA SAA.", 100, 150);
        assert!(long[0] > short[0] + 0.05, "{short:?} {long:?}");
        let steps = natural("SAA5 SAA SAA SAA5 SAA SAA SAA5 SAA SAA.", 100, 150);
        assert!(steps[0] > steps[3] && steps[3] > steps[6], "{steps:?}");
        let close = natural("SIY5 SIY SIY SIY SIY SIY.", 100, 150);
        for (open, close) in long.iter().zip(&close) {
            assert!(close > open, "{long:?} {close:?}");
        }
    }
}
