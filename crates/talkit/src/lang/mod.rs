//! Text to speech sounds, one rule set per language.
//!
//! Transcription runs in three steps: tokenize the text into words and
//! punctuation, turn each word into phonemes with that language's rules, and
//! interleave the pauses punctuation asks for. Numbers are read as words
//! first, so the letter rules apply to them unchanged.

pub mod english;
pub mod spanish;

use crate::phoneme::Phoneme;

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

    /// Transcribe `text` into the sound and pause stream the utterance
    /// builder consumes.
    pub fn transcribe(self, text: &str) -> Vec<Phone> {
        let mut out = Vec::new();
        for token in tokenize(text) {
            match token {
                Token::Word(word) => {
                    if !out.is_empty() {
                        out.push(Phone::Gap);
                    }
                    out.extend(self.word_phones(&word));
                }
                Token::Comma => out.push(Phone::Comma),
                Token::Period => out.push(Phone::Period),
                Token::Question => out.push(Phone::Question),
                Token::Space => {}
            }
        }
        out
    }

    fn word_phones(self, word: &str) -> Vec<Phone> {
        if let Some(words) = number_words(self, word) {
            let mut out = Vec::new();
            for w in &words {
                out.extend(self.raw_word_phones(w));
            }
            return merge_identical(out);
        }
        merge_identical(self.raw_word_phones(word))
    }

    fn raw_word_phones(self, word: &str) -> Vec<Phone> {
        match self {
            Self::English => english::word_phones(word),
            Self::Spanish => spanish::word_phones(word),
        }
    }
}

/// One entry of a transcription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phone {
    /// A speech sound.
    Sound(Phoneme),
    /// A speech sound the language's rules stressed. The utterance builder
    /// stretches it a little.
    Stressed(Phoneme),
    /// Short inter-word gap.
    Gap,
    /// Comma or semicolon: mid-length pause.
    Comma,
    /// Period or exclamation: long pause, falling pitch terminal.
    Period,
    /// Question mark: long pause, rising pitch terminal.
    Question,
}

/// One letter-to-sound rule. The pattern holds lowercase letters, with `#`
/// matching a word boundary at either end of the pattern without consuming a
/// letter.
pub(crate) struct Rule {
    pub pattern: &'static str,
    pub phones: &'static [Phoneme],
}

/// Apply the rules to one lowercase word. At each letter position the longest
/// matching pattern wins, so specific digraph and suffix rules outrank single
/// letters without any hand ordering. A letter no rule matches is skipped.
///
/// Each produced sound is returned with the letter span of the rule that made
/// it, in `(start, letters)`, so stress marking can find which vowel a written
/// accent belongs to.
pub(crate) fn apply_rules(word: &[char], rules: &[Rule]) -> Vec<(Phoneme, usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < word.len() {
        let mut best: Option<(usize, &Rule)> = None;
        for rule in rules {
            let letters = match_pattern(rule.pattern, word, pos);
            if letters == 0 {
                continue;
            }
            let longer = best.is_none_or(|(len, _)| letters > len);
            if longer {
                best = Some((letters, rule));
            }
        }
        match best {
            Some((letters, rule)) => {
                for ph in rule.phones {
                    out.push((*ph, pos, letters));
                }
                pos += letters;
            }
            None => pos += 1,
        }
    }
    out
}

/// The letters `pattern` consumes when it matches `word` at `pos`, or 0 when
/// it does not match. `#` at the start requires `pos` to be the word start
/// and `#` at the end requires the pattern to end with the word.
fn match_pattern(pattern: &str, word: &[char], pos: usize) -> usize {
    let chars: Vec<char> = pattern.chars().collect();
    let mut letters = 0;
    for (i, pc) in chars.iter().enumerate() {
        if *pc == '#' {
            let boundary = if i == 0 {
                pos == 0
            } else {
                pos + letters == word.len()
            };
            if !boundary {
                return 0;
            }
        } else {
            if word.get(pos + letters) != Some(pc) {
                return 0;
            }
            letters += 1;
        }
    }
    // A bare `#` pattern would match everywhere and consume nothing.
    letters
}

/// Collapse a doubled consonant, as both languages write one sound that way.
/// The Spanish trill is its own sound, so `rr` survives its own rule.
fn merge_identical(phones: Vec<Phone>) -> Vec<Phone> {
    let mut out: Vec<Phone> = Vec::with_capacity(phones.len());
    for p in phones {
        if let (Some(last), Phone::Sound(a) | Phone::Stressed(a)) = (out.last(), p)
            && matches!(last, Phone::Sound(b) | Phone::Stressed(b) if a == *b)
        {
            continue;
        }
        out.push(p);
    }
    out
}

