//! Spanish text in the phonetic notation.
//!
//! Spanish spelling maps to sounds almost one to one, so the rule table is
//! small. Stress is regular: a written accent wins, else a word ending in a
//! vowel, `n` or `s` stresses the second-to-last syllable and any other
//! ending stresses the last one.

use super::rules::{Rule, rule, sounds};
use super::{Piece, Sounds, Token};
use crate::phonetic::Ph;

#[rustfmt::skip]
const RULES: &[Rule] = &[
    rule("", "ch", "", "CH"),
    rule("", "ll", "", "Y"),
    rule("", "rr", "", "RR"),
    rule("", "cc", "", "KS"),
    rule("", "gü", "", "GUW"),
    rule("", "gu", "+", "G"),
    rule("", "qu", "", "K"),
    rule("", "g", "+", "HH"),
    rule("n", "r", "", "RR"),
    rule("l", "r", "", "RR"),
    rule("s", "r", "", "RR"),
    rule("", "c", "+", "S"),
    rule("", "c", "", "K"),
    rule("", "g", "", "G"),
    rule("", "j", "", "HH"),
    rule("", "z", "", "S"),
    rule(" ", "hu", "e", "HHW"),
    rule("", "h", "", ""),
    rule("", "ñ", "", "NY"),
    rule("", "x", "", "KS"),
    rule("", "y", " ", "IY"),
    rule("", "y", "", "Y"),
    rule("", "v", "", "V"),
    rule("", "w", "", "W"),
    rule("", "k", "", "K"),
    rule("", "ai", "", "AY"),
    rule("", "ay", "", "AY"),
    rule("", "ei", "", "EY"),
    rule("", "ey", " ", "EHY"),
    rule("", "oi", "", "OY"),
    rule("", "oy", "", "OY"),
    rule("g", "u", "a", "UW"),
    rule("g", "u", "o", "UW"),
    rule("n", "u", "#", "UW"),
    rule("s", "u", "#", "UW"),
    rule("", "u", "#", "W"),
    rule("", "a", "", "AA"),
    rule("", "e", "", "EH"),
    rule("", "i", "", "IY"),
    rule("", "o", "", "OH"),
    rule("", "u", "", "UW"),
    rule("", "ü", "", "UW"),
    rule("", "b", "", "B"),
    rule("", "d", "", "D"),
    rule("", "f", "", "F"),
    rule("", "l", "", "L"),
    rule("", "m", "", "M"),
    rule("", "n", "g", "NX"),
    rule("", "n", "", "N"),
    rule("", "p", "", "P"),
    rule("", "r", "", "R"),
    rule("", "s", "", "S"),
    rule("", "t", "", "T"),
];

/// Lowercase, accents folded. Returns the letters and the letter index a
/// written accent sat on, if the word carries one.
fn normalize(word: &str) -> (String, Option<usize>) {
    let mut out = String::with_capacity(word.len());
    let mut accent = None;
    for c in word.chars().filter(|&c| c != '\'') {
        let folded = match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' => 'u',
            c => c,
        };
        if folded != c {
            accent = Some(out.chars().count());
        }
        out.push(folded);
    }
    (out, accent)
}

/// The syllables of a word by its vowel letters: each run of vowels is one
/// syllable. The `u` of `que` and `gui` is silent and counts as no vowel.
fn syllables(chars: &[char]) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for i in 0..chars.len() {
        let silent_u = chars[i] == 'u'
            && i > 0
            && matches!(chars[i - 1], 'q' | 'g')
            && matches!(chars.get(i + 1), Some('e' | 'i'));
        if !matches!(chars[i], 'a' | 'e' | 'i' | 'o' | 'u' | 'ü') || silent_u {
            continue;
        }
        // The `u` of `agua` is a syllable of its own.
        let hiatus =
            matches!(chars[i], 'a' | 'o') && i >= 2 && chars[i - 1] == 'u' && chars[i - 2] == 'g';
        match runs.last_mut() {
            Some(run) if run.last().copied() == i.checked_sub(1) && !hiatus => run.push(i),
            _ => runs.push(vec![i]),
        }
    }
    runs
}

