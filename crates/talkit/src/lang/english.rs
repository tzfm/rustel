//! English text in the phonetic notation.
//!
//! A word is read in one of three ways, tried in order:
//!
//! - A word in [`words::WORDS`] has its own reading, weak inside a phrase
//!   and strong at its end.
//! - A word with no vowel letter, or a single letter, is read letter by
//!   letter, as in `html`.
//! - Any other word goes through the letter rules, and the stress rules mark
//!   its stressed vowel.
//!
//! Then a word sees its neighbours: `the` and `to` before a vowel read
//! `DHIY` and `TUW`, and a word before a colon loses its stress. Numbers
//! are read as the original engine reads them: up to three digits as a
//! count, four digits as a year, more digits one by one, and digits with
//! thousands commas as a count with a pause at each comma.

mod letters;
mod stress;
mod words;

use super::{Piece, Sounds, Token, rules};
use crate::phonetic::{Item, parse};

/// Sounds from the notation. The tables hold only valid notation.
fn sounds(code: &str) -> Sounds {
    parse(code)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| match item {
            Item::Sound { ph, stress } => Some((ph, stress)),
            _ => None,
        })
        .collect()
}

/// One or more words in the notation, and whether the first one opens a
/// clause, which the notation marks with `)`.
fn reading(code: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    for word in code.split(' ') {
        if let Some(rest) = word.strip_prefix(')') {
            out.push(Piece::Mark(')'));
            out.push(Piece::Word(sounds(rest)));
        } else {
            out.push(Piece::Word(sounds(word)));
        }
    }
    out
}

/// The letter names, as a spelled word reads them.
const LETTERS: [&str; 26] = [
    "EY5",
    "BIY5",
    "SIY4",
    "DIY4",
    "IY5",
    "EH4F",
    "JIY4",
    "EY5CH",
    "AY3",
    "JEY4",
    "KEY4",
    "EH5L",
    "EH4M",
    "EH5N",
    "OW5",
    "PIY4",
    "KYUW4",
    "AA4R",
    "EH4S",
    "TIY4",
    "YUW4",
    "VIY5",
    "DAH4BULYUW",
    "EH4KS",
    "WAY4",
    "ZIY4",
];

/// A word's reading, and its strong reading at the end of a phrase.
fn word(word: &str) -> (Vec<Piece>, Option<Vec<Piece>>) {
    if let Some((_, weak, strong)) = words::WORDS.iter().find(|(w, ..)| *w == word) {
        let strong = (!strong.is_empty()).then(|| reading(strong));
        return (reading(weak), strong);
    }
    let vowel_letter = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
    if !word.chars().any(vowel_letter) || word.chars().count() == 1 {
        let spelled = word
            .chars()
            .filter(char::is_ascii_lowercase)
            .map(|c| Piece::Word(sounds(LETTERS[usize::from(c as u8 - b'a')])))
            .collect();
        return (spelled, None);
    }
    let phs = rules::sounds(word, letters::RULES);
    (vec![Piece::Word(stress::mark(word, &phs))], None)
}

/// The number words of the original engine's count reading.
const UNITS: [&str; 10] = [
    "ZIH5ROW", "WAH5N", "TUW5", "THRIY5", "FOH5R", "FAY5V", "SIH5KS", "SEH5VIN", "EY5T", "NAY5N",
];
const TEENS: [&str; 10] = [
    "TEH5N",
    "ILEH5VIN",
    "TWEH5LV",
    "THER5TIY1N",
    "FOHR5TIY1N",
    "FIH5FTIY1N",
    "SIH5KSTIY1N",
    "SEH5VINTIY1N",
    "EY5TIY1N",
    "NAY5NTIY1N",
];
const TENS: [&str; 8] = [
    "TWEH5NIY",
    "THER5TIY",
    "FOH5RTIY",
    "FIH5FTIY",
    "SIH5KSTIY",
    "SEH5VNDIY",
    "EY5TIY",
    "NAY5NDIY",
];

/// 0 to 99 as words.
fn under_hundred(n: usize, out: &mut Vec<Sounds>) {
    match n {
        0..=9 => out.push(sounds(UNITS[n])),
        10..=19 => out.push(sounds(TEENS[n - 10])),
        _ => {
            out.push(sounds(TENS[n / 10 - 2]));
            if !n.is_multiple_of(10) {
                out.push(sounds(UNITS[n % 10]));
            }
        }
    }
}

/// 0 to 999 as a count.
fn under_thousand(n: usize, out: &mut Vec<Sounds>) {
    if n >= 100 {
        out.push(sounds(UNITS[n / 100]));
        out.push(sounds("/HUN5DRIHD"));
    }
    if !n.is_multiple_of(100) || n == 0 {
        under_hundred(n % 100, out);
    }
}