enum Token {
    Word(String),
    Comma,
    Period,
    Question,
    /// Flush marker for separators that add no pause of their own.
    Space,
}

/// Split text into words and pauses. Every character that is not a letter,
/// digit, apostrophe or accent is dropped; punctuation sets the pause kind.
/// A number keeps its sign, thousands commas and decimal point, so `-1,500.25`
/// reaches [`number_words`] whole.
fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut word = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (at, &c) in chars.iter().enumerate() {
        let digit_follows = chars.get(at + 1).is_some_and(char::is_ascii_digit);
        let in_number = digit_follows
            && match c {
                '-' => word.is_empty(),
                ',' => word.ends_with(|d: char| d.is_ascii_digit()) && !word.contains('.'),
                '.' => word.ends_with(|d: char| d.is_ascii_digit()) && !word.contains('.'),
                _ => false,
            };
        if in_number {
            word.push(c);
            continue;
        }
        match c {
            ',' | ';' | ':' => flush_word(&mut out, &mut word, Token::Comma),
            '.' | '!' => flush_word(&mut out, &mut word, Token::Period),
            '?' | '¿' | '¡' => flush_word(&mut out, &mut word, Token::Question),
            c if c.is_alphanumeric() || c == '\'' || c == '\u{0301}' => word.push(c),
            _ => flush_word(&mut out, &mut word, Token::Space),
        }
    }
    flush_word(&mut out, &mut word, Token::Space);
    out
}

fn flush_word(out: &mut Vec<Token>, word: &mut String, after: Token) {
    if !word.is_empty() {
        let mut w = std::mem::take(word);
        // Combining accents from normalized input never reach the rules.
        while w.ends_with('\'') {
            w.pop();
        }
        out.push(Token::Word(w));
    }
    if !matches!(after, Token::Space) {
        out.push(after);
    }
}

/// Read `word` as a number and return the words it is spoken as, or None when
/// it is not a number. Handles an optional minus sign, thousands commas and
/// one decimal point. Integers below one million become full words; anything
/// larger falls back to digit by digit.
fn number_words(lang: Language, word: &str) -> Option<Vec<String>> {
    let cleaned: String = word.chars().filter(|c| *c != ',').collect();
    let (negative, body) = match cleaned.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, cleaned.as_str()),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) if !i.is_empty() && !f.is_empty() => (i, Some(f)),
        _ if body.chars().all(|c| c.is_ascii_digit()) && !body.is_empty() => (body, None),
        _ => return None,
    };
    let digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    if !digits(int_part) || !frac_part.is_none_or(digits) {
        return None;
    }

    let minus = match lang {
        Language::English => "minus",
        Language::Spanish => "menos",
    };
    let point = match lang {
        Language::English => "point",
        Language::Spanish => "coma",
    };
    let digit_names = |part: &str| -> Vec<&'static str> {
        let names = match lang {
            Language::English => &english::DIGITS,
            Language::Spanish => &spanish::DIGITS,
        };
        part.bytes().map(|d| names[usize::from(d - b'0')]).collect()
    };
    let mut words: Vec<String> = Vec::new();
    if negative {
        words.push(minus.into());
    }
    let spoken = match int_part.parse::<i64>() {
        Ok(value) => match lang {
            Language::English => english::number_words(value),
            Language::Spanish => spanish::number_words(value),
        },
        // Too long for a number: the digits one by one.
        Err(_) => digit_names(int_part),
    };
    words.extend(spoken.into_iter().map(str::to_string));
    if let Some(frac) = frac_part {
        words.push(point.into());
        words.extend(digit_names(frac).into_iter().map(str::to_string));
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transcription without the short gaps between words, which a number
    /// spoken as several words does not carry.
    fn spoken(language: Language, text: &str) -> Vec<Phone> {
        let mut phones = language.transcribe(text);
        phones.retain(|phone| *phone != Phone::Gap);
        phones
    }

    #[test]
    fn numbers_keep_their_sign_commas_and_point() {
        let english = Language::English;
        for (written, words) in [
            ("3.5", "three point five"),
            ("1,500", "one thousand five hundred"),
            ("-2", "minus two"),
            (
                "10101010101010101010",
                "one zero one zero one zero one zero one zero one zero one zero one zero one zero \
                 one zero",
            ),
            // A point that ends a sentence stays a full stop, and a second
            // point in one number starts the next word.
            ("2. 3", "two. three"),
            ("1.5.3", "one point five. three"),
        ] {
            assert_eq!(
                spoken(english, written),
                spoken(english, words),
                "{written}"
            );
        }
        assert_eq!(
            spoken(Language::Spanish, "3.5"),
            spoken(Language::Spanish, "tres coma cinco")
        );
    }
}
