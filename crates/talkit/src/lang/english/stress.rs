//! Where a word the letter rules read takes its stress.
//!
//! One vowel carries the stress, with the digit 5 the original engine gives
//! the words its letter rules read. The reduced vowels and the syllabic
//! consonants do not carry it. The stress goes on the first vowel that can
//! carry it, with two exceptions:
//!
//! - An ending such as `-tion` or `-ic` pulls it to the vowel before the
//!   ending, as in `edition`.
//! - An unstressed prefix such as `be`, `re` or `con` passes it to the next
//!   vowel, as in `default`, and its own vowel weakens: `con` reads `KAXN`,
//!   `com` reads `KUM` and `in` reads `IN`.

use super::Sounds;
use crate::phonetic::Ph;

/// Endings that put the stress on the vowel just before them, by spelling,
/// with the number of vowel sounds they hold.
const ENDINGS: &[(&str, usize)] = &[
    ("tions", 1),
    ("tion", 1),
    ("sions", 1),
    ("sion", 1),
    ("cian", 1),
    ("tial", 1),
    ("cial", 1),
    ("ically", 3),
    ("ical", 2),
    ("ics", 1),
    ("ic", 1),
    ("ious", 1),
    ("eous", 1),
    ("ity", 2),
    ("ities", 2),
];

/// How a prefix's vowel weakens when the stress passes it.
#[derive(Clone, Copy)]
enum Weak {
    /// The vowel becomes this one, unless it is a long `IY`.
    To(Ph),
    /// The vowel and the nasal after it become one syllabic sound.
    Syllabic(Ph),
    /// The vowel stays.
    Same,
}

/// Prefixes that pass the stress on: their letters, how their first vowel
/// weakens, and the letters they do not come before. They count only before
/// a consonant.
const PREFIXES: &[(&str, Weak, &str)] = &[
    ("be", Weak::To(Ph::Ix), ""),
    ("de", Weak::To(Ph::Ih), ""),
    ("re", Weak::To(Ph::Ix), ""),
    ("pre", Weak::To(Ph::Iy), ""),
    ("con", Weak::To(Ph::Ax), ""),
    ("com", Weak::Syllabic(Ph::Um), ""),
    ("under", Weak::Same, ""),
    ("inter", Weak::Same, ""),
    ("over", Weak::Same, ""),
    ("in", Weak::Syllabic(Ph::In), ""),
    ("en", Weak::Same, "t"),
    ("es", Weak::Same, "t"),
    ("un", Weak::Same, ""),
    ("dis", Weak::Same, ""),
    ("ex", Weak::Same, ""),
    ("ef", Weak::To(Ph::Ah), ""),
    ("per", Weak::Same, ""),
    ("sup", Weak::Same, ""),
    ("suc", Weak::Same, ""),
    ("sug", Weak::Same, ""),
];

fn can_carry(ph: Ph) -> bool {
    ph.is_vowel()
        && !matches!(
            ph,
            Ph::Ax | Ph::Ix | Ph::Ul | Ph::Um | Ph::Un | Ph::Il | Ph::Im | Ph::In
        )
}

fn vowel_letter(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

/// Whether consonant letters can start a syllable, as `st` and `pl` can and
/// `nd` cannot.
fn onset(c: &[char]) -> bool {
    match *c {
        [_] => true,
        ['s', x] => "ptkcmnlwq".contains(x),
        [x, 'h'] => "tscpw".contains(x),
        [x, 'l'] => "pbcgfks".contains(x),
        [x, 'r'] => "pbcgfktd".contains(x),
        ['s', 't' | 'p' | 'c' | 'k', 'r'] | ['s', 'p' | 'c' | 'k', 'l'] => true,
        _ => false,
    }
}

/// The prefix `word` starts with, if it has one that passes the stress:
/// how its vowel weakens, and how many vowels it holds. A prefix that ends
/// in a vowel counts only before consonants that can start a syllable, so
/// `render` keeps its stress where `return` passes it on.
fn prefix(word: &str) -> Option<(Weak, usize)> {
    let found = PREFIXES.iter().find(|(p, _, not_before)| {
        let rest: Vec<char> = word[p.len().min(word.len())..].chars().collect();
        let cluster: Vec<char> = rest
            .iter()
            .copied()
            .take_while(|&c| !vowel_letter(c))
            .collect();
        word.len() > p.len() + 2
            && word.starts_with(p)
            && !cluster.is_empty()
            && !not_before.contains(cluster[0])
            && (!p.ends_with(vowel_letter) || onset(&cluster))
    });
    if let Some(&(p, weak, _)) = found {
        let mut held = 0;
        let mut last = false;
        for c in p.chars() {
            let v = vowel_letter(c);
            held += usize::from(v && !last);
            last = v;
        }
        return Some((weak, held));
    }
    // A leading `a` before one consonant or a doubled one, as in `above`
    // and `allow`.
    let chars: Vec<char> = word.chars().collect();
    let single = chars.len() > 4 && !vowel_letter(chars[1]) && vowel_letter(chars[2]);
    let double = chars.len() > 4 && chars[1] == chars[2] && !vowel_letter(chars[1]);
    if chars[0] == 'a' && (single || double) {
        let reduced = if matches!(chars[1], 't' | 'r' | 'd' | 'w' | 'g') {
            Ph::Ax
        } else {
            Ph::Ah
        };
        return Some((Weak::To(reduced), 1));
    }
    None
}

/// `phs`, the letter rules' reading of `word`, with its stress marked.
pub(super) fn mark(word: &str, phs: &[Ph]) -> Sounds {
    let vowels: Vec<usize> = (0..phs.len()).filter(|&i| phs[i].is_vowel()).collect();
    let carriers: Vec<usize> = vowels
        .iter()
        .copied()
        .filter(|&i| can_carry(phs[i]))
        .collect();
    let prefix = prefix(word);
    let by_ending = ENDINGS
        .iter()
        .find(|(end, _)| word.ends_with(end) && word.len() > end.len() + 1)
        .and_then(|&(_, held)| {
            let at = vowels.len().checked_sub(held + 1)?;
            Some(vowels[at]).filter(|&i| can_carry(phs[i]))
        });
    let after_prefix = prefix
        .and_then(|(_, held)| vowels.get(held.checked_sub(1)?))
        .and_then(|&last| carriers.iter().copied().find(|&c| c > last));
    let pick = by_ending
        .or(after_prefix)
        .or_else(|| carriers.first().copied());
    let mut out: Sounds = phs
        .iter()
        .enumerate()
        .map(|(i, &ph)| (ph, if Some(i) == pick { 5 } else { 0 }))
        .collect();
    // The prefix vowel weakens when the stress went past it.
    if let (Some((weak, _)), Some(&first)) = (prefix, vowels.first())
        && pick.is_some_and(|p| p > first)
    {
        match weak {
            Weak::To(ph) if out[first].0 != Ph::Iy => out[first].0 = ph,
            Weak::Syllabic(ph) if matches!(out.get(first + 1), Some((Ph::N | Ph::M, _))) => {
                out[first].0 = ph;
                out.remove(first + 1);
            }
            Weak::To(_) | Weak::Syllabic(_) | Weak::Same => {}
        }
    }
    out
}
