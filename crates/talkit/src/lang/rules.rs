//! Letter-to-sound rules that look at the letters around them.
//!
//! A rule reads `letters` at the current place in a word when the text
//! before them matches `left` and the text after them matches `right`, and
//! writes `sounds` in the phonetic notation. The rules are tried in order and
//! the first match wins, so a specific rule goes before a general one.
//!
//! Each context symbol matches one letter unless noted. Left contexts read
//! outward from the letters, right to left.
//!
//! | Symbol | Matches |
//! |---|---|
//! | space | the edge of the word |
//! | `#` | one or more vowels |
//! | `^` | one consonant |
//! | `:` | zero or more consonants |
//! | `*` | one or more consonants |
//! | `+` | `e`, `i` or `y` |
//! | `.` | a voiced consonant: `b d g j l m n r v w z` |
//! | `&` | a hissing sound: `s c g z x j`, or `ch` or `sh` |
//! | `@` | a consonant that makes a following `u` long: `t s r d l z n j`, or `th`, `ch` or `sh` |
//! | `%` | an ending that ends the word: `e`, `er`, `es`, `ed`, `ing` or `ely` (right side only) |
//! | other | that letter |
//!
//! A `y` after a consonant counts as a vowel, so `type` ends in a silent
//! `e` the way `tape` does.

use crate::phonetic::{Item, Ph, parse};

pub(crate) struct Rule {
    pub left: &'static str,
    pub letters: &'static str,
    pub right: &'static str,
    pub sounds: &'static str,
}

pub(crate) const fn rule(
    left: &'static str,
    letters: &'static str,
    right: &'static str,
    sounds: &'static str,
) -> Rule {
    Rule {
        left,
        letters,
        right,
        sounds,
    }
}

/// A word as the rules read it: lowercase letters with a space at each end.
struct Text(Vec<char>);

fn vowel_letter(c: char) -> bool {
    matches!(
        c,
        'a' | 'e' | 'i' | 'o' | 'u' | 'á' | 'é' | 'í' | 'ó' | 'ú' | 'ü'
    )
}

impl Text {
    fn at(&self, i: isize) -> char {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.0.get(i).copied())
            .unwrap_or(' ')
    }

    fn vowel(&self, i: isize) -> bool {
        let c = self.at(i);
        vowel_letter(c) || (c == 'y' && self.consonant(i - 1))
    }

    fn consonant(&self, i: isize) -> bool {
        let c = self.at(i);
        c.is_alphabetic() && !vowel_letter(c) && !(c == 'y' && self.consonant(i - 1))
    }

    /// Whether `two` is spelled at `i` and the letter after it.
    fn pair(&self, i: isize, two: &str) -> bool {
        let mut chars = two.chars();
        chars.next() == Some(self.at(i)) && chars.next() == Some(self.at(i + 1))
    }

    /// The letters one symbol consumes at `i`, reading in `dir` (1 or -1),
    /// or None when it does not match.
    fn symbol(&self, symbol: char, i: isize, dir: isize) -> Option<isize> {
        // The first of two letters read in this direction.
        let two = |pair: &str| {
            let start = if dir > 0 { i } else { i - 1 };
            self.pair(start, pair)
        };
        let run = |test: &dyn Fn(isize) -> bool, least: isize| {
            let mut n = 0;
            while test(i + n * dir) {
                n += 1;
            }
            (n >= least).then_some(n)
        };
        match symbol {
            ' ' => (!self.at(i).is_alphabetic()).then_some(1),
            '#' => run(&|k| self.vowel(k), 1),
            ':' => run(&|k| self.consonant(k), 0),
            '*' => run(&|k| self.consonant(k), 1),
            '^' => self.consonant(i).then_some(1),
            '+' => matches!(self.at(i), 'e' | 'i' | 'y').then_some(1),
            '.' => matches!(
                self.at(i),
                'b' | 'd' | 'g' | 'j' | 'l' | 'm' | 'n' | 'r' | 'v' | 'w' | 'z'
            )
            .then_some(1),
            '&' if two("ch") || two("sh") => Some(2),
            '&' => matches!(self.at(i), 's' | 'c' | 'g' | 'z' | 'x' | 'j').then_some(1),
            '@' if two("th") || two("ch") || two("sh") => Some(2),
            '@' => matches!(self.at(i), 't' | 's' | 'r' | 'd' | 'l' | 'z' | 'n' | 'j').then_some(1),
            '%' => ["ing", "ely", "er", "es", "ed", "e"]
                .into_iter()
                .find(|end| {
                    let n = end.len() as isize;
                    end.chars()
                        .enumerate()
                        .all(|(k, c)| self.at(i + k as isize) == c)
                        && !self.at(i + n).is_alphabetic()
                })
                .map(|end| end.len() as isize),
            c => (self.at(i) == c).then_some(1),
        }
    }

    fn left(&self, pattern: &str, before: isize) -> bool {
        let mut i = before;
        for symbol in pattern.chars().rev() {
            match self.symbol(symbol, i, -1) {
                Some(n) => i -= n,
                None => return false,
            }
        }
        true
    }

    fn right(&self, pattern: &str, after: isize) -> bool {
        let mut i = after;
        for symbol in pattern.chars() {
            match self.symbol(symbol, i, 1) {
                Some(n) => i += n,
                None => return false,
            }
        }
        true
    }

    fn spells(&self, at: isize, letters: &str) -> bool {
        letters
            .chars()
            .enumerate()
            .all(|(k, c)| self.at(at + k as isize) == c)
    }
}

/// The sounds of one lowercase word. A letter no rule reads is silent.
pub(crate) fn sounds(word: &str, rules: &[Rule]) -> Vec<Ph> {
    let text = Text(
        std::iter::once(' ')
            .chain(word.chars())
            .chain(std::iter::once(' '))
            .collect(),
    );
    let end = text.0.len() as isize - 1;
    let mut out = Vec::new();
    let mut i = 1;
    while i < end {
        let found = rules.iter().find(|r| {
            text.spells(i, r.letters)
                && text.left(r.left, i - 1)
                && text.right(r.right, i + r.letters.chars().count() as isize)
        });
        match found {
            Some(r) => {
                out.extend(sounds_of(r.sounds));
                i += r.letters.chars().count() as isize;
            }
            None => i += 1,
        }
    }
    out
}

fn sounds_of(code: &str) -> impl Iterator<Item = Ph> {
    parse(code)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| match item {
            Item::Sound { ph, .. } => Some(ph),
            _ => None,
        })
}

/// Rule tables are data: every rule must be readable.
#[cfg(test)]
pub(crate) fn check(rules: &[Rule]) {
    for r in rules {
        assert!(!r.letters.is_empty(), "empty rule");
        assert!(
            r.sounds.is_empty() || parse(r.sounds).is_ok(),
            "bad sounds {:?} for {:?}",
            r.sounds,
            r.letters
        );
    }
}
