/*
util.rs - Native ports of packages/core/util.mjs semantic helpers
Helpers adapted from Strudel packages/core/util.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

use crate::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct NoteToken {
    pub pitch_class: char,
    pub accidentals: String,
    /// JavaScript Number result for the optional octave text.
    ///
    /// Pinned `tokenizeNote` accepts the bare signed spelling in `c-` because
    /// its regexp is `-?[0-9]*`; `Number('-')` is then NaN. An integer field
    /// could neither retain that result nor distinguish it from a missing
    /// octave, and also accidentally accepted Rust's `+4` integer syntax even
    /// though the pinned regexp rejects `c+4`.
    pub octave: Option<f64>,
}

pub fn is_note_with_octave(name: &str) -> bool {
    tokenize_note_impl(name, false).is_some()
}

pub fn is_note(name: &str) -> bool {
    tokenize_note_impl(name, true).is_some()
}

pub fn tokenize_note(note: &str) -> Option<NoteToken> {
    tokenize_note_impl(note, true)
}

fn tokenize_note_impl(note: &str, signed_octave: bool) -> Option<NoteToken> {
    let mut chars = note.chars().peekable();
    let pitch_class = chars.next()?;
    if !matches!(pitch_class.to_ascii_lowercase(), 'a'..='g') {
        return None;
    }
    let mut accidentals = String::new();
    while matches!(chars.peek(), Some('#' | 'b' | 's' | 'f')) {
        accidentals.push(chars.next().unwrap());
    }
    let rest: String = chars.collect();
    let octave = if rest.is_empty() {
        None
    } else {
        if !signed_octave && rest.starts_with('-') {
            return None;
        }
        let digits = rest.strip_prefix('-').unwrap_or(&rest);
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(if digits.is_empty() {
            f64::NAN
        } else {
            rest.parse().ok()?
        })
    };
    Some(NoteToken {
        pitch_class,
        accidentals,
        octave,
    })
}

pub fn accidentals_offset(accidentals: &str) -> i32 {
    accidentals
        .chars()
        .map(|ch| match ch {
            '#' | 's' => 1,
            'b' | 'f' => -1,
            _ => 0,
        })
        .sum()
}

pub fn note_to_midi(note: &str, default_octave: i32) -> Result<f64, String> {
    let token = tokenize_note(note).ok_or_else(|| format!("not a note: \"{note}\""))?;
    let chroma = match token.pitch_class.to_ascii_lowercase() {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => unreachable!(),
    };
    Ok(
        (token.octave.unwrap_or(f64::from(default_octave)) + 1.0) * 12.0
            + f64::from(chroma + accidentals_offset(&token.accidentals)),
    )
}

pub fn midi_to_freq(n: f64) -> f64 {
    2f64.powf((n - 69.0) / 12.0) * 440.0
}

pub fn freq_to_midi(freq: f64) -> f64 {
    12.0 * (freq / 440.0).log2() + 69.0
}

pub fn value_to_midi(value: &Value, fallback: Option<f64>) -> Result<f64, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "valueToMidi: expected object value".to_string())?;
    if let Some(freq) = object.get("freq").and_then(Value::as_f64) {
        return Ok(freq_to_midi(freq));
    }
    if let Some(note) = object.get("note") {
        match note {
            Value::Str(note) => return note_to_midi(note, 3),
            Value::F64(note) => return Ok(*note),
            _ => {}
        }
    }
    fallback.ok_or_else(|| "valueToMidi: expected freq or note to be set".to_string())
}

pub fn modulo(n: i64, m: i64) -> i64 {
    ((n % m) + m) % m
}

pub fn modulo_f64(n: f64, m: f64) -> f64 {
    ((n % m) + m) % m
}

pub fn average(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// ECMAScript `Math.round`, including ties toward positive infinity.
pub fn js_round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let floor = value.floor();
    let rounded = if value - floor < 0.5 {
        floor
    } else {
        floor + 1.0
    };
    // Math.round(-0.5) and every input in [-0.5, 0) produce negative zero.
    if rounded == 0.0 && value.is_sign_negative() {
        -0.0
    } else {
        rounded
    }
}

/// Wrap a rounded sample selector, returning zero when the bank is empty.
pub fn sound_index(value: Option<f64>, sounds: usize) -> usize {
    if sounds == 0 {
        return 0;
    }
    modulo(js_round(value.unwrap_or(0.0)) as i64, sounds as i64) as usize
}

pub fn rotate<T: Clone>(values: &[T], n: isize) -> Vec<T> {
    if values.is_empty() {
        return Vec::new();
    }
    // Upstream is `arr.slice(n).concat(arr.slice(0, n))`, not a modulo
    // rotation. Array.slice clamps a positive out-of-range index to `len` and
    // a negative one below `-len` to zero, so either extreme is the identity.
    let len = isize::try_from(values.len()).unwrap_or(isize::MAX);
    let at = if n < 0 {
        len.saturating_add(n).max(0)
    } else {
        n.min(len)
    } as usize;
    values[at..]
        .iter()
        .chain(values[..at].iter())
        .cloned()
        .collect()
}

pub fn list_range(min: i64, max: i64) -> Vec<i64> {
    if max < min {
        return Vec::new();
    }
    (min..=max).collect()
}

pub fn split_at_js<T: Clone>(index: isize, values: &[T]) -> (Vec<T>, Vec<T>) {
    let len = values.len() as isize;
    let at = if index < 0 {
        (len + index).max(0)
    } else {
        index.min(len)
    } as usize;
    (values[..at].to_vec(), values[at..].to_vec())
}

pub fn pairs<T: Clone>(values: &[T]) -> Vec<(T, T)> {
    values
        .windows(2)
        .map(|window| (window[0].clone(), window[1].clone()))
        .collect()
}

pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

pub fn parse_numeral(value: &Value) -> Result<f64, String> {
    match value {
        Value::F64(value) => Ok(*value),
        Value::Bool(value) => Ok(f64::from(*value)),
        Value::Null => Ok(0.0),
        Value::Str(value) => {
            if let Ok(number) = value.trim().parse::<f64>() {
                return Ok(number);
            }
            if value.trim().is_empty() {
                return Ok(0.0);
            }
            if is_note(value) {
                return note_to_midi(value, 3);
            }
            Err(format!("cannot parse as numeral: \"{value}\""))
        }
        _ => Err(format!("cannot parse as numeral: \"{}\"", value.show())),
    }
}

pub fn parse_fractional(value: &Value) -> Result<f64, String> {
    match parse_numeric(value) {
        Some(value) => Ok(value),
        None => {
            let Value::Str(name) = value else {
                return Err(format!("cannot parse as fractional: \"{}\"", value.show()));
            };
            match name.as_str() {
                "pi" => Ok(std::f64::consts::PI),
                "w" => Ok(1.0),
                "h" => Ok(0.5),
                "q" => Ok(0.25),
                "e" => Ok(0.125),
                "s" => Ok(0.0625),
                "t" => Ok(1.0 / 3.0),
                "f" => Ok(0.2),
                "x" => Ok(1.0 / 6.0),
                _ => Err(format!("cannot parse as fractional: \"{name}\"")),
            }
        }
    }
}

fn parse_numeric(value: &Value) -> Option<f64> {
    match value {
        Value::F64(value) => Some(*value),
        Value::Bool(value) => Some(f64::from(*value)),
        Value::Null => Some(0.0),
        Value::Str(value) if value.trim().is_empty() => Some(0.0),
        Value::Str(value) => value.parse().ok(),
        _ => None,
    }
}

const SOLFEGGIO: [&str; 12] = [
    "Do", "Reb", "Re", "Mib", "Mi", "Fa", "Solb", "Sol", "Lab", "La", "Sib", "Si",
];
const INDIAN: [&str; 7] = ["Sa", "Re", "Ga", "Ma", "Pa", "Dha", "Ni"];
const GERMAN: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Hb", "H",
];
const BYZANTINE: [&str; 12] = [
    "Ni", "Pab", "Pa", "Voub", "Vou", "Ga", "Dib", "Di", "Keb", "Ke", "Zob", "Zo",
];
const JAPANESE: [&str; 7] = ["I", "Ro", "Ha", "Ni", "Ho", "He", "To"];
const ENGLISH: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

/// `sol2note`, matching strudel.cc exactly, including its seven-note table quirk:
/// indexing is still modulo 12, so some inputs are unsupported.
pub fn sol_to_note(n: i32, notation: &str) -> Option<String> {
    let table: &[&str] = match notation {
        "solfeggio" => &SOLFEGGIO,
        "indian" => &INDIAN,
        "german" => &GERMAN,
        "byzantine" => &BYZANTINE,
        "japanese" => &JAPANESE,
        _ => &ENGLISH,
    };
    let index = n.rem_euclid(12) as usize;
    let note = table.get(index)?;
    Some(format!("{note}{}", n.div_euclid(12) - 1))
}

#[cfg(test)]
mod sound_index_tests {
    use super::sound_index;

    #[test]
    fn an_empty_bank_has_no_wrapped_index() {
        assert_eq!(sound_index(Some(3.0), 0), 0);
        assert_eq!(sound_index(None, 0), 0);
    }

    #[test]
    fn a_nonempty_bank_keeps_rounding_and_wrapping() {
        assert_eq!(sound_index(None, 4), 0);
        assert_eq!(sound_index(Some(4.5), 4), 1);
        assert_eq!(sound_index(Some(-1.0), 4), 3);
        assert_eq!(sound_index(Some(-0.5), 4), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strudel_note_cases_all_parse() {
        assert!(is_note("C#3"));
        assert!(is_note("Fbb3"));
        assert!(is_note("c-2"));
        assert!(is_note("c-"), "pinned regexp accepts a bare minus octave");
        assert!(!is_note("c+4"), "pinned regexp has no plus-sign branch");
        assert!(!is_note("H5"));
        assert_eq!(note_to_midi("A4", 3), Ok(69.0));
        assert_eq!(note_to_midi("Cbb3", 3), Ok(46.0));
        assert!(note_to_midi("c-", 3).unwrap().is_nan());
        assert!(note_to_midi("c+4", 3).is_err());
        assert_eq!(midi_to_freq(57.0), 220.0);
        assert_eq!(freq_to_midi(220.0), 57.0);
    }

    #[test]
    fn the_strudel_collection_cases_all_parse() {
        assert_eq!(rotate(&[0, 1, 2, 3], -3), vec![1, 2, 3, 0]);
        assert_eq!(rotate(&[0, 1, 2, 3], 4), vec![0, 1, 2, 3]);
        assert_eq!(rotate(&[0, 1, 2, 3], 5), vec![0, 1, 2, 3]);
        assert_eq!(rotate(&[0, 1, 2, 3], -4), vec![0, 1, 2, 3]);
        assert_eq!(rotate(&[0, 1, 2, 3], -5), vec![0, 1, 2, 3]);
        assert_eq!(rotate(&[0, 1, 2, 3], isize::MAX), vec![0, 1, 2, 3]);
        assert_eq!(rotate(&[0, 1, 2, 3], isize::MIN), vec![0, 1, 2, 3]);
        assert_eq!(split_at_js(-3, &[0, 1, 2, 3]), (vec![0], vec![1, 2, 3]));
        assert_eq!(pairs(&[0, 1, 2]), vec![(0, 1), (1, 2)]);
        assert_eq!(modulo(-5, 3), 1);
        assert_eq!(js_round(0.49999999999999994), 0.0);
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(-0.5000000000000001), -1.0);
        assert!(js_round(-0.5).is_sign_negative());
        assert!(js_round(-0.1).is_sign_negative());
    }

    #[test]
    fn parses_the_numeral_and_fractional_shorthands() {
        assert_eq!(parse_numeral(&Value::Str("c4".into())), Ok(60.0));
        assert_eq!(parse_fractional(&Value::Str("q".into())), Ok(0.25));
        assert!(parse_fractional(&Value::Str("xyz".into())).is_err());
    }
}