/// A number token as words and marks.
fn number(text: &str) -> Vec<Piece> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let mut out: Vec<Piece> = Vec::new();
    let mut words: Vec<Sounds> = Vec::new();
    if whole.contains(',') {
        // A count in groups of three, with a pause after each group.
        let groups: Vec<usize> = whole.split(',').filter_map(|g| g.parse().ok()).collect();
        let scales = ["", "THAW5ZIND", "MIH5LYIN", "BIH5LYIN"];
        for (k, &group) in groups.iter().enumerate() {
            let scale = groups.len() - 1 - k;
            if group == 0 {
                continue;
            }
            under_thousand(group, &mut words);
            if scale > 0 {
                words.push(sounds(scales[scale.min(3)]));
                if groups[k + 1..].iter().any(|&g| g > 0) {
                    out.extend(words.drain(..).map(Piece::Word));
                    out.push(Piece::Mark(','));
                }
            }
        }
    } else {
        let digits = whole.trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        let n: usize = digits.parse().unwrap_or(0);
        match digits.len() {
            0..=3 => under_thousand(n, &mut words),
            4 if n % 1000 < 10 => {
                words.push(sounds(UNITS[n / 1000]));
                words.push(sounds("THAW5ZIHND"));
                if !n.is_multiple_of(1000) {
                    under_hundred(n % 100, &mut words);
                }
            }
            4 => {
                under_hundred(n / 100, &mut words);
                match n % 100 {
                    0 => words.push(sounds("/HAH5NDRIHD")),
                    1..=9 => {
                        words.push(sounds("OW5"));
                        words.push(sounds(UNITS[n % 10]));
                    }
                    rest => under_hundred(rest, &mut words),
                }
                // In a year, a tens word before another word takes a
                // secondary stress.
                let last = words.len() - 1;
                for w in &mut words[..last] {
                    if TENS.iter().any(|t| sounds(t) == *w) {
                        for (_, stress) in w.iter_mut().filter(|(_, s)| *s == 5) {
                            *stress = 2;
                        }
                    }
                }
            }
            _ => words.extend(digits.bytes().map(|d| sounds(UNITS[usize::from(d - b'0')]))),
        }
    }
    if !fraction.is_empty() {
        words.push(sounds("POY5NT"));
        words.extend(
            fraction
                .bytes()
                .map(|d| sounds(UNITS[usize::from(d - b'0')])),
        );
    }
    out.extend(words.into_iter().map(Piece::Word));
    out
}

/// A sign read as a word, as the original engine reads it. A slash between
/// numbers takes a full stress.
fn sign(c: char, between_numbers: bool) -> &'static str {
    match c {
        '/' if between_numbers => "SLAE5SH",
        '/' => "SLAE4SH",
        '%' => "PERSEH5NT",
        '+' => "PLAH4S",
        '&' => "AEND",
        '*' => "STAA4R",
        '=' => "IY4KWULZ",
        '#' => "NAH4MBER",
        '@' => "AE2T",
        '-' => "DAE5SH",
        _ => "DAA5LER",
    }
}

/// A time such as `7:30`: the hour as a count, then `o'clock`, `oh` and a
/// unit, or the minutes with a secondary stress on their tens.
fn time(hours: &str, minutes: &str, out: &mut Vec<Sounds>) {
    under_thousand(hours.parse().unwrap_or(0), out);
    let m: usize = minutes.parse().unwrap_or(0);
    match m {
        0 => out.push(sounds("OWKLAA5K")),
        1..=9 => {
            out.push(sounds("OW4"));
            out.push(sounds(UNITS[m]));
        }
        _ => {
            let at = out.len();
            under_hundred(m, out);
            if m >= 20 {
                for (_, stress) in out[at].iter_mut().filter(|(_, s)| *s == 5) {
                    *stress = 2;
                }
            }
        }
    }
}

/// A number token as words and marks: a joined number digit by digit, a
/// time, or a number.
fn number_token(text: &str, joined: bool) -> Vec<Piece> {
    if joined {
        return text
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|d| Piece::Word(sounds(UNITS[usize::from(d - b'0')])))
            .collect();
    }
    if let Some((hours, minutes)) = text.split_once(':') {
        let mut words = Vec::new();
        time(hours, minutes, &mut words);
        return words.into_iter().map(Piece::Word).collect();
    }
    number(text)
}

/// Strip every stress digit.
fn unstressed(pieces: &mut [Piece]) {
    for piece in pieces {
        if let Piece::Word(sounds) = piece {
            for (_, stress) in sounds {
                *stress = 0;
            }
        }
    }
}

