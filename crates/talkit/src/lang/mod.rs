//! Text to the phonetic notation, one rule set per language.
//!
//! Transcription runs in three steps: split the text into words, numbers and
//! punctuation, read each word with the language's rules, and join the
//! readings with the marks that set the pauses. The result is a phonetic
//! string such as `/HEH5LOW WER5LD.`, which the narrator speaks.

mod english;
mod rules;
mod spanish;

use crate::phonetic::Ph;

/// A language the synthesizer speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Language {
    #[default]
    English,
    Spanish,
}

impl Language {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "english" | "en" | "ing" => Some(Self::English),
            "spanish" | "es" | "esp" | "espanol" | "español" => Some(Self::Spanish),
            _ => None,
        }
    }

    /// The name shown in help and errors.
    pub fn name(self) -> &'static str {
        match self {
            Self::English => "english",
            Self::Spanish => "spanish",
        }
    }

    /// `text` in the phonetic notation.
    pub fn transcribe(self, text: &str) -> String {
        let tokens = tokenize(text);
        let pieces = match self {
            Self::English => english::read(&tokens),
            Self::Spanish => spanish::read(&tokens),
        };
        write(&pieces)
    }
}

/// A piece of text as the readers see it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// Letters and apostrophes, lowercase.
    Word(String),
    /// Digits as written, with a point, thousands commas and a time's colon
    /// kept. `joined` is true when a slash or a dash ties the number to the
    /// one next to it, as in a date: such a number is read digit by digit.
    Number { text: String, joined: bool },
    /// A sign read as a word: `/ % + & * = # $ @`, and a dash between digits.
    Sign(char),
    /// A pause or a break: `,` `.` `?` `:` or `-`.
    Mark(char),
}

/// Split text into words, numbers, signs and marks. Every other character
/// separates words. A comma or point between digits stays in the number,
/// so `1,500.25` is one token, and so does the colon of a time like `7:30`.
fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let digit_at = |k: usize| chars.get(k).is_some_and(char::is_ascii_digit);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let start = i;
            while digit_at(i)
                || (matches!(chars.get(i), Some(',' | '.')) && digit_at(i + 1) && digit_at(i - 1))
                || (chars.get(i) == Some(&':')
                    && digit_at(i - 1)
                    && digit_at(i + 1)
                    && digit_at(i + 2)
                    && !digit_at(i + 3))
            {
                i += 1;
            }
            let tied = |k: Option<usize>| {
                k.and_then(|k| chars.get(k))
                    .is_some_and(|&c| matches!(c, '/' | '-'))
            };
            let joined = tied(start.checked_sub(1)) || tied(Some(i));
            out.push(Token::Number {
                text: chars[start..i].iter().collect(),
                joined,
            });
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphabetic() || chars[i] == '\'' || chars[i] == '\u{2019}')
            {
                i += 1;
            }
            let word: String = chars[start..i]
                .iter()
                .map(|&c| if c == '\u{2019}' { '\'' } else { c })
                .collect::<String>()
                .trim_end_matches('\'')
                .to_lowercase();
            out.push(Token::Word(word));
            continue;
        }
        let between_digits = i > 0 && digit_at(i - 1) && digit_at(i + 1);
        let token = match c {
            '/' | '%' | '+' | '&' | '*' | '=' | '#' | '$' | '@' => Some(Token::Sign(c)),
            '-' if between_digits => Some(Token::Sign('-')),
            ',' | '(' | ')' => Some(Token::Mark(',')),
            // A point between letters belongs to an abbreviation, as in `e.g.`.
            '.' if chars.get(i + 1).is_some_and(|n| n.is_alphabetic()) => None,
            '.' | '!' | ';' => Some(Token::Mark('.')),
            '?' | '¿' => Some(Token::Mark('?')),
            ':' => Some(Token::Mark(':')),
            // A dash between spaces, or before a number, is a break. A hyphen
            // inside a word only splits it.
            '-' | '\u{2013}' | '\u{2014}'
                if !chars.get(i + 1).is_some_and(|n| n.is_alphabetic())
                    || !i
                        .checked_sub(1)
                        .and_then(|p| chars.get(p))
                        .is_some_and(|n| n.is_alphanumeric()) =>
            {
                Some(Token::Mark('-'))
            }
            _ => None,
        };
        match token {
            // Repeated marks are one pause.
            Some(Token::Mark(m)) if out.last() == Some(&Token::Mark(m)) => {}
            Some(token) => out.push(token),
            None => {}
        }
        i += 1;
    }
    // Opening marks do not pause before anything.
    while matches!(out.first(), Some(Token::Mark(_))) {
        out.remove(0);
    }
    out
}

/// A word's sounds, each with its stress digit, 0 when unstressed.
type Sounds = Vec<(Ph, u8)>;

/// A reading: words and marks.
enum Piece {
    Word(Sounds),
    Mark(char),
}

/// The notation of a reading: words apart, a mark right after its word.
///
/// A one-letter code followed by `HH` would read back as a two-letter code
/// such as `DH`, so there the aspiration is written `/H`, its other code.
fn write(pieces: &[Piece]) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Word(sounds) if !sounds.is_empty() => {
                if !out.is_empty() {
                    out.push(' ');
                }
                let mut last = "";
                for &(ph, stress) in sounds {
                    let ph = if ph == Ph::Hh && last.len() == 1 {
                        Ph::H
                    } else {
                        ph
                    };
                    last = ph.code();
                    out.push_str(last);
                    if stress > 0 {
                        out.push(char::from(b'0' + stress));
                    }
                }
            }
            Piece::Word(_) => {}
            Piece::Mark(mark) => out.push(*mark),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The narrator reads every transcription back: a reading the notation
    /// cannot parse would render as silence.
    #[test]
    fn every_reading_parses() {
        let text = "Adjunto el mensaje, ¿vale? Hello-world: it's 1,500.25 dollars (or 2024)! \n                    html cfg a b c dejar ojo.";
        for language in [Language::English, Language::Spanish] {
            let notation = language.transcribe(text);
            assert!(crate::phonetic::parse(&notation).is_ok(), "{notation}");
        }
    }

    #[test]
    fn splits_words_numbers_and_marks() {
        assert_eq!(
            tokenize("(Hi), it's 1,500.25 - ok: done!? well-known 7:30 3/4"),
            vec![
                Token::Word("hi".into()),
                Token::Mark(','),
                Token::Word("it's".into()),
                Token::Number {
                    text: "1,500.25".into(),
                    joined: false
                },
                Token::Mark('-'),
                Token::Word("ok".into()),
                Token::Mark(':'),
                Token::Word("done".into()),
                Token::Mark('.'),
                Token::Mark('?'),
                Token::Word("well".into()),
                Token::Word("known".into()),
                Token::Number {
                    text: "7:30".into(),
                    joined: false
                },
                Token::Number {
                    text: "3".into(),
                    joined: true
                },
                Token::Sign('/'),
                Token::Number {
                    text: "4".into(),
                    joined: true
                },
            ]
        );
    }
}