/// The syllable that takes the beat, and the place of the beat in it when
/// a written accent marks it. Without an accent, a word ending in a vowel,
/// `n` or `s` stresses its second-to-last syllable and any other its last.
fn beat(
    chars: &[char],
    runs: &[Vec<usize>],
    accent: Option<usize>,
) -> Option<(usize, Option<usize>)> {
    if let Some(a) = accent {
        return runs
            .iter()
            .enumerate()
            .find_map(|(r, run)| run.iter().position(|&i| i == a).map(|k| (r, Some(k))));
    }
    let penult = matches!(chars.last(), Some('a' | 'e' | 'i' | 'o' | 'u' | 'n' | 's'));
    let r = match runs.len() {
        0 => return None,
        n if penult && n >= 2 => n - 2,
        n => n - 1,
    };
    // In the run, the open vowel takes the beat, else the last one.
    let run = &runs[r];
    let k = run
        .iter()
        .position(|&i| matches!(chars[i], 'a' | 'e' | 'o'))
        .unwrap_or(run.len() - 1);
    Some((r, Some(k)))
}

/// Short words that take no stress, as the original engine reads them.
const UNSTRESSED: &[&str] = &[
    "a", "al", "con", "de", "del", "el", "en", "es", "hay", "la", "las", "lo", "los", "me", "mi",
    "mis", "ni", "no", "nos", "o", "por", "que", "se", "sin", "su", "sus", "te", "tu", "tus", "un",
    "y", "ya", "yo",
];

/// One word's sounds, with the stressed vowel marked. In the stressed
/// syllable the accented vowel takes the beat, else its open vowel `a`,
/// `e` or `o`, else its last vowel.
fn word(word: &str) -> Sounds {
    let (letters, accent) = normalize(word);
    let chars: Vec<char> = letters.chars().collect();
    let beat = beat(&chars, &syllables(&chars), accent);
    let mut out: Sounds = sounds(&letters, RULES)
        .into_iter()
        .map(|ph| (ph, 0))
        .collect();
    // The vowel sounds in groups, one group per syllable.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, (ph, _)) in out.iter().enumerate() {
        if !ph.is_vowel() {
            continue;
        }
        match groups.last_mut() {
            Some(g) if g.last().copied() == i.checked_sub(1) => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    let open = |ph: Ph| matches!(ph, Ph::Aa | Ph::Eh | Ph::Oh | Ph::Ay | Ph::Ey | Ph::Oy);
    if let Some((r, at)) = beat
        && let Some(group) = groups.get(r)
        && !UNSTRESSED.contains(&letters.as_str())
    {
        let pick = at
            .and_then(|k| group.get(k).copied())
            .or_else(|| group.iter().copied().find(|&i| open(out[i].0)))
            .or(group.last().copied());
        if let Some(i) = pick {
            out[i].1 = 5;
        }
    }
    out
}
/// The number words of the original engine's Spanish count reading.
const UNITS: [&str; 10] = [
    "SEH5ROH",
    "UW5NOH",
    "DOH5S",
    "TREY5S",
    "KWAA5TROH",
    "SIY5NXKOH",
    "SEY5S",
    "SIYEH5TEH",
    "OH5CHOH",
    "NUWEY5VEH",
];
const TEENS: [&str; 6] = [
    "DIYEH5S",
    "OH5NSEH",
    "DOH5SEH",
    "TREH5SEH",
    "KAATOH5RSEH",
    "KIY5NSEH",
];
/// Twenty to ninety alone, and before `y` and a unit.
const TENS: [(&str, &str); 8] = [
    ("VEY5NTEY", "VEY5NTEHIY"),
    ("TREY5NTAA", "TREY5NTAAIY"),
    ("KWAAREH5NTAA", "KWAAREH5NTAAIY"),
    ("SIYNXKWEH5NTAA", "SIYNXKWEH5NTAAIY"),
    ("SEHSEH5NTAA", "SEHSEH5NTAAIY"),
    ("SEHTEH5NTAA", "SEHTEH5NTAAIY"),
    ("OHCHEH5NTAA", "OHCHEH5NTAAIY"),
    ("NOHVEH5NTAA", "NOHVEH5NTAAIY"),
];

/// 0 to 99 as words.
fn under_hundred(n: usize, out: &mut Vec<&'static str>) {
    match n {
        0..=9 => out.push(UNITS[n]),
        10..=15 => out.push(TEENS[n - 10]),
        16..=19 => out.extend(["DIYEHSIY", UNITS[n - 10]]),
        _ if n.is_multiple_of(10) => out.push(TENS[n / 10 - 2].0),
        _ => out.extend([TENS[n / 10 - 2].1, UNITS[n % 10]]),
    }
}

/// 1 to 999 as words.
fn under_thousand(n: usize, out: &mut Vec<&'static str>) {
    match n / 100 {
        0 => {}
        1 if n == 100 => out.push("SIYEH4N"),
        1 => out.push("SIYEH4NTOH"),
        5 => out.push("KIYNIYEH4NTOHS"),
        7 => out.push("SEH3TEHSIYEH4NTOHS"),
        9 => out.push("NOH3VEHSIYEH4NTOHS"),
        h => out.extend([UNITS[h], "SIYEH4NTOHS"]),
    }
    if !n.is_multiple_of(100) {
        under_hundred(n % 100, out);
    }
}

/// 0 to 999,999 as a count. A thousand reads `mil`, without `uno`.
fn count(n: usize, out: &mut Vec<&'static str>) {
    match n {
        0 => out.push(UNITS[0]),
        // A lone one has its own stress.
        1 => out.push("UW4NOH"),
        _ => {
            if n >= 1000 {
                if n / 1000 > 1 {
                    under_thousand(n / 1000, out);
                }
                out.push("MIY5L");
            }
            under_thousand(n % 1000, out);
        }
    }
}

/// A number token as words and marks: up to four digits as a count, more
/// digits one by one, thousands commas as a count with a pause at each
/// comma, and a point as `y`, the digits one by one and `decimal`.
fn number(text: &str) -> Vec<Piece> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digit = |d: u8| UNITS[usize::from(d - b'0')];
    let mut out: Vec<Piece> = Vec::new();
    let mut words: Vec<&'static str> = Vec::new();
    if whole.contains(',') {
        let groups: Vec<usize> = whole.split(',').filter_map(|g| g.parse().ok()).collect();
        for (k, &group) in groups.iter().enumerate() {
            let scale = groups.len() - 1 - k;
            if group == 0 {
                continue;
            }
            match scale {
                0 => under_thousand(group, &mut words),
                1 => count(group * 1000, &mut words),
                _ => {
                    if group > 1 {
                        under_thousand(group, &mut words);
                    }
                    words.push(if group > 1 { "MIYYOH5NEHS" } else { "MIYYOH5N" });
                }
            }
            if scale > 0 && groups[k + 1..].iter().any(|&g| g > 0) {
                out.extend(words.drain(..).map(|w| Piece::Word(sounds_of(w))));
                out.push(Piece::Mark(','));
            }
        }
    } else {
        let digits = whole.trim_start_matches('0');
        match digits.len() {
            0 => words.push(UNITS[0]),
            1..=4 => count(digits.parse().unwrap_or(0), &mut words),
            _ => words.extend(digits.bytes().map(digit)),
        }
    }
    if !fraction.is_empty() {
        words.push("IY");
        words.extend(fraction.bytes().map(digit));
        words.push("DEH5SIYMAAL");
    }
    out.extend(words.into_iter().map(|w| Piece::Word(sounds_of(w))));
    out
}

