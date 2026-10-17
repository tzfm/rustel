//! Spanish letter-to-sound rules.
//!
//! Spanish spelling maps to sounds almost one to one, so the rule table is
//! small. Stress is regular: a written accent wins, else a word ending in a
//! vowel, n or s stresses the second-to-last syllable and any other ending
//! stresses the last one. The stressed vowel is handed to the utterance
//! builder, which stretches it a little.

use crate::lang::{Phone, Rule, apply_rules};
use crate::phoneme::Phoneme as P;

/// Whole words the rules would get wrong.
const EXCEPTIONS: &[(&str, &[P])] = &[("y", &[P::Iy])];

const RULES: &[Rule] = &[
    (Rule {
        pattern: "ch",
        phones: &[P::T, P::Sh],
    }),
    (Rule {
        pattern: "ll",
        phones: &[P::Y],
    }),
    (Rule {
        pattern: "rr",
        phones: &[P::Rr],
    }),
    (Rule {
        pattern: "cc",
        phones: &[P::K, P::S],
    }),
    (Rule {
        pattern: "gue",
        phones: &[P::G],
    }),
    (Rule {
        pattern: "gui",
        phones: &[P::G],
    }),
    (Rule {
        pattern: "qu",
        phones: &[P::K],
    }),
    (Rule {
        pattern: "ge",
        phones: &[P::X],
    }),
    (Rule {
        pattern: "gi",
        phones: &[P::X],
    }),
    (Rule {
        pattern: "#r",
        phones: &[P::Rr],
    }),
    (Rule {
        pattern: "nr",
        phones: &[P::N, P::Rr],
    }),
    (Rule {
        pattern: "lr",
        phones: &[P::L, P::Rr],
    }),
    (Rule {
        pattern: "sr",
        phones: &[P::S, P::Rr],
    }),
    (Rule {
        pattern: "ce",
        phones: &[P::S, P::Eh],
    }),
    (Rule {
        pattern: "ci",
        phones: &[P::S, P::Ih],
    }),
    (Rule {
        pattern: "c",
        phones: &[P::K],
    }),
    (Rule {
        pattern: "g",
        phones: &[P::G],
    }),
    (Rule {
        pattern: "j",
        phones: &[P::X],
    }),
    (Rule {
        pattern: "z",
        phones: &[P::S],
    }),
    (Rule {
        pattern: "h",
        phones: &[],
    }),
    (Rule {
        pattern: "ñ",
        phones: &[P::N, P::Y],
    }),
    (Rule {
        pattern: "x",
        phones: &[P::K, P::S],
    }),
    (Rule {
        pattern: "y#",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "v",
        phones: &[P::B],
    }),
    (Rule {
        pattern: "w",
        phones: &[P::W],
    }),
    (Rule {
        pattern: "k",
        phones: &[P::K],
    }),
    (Rule {
        pattern: "a",
        phones: &[P::Aa],
    }),
    (Rule {
        pattern: "e",
        phones: &[P::Eh],
    }),
    (Rule {
        pattern: "i",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "o",
        phones: &[P::Ao],
    }),
    (Rule {
        pattern: "u",
        phones: &[P::Uw],
    }),
    (Rule {
        pattern: "b",
        phones: &[P::B],
    }),
    (Rule {
        pattern: "d",
        phones: &[P::D],
    }),
    (Rule {
        pattern: "f",
        phones: &[P::F],
    }),
    (Rule {
        pattern: "l",
        phones: &[P::L],
    }),
    (Rule {
        pattern: "m",
        phones: &[P::M],
    }),
    (Rule {
        pattern: "n",
        phones: &[P::N],
    }),
    (Rule {
        pattern: "p",
        phones: &[P::P],
    }),
    (Rule {
        pattern: "r",
        phones: &[P::R],
    }),
    (Rule {
        pattern: "s",
        phones: &[P::S],
    }),
    (Rule {
        pattern: "t",
        phones: &[P::T],
    }),
];

/// One lowercase word to phonemes, with the stressed vowel marked.
pub(super) fn word_phones(word: &str) -> Vec<Phone> {
    let (letters, accent) = normalize(word);
    if let Some((_, phones)) = EXCEPTIONS.iter().find(|(w, _)| *w == letters) {
        return phones.iter().map(|p| Phone::Sound(*p)).collect();
    }
    let stress = accent.or_else(|| default_stress(&letters));
    let chars: Vec<char> = letters.chars().collect();
    apply_rules(&chars, RULES)
        .into_iter()
        .map(|(p, start, len)| {
            let is_stressed = stress.is_some_and(|s| start <= s && s < start + len);
            if is_stressed && is_vowel(p) {
                Phone::Stressed(p)
            } else {
                Phone::Sound(p)
            }
        })
        .collect()
}

fn is_vowel(p: P) -> bool {
    matches!(p, P::Aa | P::Eh | P::Iy | P::Ao | P::Uw)
}

/// Lowercase, accents folded. Returns the letters and the letter index a
/// written accent sat on, if the word carries one.
fn normalize(word: &str) -> (String, Option<usize>) {
    let mut out = String::with_capacity(word.len());
    let mut accent = None;
    for c in word.to_lowercase().chars() {
        if c == '\'' || c == '\u{0301}' {
            continue;
        }
        let folded = match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            c => c,
        };
        if folded != c {
            accent = Some(out.chars().count());
        }
        out.push(folded);
    }
    (out, accent)
}