/// The reading of one token, without its neighbours.
fn reading_of(token: &Token) -> Vec<Piece> {
    match token {
        Token::Word(w) => word(w).0,
        Token::Number { text, joined } => number_token(text, *joined),
        _ => Vec::new(),
    }
}

/// Read `tokens` as words and marks.
pub(super) fn read(tokens: &[Token]) -> Vec<Piece> {
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let prev = i.checked_sub(1).and_then(|p| tokens.get(p));
        let next = tokens.get(i + 1);
        let is_number = |t: Option<&Token>| matches!(t, Some(Token::Number { .. }));
        match token {
            // A lone `a` beside a sign is the letter, as in `a+b`.
            Token::Word(w)
                if w == "a"
                    && (matches!(prev, Some(Token::Sign(_)))
                        || matches!(next, Some(Token::Sign(_)))) =>
            {
                out.extend(reading(LETTERS[0]));
            }
            Token::Word(w) => {
                let (weak, strong) = word(w);
                let phrase_end = matches!(next, Some(Token::Mark('.' | '?' | ',' | ':')));
                let mut pieces = match strong {
                    Some(strong) if phrase_end => strong,
                    _ => weak,
                };
                let vowel_next = next
                    .map(reading_of)
                    .unwrap_or_default()
                    .iter()
                    .find_map(|p| match p {
                        Piece::Word(s) => s.first().map(|(ph, _)| ph.is_vowel()),
                        Piece::Mark(_) => None,
                    });
                if vowel_next == Some(true) {
                    match w.as_str() {
                        "the" => pieces = reading("DHIY"),
                        "to" => pieces = reading("TUW"),
                        _ => {}
                    }
                }
                if next == Some(&Token::Mark(':')) {
                    unstressed(&mut pieces);
                }
                out.extend(pieces);
            }
            Token::Number { text, joined } => {
                out.extend(number_token(text, *joined));
                // A dollar sign is read after its amount.
                if prev == Some(&Token::Sign('$')) {
                    let one = text.trim_start_matches('0') == "1";
                    out.extend(reading(if one { "DAA5LER" } else { "DAA5LERZ" }));
                }
            }
            Token::Sign('$') if is_number(next) => {}
            Token::Sign(c) => out.extend(reading(sign(*c, is_number(prev) && is_number(next)))),
            Token::Mark(':') => out.push(Piece::Mark('.')),
            Token::Mark(m) => out.push(Piece::Mark(*m)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;

    fn say(text: &str) -> String {
        Language::English.transcribe(text)
    }

    #[test]
    fn rule_tables_parse() {
        rules::check(letters::RULES);
        for (w, weak, strong) in words::WORDS {
            for code in [weak, strong] {
                for part in code.split(' ') {
                    let part = part.trim_start_matches(')');
                    assert!(part.is_empty() || parse(part).is_ok(), "{w}: {part}");
                }
            }
        }
    }

    #[test]
    fn short_words_read_strong_at_a_phrase_end() {
        assert_eq!(say("what is it for?"), "WAXT IXZ IHT FOH4R?");
        assert_eq!(say("for the end"), "FOHR DHIY EH5ND");
    }

    #[test]
    fn reads_numbers_as_the_engine_does() {
        assert_eq!(say("42"), "FOH5RTIY TUW5");
        assert_eq!(say("1905"), "NAY5NTIY1N OW5 FAY5V");
        assert_eq!(say("2024"), "TWEH2NIY TWEH2NIY FOH5R");
        assert_eq!(say("2000"), "TUW5 THAW5ZIHND");
        assert_eq!(say("12345"), "WAH5N TUW5 THRIY5 FOH5R FAY5V");
        assert_eq!(
            say("1,234"),
            "WAH5N THAW5ZIND, TUW5 /HUN5DRIHD THER5TIY FOH5R"
        );
        assert_eq!(say("3.14"), "THRIY5 POY5NT WAH5N FOH5R");
        assert_eq!(say("7:30"), "SEH5VIN THER2TIY");
        assert_eq!(say("9:00"), "NAY5N OWKLAA5K");
        assert_eq!(say("3/4"), "THRIY5 SLAE5SH FOH5R");
        assert_eq!(say("10-20"), "WAH5N ZIH5ROW DAE5SH TUW5 ZIH5ROW");
        assert_eq!(say("$5 50%"), "FAY5V DAA5LERZ FIH5FTIY PERSEH5NT");
        assert_eq!(say("a+b"), "EY5 PLAH4S BIY5");
    }

    #[test]
    fn spells_words_without_vowels() {
        assert_eq!(say("html"), "EY5CH TIY4 EH4M EH5L");
    }
}