/// Sounds from the notation. The tables hold only valid notation.
fn sounds_of(code: &str) -> Sounds {
    crate::phonetic::parse(code)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| match item {
            crate::phonetic::Item::Sound { ph, stress } => Some((ph, stress)),
            _ => None,
        })
        .collect()
}

/// Read `tokens` as words and marks.
pub(super) fn read(tokens: &[Token]) -> Vec<Piece> {
    let mut out = Vec::new();
    for token in tokens {
        match token {
            Token::Word(w) => out.push(Piece::Word(word(w))),
            Token::Number { text, joined: true } => {
                for d in text.bytes().filter(u8::is_ascii_digit) {
                    out.push(Piece::Word(sounds_of(UNITS[usize::from(d - b'0')])));
                }
            }
            Token::Number { text, .. } => {
                // A time reads its hour and its minutes as two counts.
                for part in text.split(':') {
                    out.extend(number(part));
                }
            }
            // The Spanish reader speaks no signs.
            Token::Sign(_) => {}
            Token::Mark(':') => out.push(Piece::Mark('.')),
            Token::Mark(m) => out.push(Piece::Mark(*m)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::lang::Language;

    fn say(text: &str) -> String {
        Language::Spanish.transcribe(text)
    }

    #[test]
    fn reads_letters_and_stress() {
        assert_eq!(say("perro"), "PEH5RROH");
        assert_eq!(say("guerra"), "GEH5RRAA");
        assert_eq!(say("canción"), "KAANSIYOH5N");
        assert_eq!(say("ciudad"), "SIYUWDAA5D");
        assert_eq!(say("quiero"), "KIYEH5ROH");
    }

    #[test]
    fn reads_numbers_as_the_engine_does() {
        assert_eq!(say("42"), "KWAAREH5NTAAIY DOH5S");
        assert_eq!(say("16"), "DIYEHSIY SEY5S");
        assert_eq!(say("2024"), "DOH5S MIY5L VEY5NTEHIY KWAA5TROH");
        assert_eq!(say("1,500"), "MIY5L, KIYNIYEH4NTOHS");
        assert_eq!(say("3.5"), "TREY5S IY SIY5NXKOH DEH5SIYMAAL");
    }
}