/// The letter index the default stress rule puts the beat on: the first vowel
/// of the chosen syllable.
fn default_stress(letters: &str) -> Option<usize> {
    let chars: Vec<char> = letters.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let groups: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(_, c)| "aeiou".contains(**c))
        .map(|(i, _)| i)
        .collect();
    if groups.is_empty() {
        return None;
    }
    // Split the vowel indices into runs; each run is one syllable nucleus.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (n, idx) in groups.iter().enumerate() {
        let contiguous = n > 0 && *idx == groups[n - 1] + 1;
        if contiguous {
            runs.last_mut().expect("first run exists").1 = *idx;
        } else {
            runs.push((*idx, *idx));
        }
    }
    let penult = matches!(chars.last(), Some('a' | 'e' | 'i' | 'o' | 'u' | 'n' | 's'));
    let pick = if penult && runs.len() >= 2 {
        runs[runs.len() - 2]
    } else {
        runs[runs.len() - 1]
    };
    Some(pick.0)
}

pub(super) const DIGITS: [&str; 10] = [
    "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve",
];

/// Words for 1 to 29, which Spanish names individually.
const UNDER30: [&str; 29] = [
    "uno",
    "dos",
    "tres",
    "cuatro",
    "cinco",
    "seis",
    "siete",
    "ocho",
    "nueve",
    "diez",
    "once",
    "doce",
    "trece",
    "catorce",
    "quince",
    "dieciseis",
    "diecisiete",
    "dieciocho",
    "diecinueve",
    "veinte",
    "veintiuno",
    "veintidos",
    "veintitres",
    "veinticuatro",
    "veinticinco",
    "veintiseis",
    "veintisiete",
    "veintiocho",
    "veintinueve",
];

const TENS: [&str; 7] = [
    "treinta",
    "cuarenta",
    "cincuenta",
    "sesenta",
    "setenta",
    "ochenta",
    "noventa",
];

const HUNDREDS: [&str; 9] = [
    "ciento",
    "doscientos",
    "trescientos",
    "cuatrocientos",
    "quinientos",
    "seiscientos",
    "setecientos",
    "ochocientos",
    "novecientos",
];

/// Words for `n`. Above 999,999 the digits are named one by one.
pub(super) fn number_words(n: i64) -> Vec<&'static str> {
    if n < 0 {
        return number_words(-n);
    }
    if n > 999_999 {
        return n
            .to_string()
            .chars()
            .map(|c| DIGITS[(c as u8 - b'0') as usize])
            .collect();
    }
    let mut out = Vec::new();
    if n >= 1000 {
        let thousands = n / 1000;
        if thousands > 1 {
            under_thousand(&mut out, thousands);
        }
        out.push("mil");
    }
    let rest = n % 1000;
    if rest == 0 && !out.is_empty() {
        return out;
    }
    if n == 0 {
        out.push("cero");
        return out;
    }
    under_thousand(&mut out, rest);
    out
}

fn under_thousand(out: &mut Vec<&'static str>, n: i64) {
    if n == 100 {
        out.push("cien");
        return;
    }
    if n > 100 {
        out.push(HUNDREDS[(n / 100 - 1) as usize]);
    }
    let rest = n % 100;
    if rest == 0 {
        return;
    }
    if rest < 30 {
        out.push(UNDER30[(rest - 1) as usize]);
    } else {
        out.push(TENS[(rest / 10 - 3) as usize]);
        if rest % 10 > 0 {
            out.push("y");
            out.push(DIGITS[(rest % 10) as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phones(word: &str) -> Vec<P> {
        word_phones(word)
            .into_iter()
            .filter_map(|p| match p {
                Phone::Sound(p) | Phone::Stressed(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    fn stressed(word: &str) -> Vec<bool> {
        word_phones(word)
            .into_iter()
            .map(|p| matches!(p, Phone::Stressed(_)))
            .collect()
    }

    #[test]
    fn reads_the_letters() {
        assert_eq!(phones("hola"), vec![P::Ao, P::L, P::Aa]);
        assert_eq!(phones("señor"), vec![P::S, P::Eh, P::N, P::Y, P::Ao, P::R]);
        assert_eq!(phones("perro"), vec![P::P, P::Eh, P::Rr, P::Ao]);
        assert_eq!(phones("jugo"), vec![P::X, P::Uw, P::G, P::Ao]);
        assert_eq!(phones("y"), vec![P::Iy]);
    }

    #[test]
    fn marks_stress_by_rule_and_accent() {
        // "canto" ends in a vowel, so the penult vowel "a" carries the beat.
        assert_eq!(stressed("canto"), vec![false, true, false, false, false]);
        // "cantar" ends in a consonant, so the last vowel does.
        assert_eq!(
            stressed("cantar"),
            vec![false, false, false, false, true, false]
        );
        // A written accent overrides the default.
        assert_eq!(stressed("cantó"), vec![false, false, false, false, true]);
    }

    #[test]
    fn reads_numbers_as_words() {
        assert_eq!(number_words(0), vec!["cero"]);
        assert_eq!(number_words(31), vec!["treinta", "y", "uno"]);
        assert_eq!(number_words(100), vec!["cien"]);
        assert_eq!(number_words(121), vec!["ciento", "veintiuno"]);
        assert_eq!(number_words(2000), vec!["dos", "mil"]);
    }
}
