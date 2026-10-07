/*
tonaljs.rs - tonal-theory pitch algebra: notes, intervals, scales
Scale wrappers adapted from Strudel packages/tonal/tonal.mjs:
Copyright (C) 2022 Strudel contributors

Pitch algebra and scale dictionary adapted from @tonaljs 4.10.0:
Copyright (c) 2015 danigb
See crates/core/LICENSE-tonal for the original MIT terms.

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Pitch algebra for the tonal layer: note/interval parsing, fifths-octaves
//! coordinates, enharmonic-correct transposition and the scale-name
//! dictionary (committed generated data in [`crate::tonaljs_scales`]).
//!
//! Degenerate spellings (`"5Pxx"`, `"c03"`) resolve to the strudel.cc
//! musical result even where the string round-trip differs; the
//! compatibility fixtures pin the observable results.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::tonaljs_scales::SCALE_DICTIONARY;

// ---------------------------------------------------------------------------
// Fifths/octaves coordinates
// ---------------------------------------------------------------------------

/// Per-letter (C..B) semitone sizes, fifths coordinates, and their inverses.
const SIZES: [i64; 7] = [0, 2, 4, 5, 7, 9, 11];
const FIFTHS: [i64; 7] = [0, 2, 4, -1, 1, 3, 5];
const STEPS_TO_OCTS: [i64; 7] = [0, 1, 2, -1, 0, 1, 2];
const FIFTHS_TO_STEPS: [usize; 7] = [3, 0, 4, 1, 5, 2, 6];

/// Floor division: rounds toward negative infinity.
fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// Pitch → fifths/octaves coordinates; pitch classes drop the octave part.
fn coordinates(step: usize, alt: i64, oct: Option<i64>, dir: i64) -> Option<(i64, Option<i64>)> {
    let f = i64::try_from((FIFTHS[step] as i128 + 7 * alt as i128) * dir as i128).ok()?;
    let o = oct
        .map(|oct| {
            i64::try_from(
                (oct as i128 - STEPS_TO_OCTS[step] as i128 - 4 * alt as i128) * dir as i128,
            )
        })
        .transpose()
        .ok()?;
    Some((f, o))
}

/// Fifths/octaves coordinates → (step, alt, oct).
fn pitch_from_coord(f: i64, o: Option<i64>) -> Option<(usize, i64, Option<i64>)> {
    let shifted = f as i128 + 1;
    let step = FIFTHS_TO_STEPS[shifted.rem_euclid(7) as usize];
    let alt = i64::try_from(shifted.div_euclid(7)).ok()?;
    let oct = o
        .map(|o| i64::try_from(o as i128 + 4 * alt as i128 + STEPS_TO_OCTS[step] as i128))
        .transpose()
        .ok()?;
    Some((step, alt, oct))
}

// ---------------------------------------------------------------------------
// Note names: parsing and printing
// ---------------------------------------------------------------------------

/// A successfully parsed note; parse failures are `None` at this layer.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteProps {
    /// Canonical name: letter + accidentals (+ octave when present).
    pub name: String,
    /// Pitch class: letter + accidentals.
    pub pc: String,
    pub step: usize,
    pub alt: i64,
    pub oct: Option<i64>,
    /// Fifths/octaves coordinate (octave component `None` for pitch classes).
    pub coord: (i64, Option<i64>),
    pub chroma: i64,
    pub height: i64,
    pub midi: Option<i64>,
}

/// Splits a note name into (letter, accidentals, octave, rest). The
/// accidental run is homogeneous (`#...`, `b...` or `x...`, never mixed);
/// `x` expands to `##`.
fn tokenize_note(str: &str) -> (String, String, String, String) {
    let mut chars = str.chars().peekable();
    let letter = match chars.peek() {
        Some(c @ 'a'..='g') | Some(c @ 'A'..='G') => {
            let up = c.to_ascii_uppercase();
            chars.next();
            up.to_string()
        }
        _ => String::new(),
    };
    let acc_char = match chars.peek() {
        Some('#') => Some('#'),
        Some('b') => Some('b'),
        Some('x') => Some('x'),
        _ => None,
    };
    let mut acc = String::new();
    if let Some(ac) = acc_char {
        while chars.peek() == Some(&ac) {
            chars.next();
            acc.push(ac);
        }
    }
    let acc = acc.replace('x', "##");
    let mut oct = String::new();
    if chars.peek() == Some(&'-') {
        let mut ahead = chars.clone();
        ahead.next();
        if matches!(ahead.peek(), Some('0'..='9')) {
            chars.next();
            oct.push('-');
        }
    }
    while matches!(chars.peek(), Some('0'..='9')) {
        oct.push(chars.next().unwrap());
    }
    let rest: String = chars.collect::<String>().trim_start().to_string();
    (letter, acc, oct, rest)
}

/// Parses a note name into its algebraic record; `None` on garbage.
pub fn note_get(name: &str) -> Option<NoteProps> {
    let (letter, acc, oct_str, rest) = tokenize_note(name);
    if letter.is_empty() || !rest.is_empty() {
        return None;
    }
    let step = ((letter.as_bytes()[0] as i64) + 3).rem_euclid(7) as usize;
    // A leading 'b' counts negative; otherwise the run length counts up.
    let acc_len = i64::try_from(acc.len()).ok()?;
    let alt = if acc.starts_with('b') {
        -acc_len
    } else {
        acc_len
    };
    let oct: Option<i64> = if oct_str.is_empty() {
        None
    } else {
        Some(oct_str.parse().ok()?)
    };
    let coord = coordinates(step, alt, oct, 1)?;
    // height: octless pitch classes sit 99 octaves down (mod 12 first).
    let height = match oct {
        None => (SIZES[step] + alt).rem_euclid(12) - 12 * 99,
        Some(oct) => {
            i64::try_from(SIZES[step] as i128 + alt as i128 + 12 * (oct as i128 + 1)).ok()?
        }
    };
    let midi = (0..=127).contains(&height).then_some(height);
    let pc = format!("{letter}{acc}");
    let name = match oct {
        // Only degenerate spellings like "c03" print differently than typed.
        Some(oct) => format!("{pc}{oct}"),
        None => pc.clone(),
    };
    Some(NoteProps {
        name,
        pc,
        step,
        alt,
        oct,
        coord,
        chroma: (SIZES[step] + alt).rem_euclid(12),
        height,
        midi,
    })
}

/// Prints (step, alt, oct) as a note name.
fn note_pitch_name(step: usize, alt: i64, oct: Option<i64>) -> String {
    let letter = b"CDEFGAB"[step] as char;
    let acc = if alt < 0 {
        "b".repeat((-alt) as usize)
    } else {
        "#".repeat(alt as usize)
    };
    match oct {
        Some(oct) => format!("{letter}{acc}{oct}"),
        None => format!("{letter}{acc}"),
    }
}

/// Coordinate → note name.
fn note_name_from_coord(f: i64, o: Option<i64>) -> Option<String> {
    let (step, alt, oct) = pitch_from_coord(f, o)?;
    Some(note_pitch_name(step, alt, oct))
}

// ---------------------------------------------------------------------------
// Interval names: parsing and printing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntervalType {
    Perfectable,
    Majorable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntervalProps {
    pub name: String,
    pub semitones: i64,
    /// `[fifths, octs, dir]`.
    pub coord: (i64, i64, i64),
}

/// Quality class per degree: P M M P P M M.
const TYPES: [IntervalType; 7] = [
    IntervalType::Perfectable,
    IntervalType::Majorable,
    IntervalType::Majorable,
    IntervalType::Perfectable,
    IntervalType::Perfectable,
    IntervalType::Majorable,
    IntervalType::Majorable,
];

/// Splits an interval name into (number, quality). Two grammars: tonal
/// ("3M") and shorthand ("M3"). Only the tonal form anchors at the start
/// and only the shorthand form at the end, so trailing garbage after a
/// tonal match is ignored.
fn tokenize_interval(str: &str) -> Option<(i64, String)> {
    let bytes = str.as_bytes();
    // Tonal form: signed digits then a quality run, anchored at the start.
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > digits_start {
        let num: i64 = str[..i].parse().ok()?;
        let rest = &bytes[i..];
        // Ordered alternation: d{1,4} | m | M | P | A{1,4}.
        let quality_len = match rest.first() {
            Some(b'd') => Some(rest.iter().take_while(|&&b| b == b'd').count().min(4)),
            Some(b'm') => Some(1),
            Some(b'M') => Some(1),
            Some(b'P') => Some(1),
            Some(b'A') => Some(rest.iter().take_while(|&&b| b == b'A').count().min(4)),
            _ => None,
        };
        if let Some(len) = quality_len {
            return Some((num, str[i..i + len].to_string()));
        }
    }
    // Shorthand form: quality then signed digits, anchored at the end; the
    // leftmost start that matches through to the end wins.
    for (start, _) in str.char_indices() {
        for q in ["AA", "A", "P", "M", "m", "d", "dd"] {
            if str[start..].starts_with(q) {
                let after = &str[start + q.len()..];
                let ab = after.as_bytes();
                let mut j = 0;
                if j < ab.len() && (ab[j] == b'-' || ab[j] == b'+') {
                    j += 1;
                }
                if j < ab.len()
                    && ab[j..].iter().all(|b| b.is_ascii_digit())
                    && let Ok(num) = after.parse::<i64>()
                {
                    return Some((num, q.to_string()));
                }
            }
        }
    }
    None
}

/// Quality → alteration within a degree's quality class.
fn q_to_alt(interval_type: IntervalType, q: &str) -> Option<i64> {
    use IntervalType::*;
    match (q, interval_type) {
        ("M", Majorable) | ("P", Perfectable) => Some(0),
        ("m", Majorable) => Some(-1),
        _ if !q.is_empty() && q.bytes().all(|b| b == b'A') => Some(q.len() as i64),
        _ if !q.is_empty() && q.bytes().all(|b| b == b'd') => Some(match interval_type {
            Perfectable => -(q.len() as i64),
            Majorable => -(q.len() as i64 + 1),
        }),
        // "M" on a perfectable degree resolves to 0; the one rejected
        // pairing ("P" on a majorable degree) is filtered by the caller.
        _ => Some(0),
    }
}

/// Alteration → quality string.
fn alt_to_q(interval_type: IntervalType, alt: i64) -> String {
    use IntervalType::*;
    if alt == 0 {
        match interval_type {
            Majorable => "M".into(),
            Perfectable => "P".into(),
        }
    } else if alt == -1 && interval_type == Majorable {
        "m".into()
    } else if alt > 0 {
        "A".repeat(alt as usize)
    } else {
        let d = match interval_type {
            Perfectable => -alt,
            Majorable => -(alt + 1),
        };
        "d".repeat(d as usize)
    }
}

/// Parses an interval name into its algebraic record.
pub fn interval_get(src: &str) -> Option<IntervalProps> {
    let (num, q) = tokenize_interval(src)?;
    let magnitude = num.checked_abs()?;
    let step = ((magnitude - 1).rem_euclid(7)) as usize;
    let t = TYPES[step];
    if t == IntervalType::Majorable && q == "P" {
        return None;
    }
    // "5M" is accepted on purpose: "M" on a perfectable degree resolves to
    // alt 0, i.e. the perfect interval.
    let alt = q_to_alt(t, &q)?;
    let dir: i64 = if num < 0 { -1 } else { 1 };
    let oct = floor_div(magnitude - 1, 7);
    let semitones =
        i64::try_from(dir as i128 * (SIZES[step] as i128 + alt as i128 + 12 * oct as i128)).ok()?;
    let (f, o) = coordinates(step, alt, Some(oct), dir)?;
    Some(IntervalProps {
        name: format!("{num}{q}"),
        semitones,
        coord: (
            f,
            o.expect("interval coordinates always carry octaves"),
            dir,
        ),
    })
}

/// Prints (step, alt, oct, dir) as a tonal-form interval name.
fn interval_pitch_name(step: usize, alt: i64, oct: i64, dir: i64) -> Option<String> {
    let calc_num = i64::try_from(step as i128 + 1 + 7 * oct as i128).ok()?;
    let num = if calc_num == 0 {
        step as i64 + 1
    } else {
        calc_num
    };
    let d = if dir < 0 { "-" } else { "" };
    Some(format!("{d}{num}{}", alt_to_q(TYPES[step], alt)))
}

/// Coordinate → interval name; descending when the span is negative.
fn interval_name_from_coord(f: i64, o: i64) -> Option<String> {
    let span = 7 * f as i128 + 12 * o as i128;
    i64::try_from(span).ok()?;
    let (f, o, dir) = if span < 0 {
        (f.checked_neg()?, o.checked_neg()?, -1)
    } else {
        (f, o, 1)
    };
    let (step, alt, oct) = pitch_from_coord(f, Some(o))?;
    interval_pitch_name(step, alt, oct.expect("octave carried through"), dir)
}

/// Semitone span of an interval name; `None` on garbage.
pub fn interval_semitones(name: &str) -> Option<i64> {
    interval_get(name).map(|i| i.semitones)
}

/// Names the interval spanning a semitone count ("5P" for 7, "5d" for 6).
/// Total over `i64`, `i64::MIN` included.
pub fn interval_from_semitones(semitones: i64) -> String {
    const IN: [u64; 12] = [1, 2, 2, 3, 3, 4, 5, 5, 6, 6, 7, 7];
    const IQ: [&str; 12] = ["P", "m", "M", "m", "M", "P", "d", "P", "m", "M", "m", "M"];
    // The magnitude as u64 keeps i64::MIN (2^63) whole, where `abs` would
    // overflow, and makes the remainder a valid index.
    let n = semitones.unsigned_abs();
    let c = (n % 12) as usize;
    let o = n / 12;
    // n <= 2^63 bounds o by 2^63 / 12, so the number stays below 2^63.
    let number = IN[c] + 7 * o;
    // IN[c] >= 1, so a descending interval never prints as "-0".
    let sign = if semitones < 0 { "-" } else { "" };
    format!("{sign}{number}{}", IQ[c])
}

/// A semitone count as `i64` when the conversion is exact: `n` integral and
/// strictly inside (-2^63, 2^63). `None` for fractions, NaN, ±inf and
/// anything beyond, where `as i64` would saturate (or send NaN to 0) and so
/// name a different interval than the one asked for.
pub(crate) fn exact_semitones(n: f64) -> Option<i64> {
    let in_range = n > i64::MIN as f64 && n < i64::MAX as f64;
    (in_range && n.fract() == 0.0).then_some(n as i64)
}

/// Adds two intervals; `None` when either is invalid.
pub fn interval_add(a: &str, b: &str) -> Option<String> {
    let a = interval_get(a)?;
    let b = interval_get(b)?;
    // Stored coords are already signed by dir, so addition is componentwise.
    interval_name_from_coord(
        a.coord.0.checked_add(b.coord.0)?,
        a.coord.1.checked_add(b.coord.1)?,
    )
}

/// Subtracts two intervals; `None` when either is invalid.
pub fn interval_subtract(a: &str, b: &str) -> Option<String> {
    let a = interval_get(a)?;
    let b = interval_get(b)?;
    interval_name_from_coord(
        a.coord.0.checked_sub(b.coord.0)?,
        a.coord.1.checked_sub(b.coord.1)?,
    )
}

// ---------------------------------------------------------------------------
// Transposition
// ---------------------------------------------------------------------------

/// Transposes a note by an interval; `""` on any invalid input. Pitch
/// classes stay pitch classes.
pub fn note_transpose(note_name: &str, interval_name: &str) -> String {
    let Some(note) = note_get(note_name) else {
        return String::new();
    };
    let Some(interval) = interval_get(interval_name) else {
        return String::new();
    };
    let (nf, no) = note.coord;
    let (if_, io, _dir) = interval.coord;
    match no {
        None => nf
            .checked_add(if_)
            .and_then(|f| note_name_from_coord(f, None))
            .unwrap_or_default(),
        Some(no) => nf
            .checked_add(if_)
            .zip(no.checked_add(io))
            .and_then(|(f, o)| note_name_from_coord(f, Some(o)))
            .unwrap_or_default(),
    }
}

/// Respells a note as the given enharmonic pitch class (`""` on a chroma
/// mismatch); the voicings finder uses it to keep dictionary spellings.
pub fn note_enharmonic(note_name: &str, dest_name: &str) -> String {
    let Some(src) = note_get(note_name) else {
        return String::new();
    };
    let Some(dest) = note_get(dest_name) else {
        return String::new();
    };
    if dest.chroma != src.chroma {
        return String::new();
    }
    let Some(src_oct) = src.oct else {
        return dest.pc;
    };
    let src_chroma = src.chroma - src.alt;
    let dest_chroma = dest.chroma - dest.alt;
    let dest_oct_offset = if src_chroma > 11 || dest_chroma < 0 {
        -1
    } else if src_chroma < 0 || dest_chroma > 11 {
        1
    } else {
        0
    };
    format!("{}{}", dest.pc, src_oct + dest_oct_offset)
}

/// Transposes, giving octave-less notes a default octave of 3; the octave
/// stays in the result only when the transposition leaves octave 3
/// (`C + 2M → D`, but `B + 2M → C#4`). An unparsable note is `None` and the
/// caller must abort the query; an invalid interval is `Some("")`.
/// Order-independent by design: no note cache is consulted or mutated, so
/// earlier calls can never change a later result.
pub fn note_transpose_defaulted(note_name: &str, interval_name: &str) -> Option<String> {
    let note = note_get(note_name)?;
    if note.oct.is_some() {
        return Some(note_transpose(note_name, interval_name));
    }
    if interval_get(interval_name).is_none() {
        // An invalid interval yields "", not an abort.
        return Some(String::new());
    }
    let with_oct3 = format!("{}3", note.pc);
    let out = note_transpose(&with_oct3, interval_name);
    match note_get(&out) {
        Some(result) if result.oct == Some(3) => Some(result.pc),
        _ => Some(out),
    }
}

// ---------------------------------------------------------------------------
// The scale-name dictionary
// ---------------------------------------------------------------------------

fn scale_index() -> &'static HashMap<&'static str, usize> {
    static INDEX: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        for (i, (name, aliases, _)) in SCALE_DICTIONARY.iter().enumerate() {
            index.entry(*name).or_insert(i);
            for alias in *aliases {
                index.entry(*alias).or_insert(i);
            }
        }
        index
    })
}

/// A resolved scale: tonic, intervals, tonic-transposed notes.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleProps {
    /// Canonical tonic name (`None` when the query had no tonic).
    pub tonic: Option<String>,
    pub intervals: &'static [&'static str],
    /// Tonic-transposed note names; empty without a tonic.
    pub notes: Vec<String>,
}

/// Splits a scale query into (tonic, type); either half may be empty.
pub fn scale_tokenize(name: &str) -> (String, String) {
    let head = name.split(' ').next().unwrap_or("");
    let tonic = if head.len() == name.len() {
        None // no space: the tonic substring is empty, so it never parses
    } else {
        note_get(head)
    };
    match tonic {
        None => match note_get(name) {
            None => (String::new(), name.to_lowercase()),
            Some(n) => (n.name, String::new()),
        },
        Some(tonic) => {
            // The normalized tonic can be longer ("Cx" -> "C##") or shorter
            // ("c03" -> "C3") than the input. Slice after the original head.
            let type_start = head.len() + 1;
            let scale_type = name[type_start..].to_lowercase();
            (tonic.name, scale_type)
        }
    }
}

/// Resolves a scale query; `None` for an unknown scale type.
pub fn scale_get(src: &str) -> Option<ScaleProps> {
    let (tonic_name, type_name) = scale_tokenize(src);
    let index = *scale_index().get(type_name.as_str())?;
    let intervals = SCALE_DICTIONARY[index].2;
    let tonic = (!tonic_name.is_empty()).then_some(tonic_name);
    let notes = match &tonic {
        Some(tonic) => intervals.iter().map(|i| note_transpose(tonic, i)).collect(),
        None => Vec::new(),
    };
    Some(ScaleProps {
        tonic,
        intervals,
        notes,
    })
}

// ---------------------------------------------------------------------------
// the tonal layer - what the combinators call
// ---------------------------------------------------------------------------

/// Note-shaped test: letter, a `#bsf` run, an optional `-` and digits.
/// Looser than [`note_get`]; used to phrase scale errors.
pub fn is_score_note(value: &str) -> bool {
    let mut chars = value.chars();
    if !matches!(chars.next(), Some('a'..='g' | 'A'..='G')) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    let mut i = 0;
    while i < rest.len() && matches!(rest[i], '#' | 'b' | 's' | 'f') {
        i += 1;
    }
    if i < rest.len() && rest[i] == '-' {
        i += 1;
    }
    rest[i..].iter().all(|c| c.is_ascii_digit())
}

/// Resolves a `"C:major"`-style scale name (colons become spaces); a bare
/// note or a tonic-less query gets the "incomplete" hint instead.
pub fn get_scale(scale_name: &str) -> Result<ScaleProps, String> {
    let spaced = scale_name.replace(':', " ");
    match scale_get(&spaced) {
        Some(scale) => Ok(scale),
        None => {
            let (tonic, _) = scale_tokenize(&spaced);
            if is_score_note(&spaced) || tonic.is_empty() {
                Err(format!(
                    "Scale name {spaced} is incomplete. Make sure to use \":\" instead of spaces, example: .scale(\"C:major\")"
                ))
            } else {
                Err(format!("Invalid scale name \"{spaced}\""))
            }
        }
    }
}

/// Perfect interval spanning `octaves` octaves ("8P" for 1, "-8P" for -1).
fn octaves_interval(octaves: i64) -> Option<String> {
    let base: i64 = if octaves <= 0 { -1 } else { 1 };
    let degree = i64::try_from(base as i128 + octaves as i128 * 7).ok()?;
    Some(format!("{degree}P"))
}

/// Note at scale degree `step` (ceil'd); tonic defaults to C, octave to 3.
pub fn scale_step(step: f64, scale: &str) -> Result<String, String> {
    let rounded = step.ceil();
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= -(i64::MIN as f64) {
        return Err(format!("scale step \"{step}\" exceeds the native range"));
    }
    let step = rounded as i64;
    let resolved = get_scale(scale)?;
    let tonic = resolved.tonic.as_deref().unwrap_or("C");
    let tonic_note = note_get(tonic).ok_or_else(|| format!("invalid tonic \"{tonic}\""))?;
    let oct = tonic_note.oct.unwrap_or(3);
    let len = resolved.intervals.len() as i64;
    let octave_offset = floor_div(step, len);
    let index = step.rem_euclid(len) as usize;
    let range_error = || format!("scale step \"{step}\" exceeds the native range");
    let octaves = octaves_interval(octave_offset).ok_or_else(range_error)?;
    let interval = interval_add(resolved.intervals[index], &octaves).ok_or_else(range_error)?;
    let transposed = note_transpose(&format!("{}{oct}", tonic_note.pc), &interval);
    if transposed.is_empty() {
        return Err(range_error());
    }
    Ok(transposed)
}

/// Walks `offset` degrees through the scale from `note`, bumping the octave
/// at every C crossing.
pub fn scale_offset(scale: &str, offset: f64, note: &str) -> Result<String, String> {
    let resolved = get_scale(scale)?;
    let notes: Vec<String> = resolved
        .notes
        .iter()
        .map(|n| note_get(n).map(|p| p.pc).unwrap_or_default())
        .collect();
    if notes.is_empty() {
        return Err(format!("scale \"{scale}\" has no notes"));
    }
    if offset.is_nan() {
        return Err(format!("scale offset \"{offset}\" not a number"));
    }
    if crate::cancellation_requested() || crate::query_deadline_expired() {
        return Err("scale offset interrupted".into());
    }
    // An unparsable note gets pc "" - never in any scale - so it falls
    // into the not-in-scale error below.
    let (from_pc, oct) = match note_get(note) {
        Some(from) => (from.pc, from.oct.unwrap_or(3)),
        None => (String::new(), 3),
    };
    let note_index = notes
        .iter()
        .position(|n| *n == from_pc)
        .ok_or_else(|| format!("note \"{note}\" is not in scale \"{scale}\""))?
        as i64;
    // A fractional offset takes ceil(abs(offset)) whole steps. The final
    // index and C-crossing count are closed-form: an O(|offset|) step loop
    // here ran without a deadline poll, and huge offsets froze the producer.
    const I64_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    let steps = offset.abs().ceil();
    if !steps.is_finite() || steps >= I64_MAX_EXCLUSIVE {
        return Err(format!(
            "scale offset \"{offset}\" exceeds the native range"
        ));
    }
    let steps = steps as i64;
    let direction: i64 = if offset > 0.0 {
        1
    } else if offset < 0.0 {
        -1
    } else {
        0
    };
    if steps == 0 || direction == 0 {
        return Ok(format!("{from_pc}{oct}"));
    }

    let len = i64::try_from(notes.len()).map_err(|_| "scale has too many notes")?;
    let complete_cycles = steps / len;
    let remainder = steps % len;
    let c_notes = i64::try_from(notes.iter().filter(|note| note.starts_with('C')).count())
        .map_err(|_| "scale has too many notes")?;
    let mut crossings = complete_cycles
        .checked_mul(c_notes)
        .ok_or_else(|| format!("scale offset \"{offset}\" exceeds the native range"))?;
    for step in 0..remainder {
        let relative = if direction > 0 { step + 1 } else { -step };
        let index = (note_index + relative).rem_euclid(len) as usize;
        if notes[index].starts_with('C') {
            crossings = crossings
                .checked_add(1)
                .ok_or_else(|| format!("scale offset \"{offset}\" exceeds the native range"))?;
        }
    }
    let octave_delta = direction
        .checked_mul(crossings)
        .ok_or_else(|| format!("scale offset \"{offset}\" exceeds the native range"))?;
    let octave = oct
        .checked_add(octave_delta)
        .ok_or_else(|| format!("scale offset \"{offset}\" exceeds the native range"))?;
    let final_index = (note_index + direction * remainder).rem_euclid(len) as usize;
    Ok(format!("{}{octave}", notes[final_index]))
}

#[cfg(test)]
mod scale_offset_scan_tests {
    use super::scale_offset;

    /// A large offset was once a loop of that many iterations per hap, with
    /// no deadline poll: a saved `scaleTranspose(1e9)` stalled the producer
    /// for a minute while the process reported healthy.
    #[test]
    fn a_huge_offset_returns_promptly() {
        let started = std::time::Instant::now();
        let far = scale_offset("C:major", 1e9, "c3").expect("a large offset resolves");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "a billion-degree transpose took {:?}",
            started.elapsed()
        );
        assert!(!far.is_empty());
    }

    /// The closed form must agree with stepping, degree for degree, including
    /// the octave changes that stepping counted by crossing C.
    #[test]
    fn the_closed_form_agrees_with_stepping() {
        // Stepping, written out independently of the implementation.
        fn by_steps(scale: &str, offset: i64, note: &str) -> String {
            // Normalised through the same door, so zero steps compares as
            // "C3" rather than the raw "c3" the caller wrote.
            let mut current = scale_offset(scale, 0.0, note).expect("normalise");
            let step = offset.signum();
            for _ in 0..offset.abs() {
                current = scale_offset(scale, step as f64, &current).expect("single step");
            }
            current
        }
        for scale in ["C:major", "C:lydian", "D:minor"] {
            for offset in [-15i64, -8, -3, -1, 0, 1, 3, 8, 15] {
                let closed = scale_offset(scale, offset as f64, "c3").expect("closed form");
                let stepped = by_steps(scale, offset, "c3");
                assert_eq!(
                    closed, stepped,
                    "{scale} offset {offset}: closed form {closed} != stepped {stepped}"
                );
            }
        }
    }
}

/// Index of the entry nearest `target`; `prefer_higher` breaks ties toward
/// the later entry.
pub fn nearest_number_index(target: f64, numbers: &[f64], prefer_higher: bool) -> usize {
    let mut best_index = 0;
    let mut best_diff = f64::INFINITY;
    for (i, s) in numbers.iter().enumerate() {
        let diff = (s - target).abs();
        if (!prefer_higher && diff < best_diff) || (prefer_higher && diff <= best_diff) {
            best_index = i;
            best_diff = diff;
        }
    }
    best_index
}

/// Snaps a MIDI number to the nearest scale note, preferring the higher on
/// ties. Recomputed per call - a handful of coordinate additions.
pub fn nearest_scale_note(scale_name: &str, note_midi: f64) -> Result<String, String> {
    let resolved = get_scale(scale_name)?;
    // A scale named without a root (`major`, or the second step of a
    // mini-notation string written with a space) quantizes against C, as
    // in `scale_step`. An empty root is not a note and would fail the
    // whole query.
    let tonic = resolved.tonic.as_deref().unwrap_or("C");
    let pc = note_get(tonic).map(|p| p.pc).unwrap_or_default();
    let root = format!("{pc}0");
    let mut s_notes: Vec<String> = resolved
        .intervals
        .iter()
        .map(|i| note_transpose(&root, i))
        .collect();
    s_notes.push(note_transpose(&root, "8P"));
    let s_midi: Vec<f64> = s_notes
        .iter()
        .map(|n| {
            crate::util::note_to_midi(n, 3).map_err(|error| format!("scale note \"{n}\": {error}"))
        })
        .collect::<Result<_, _>>()?;
    let root_midi = s_midi[0];
    let octave_diff = ((note_midi - root_midi) / 12.0).floor();
    // The note's octave is an unbounded f64 ("c-" is NaN, a 400-digit octave
    // is ±inf), so the octave shift may not fit an i64. Strudel does not
    // throw here: fromSemitones names no interval for NaN or ±inf, and the
    // note quantizes to "". A finite shift beyond i64 (Strudel keeps its
    // giant octave) cannot be named exactly, so it quantizes to "" as well
    // instead of saturating.
    let Some(shift) = exact_semitones(12.0 * octave_diff) else {
        return Ok(String::new());
    };
    let aligned: Vec<f64> = s_midi.iter().map(|m| m + 12.0 * octave_diff).collect();
    let idx = nearest_number_index(note_midi, &aligned, true);
    Ok(note_transpose(
        &s_notes[idx],
        &interval_from_semitones(shift),
    ))
}

/// Semitone value of scale degree `step`, optionally anchored near a MIDI
/// note. `scale` arrives raw and needs the space form ("C major"): a colon
/// spelling parses as one big type with an empty root, which cannot resolve
/// to MIDI.
pub fn step_in_named_scale(
    step: f64,
    scale: &str,
    anchor_midi: Option<f64>,
) -> Result<f64, String> {
    let (root, scale_name) = scale_tokenize(scale);
    let root_midi = crate::util::note_to_midi(&root, 3)
        .map_err(|error| format!("scale root \"{root}\": {error}"))?;
    let root_chroma = root_midi % 12.0;
    // An unknown scale type resolves to NaN, never an error.
    let steps: Vec<f64> = match scale_get(&format!("C {scale_name}")) {
        Some(resolved) => resolved
            .intervals
            .iter()
            .map(|i| {
                interval_semitones(i)
                    .map(|s| s as f64)
                    .ok_or_else(|| format!("bad interval \"{i}\""))
            })
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    if steps.is_empty() {
        return Ok(f64::NAN);
    }
    let mut step = step;
    let mut transpose = root_midi;
    if let Some(anchor) = anchor_midi {
        let anchor_chroma = anchor % 12.0;
        let anchor_diff = (anchor_chroma - root_chroma).rem_euclid(12.0);
        // The scale combinator never asks to prefer the higher tie.
        let zero_index = nearest_number_index(anchor_diff, &steps, false);
        step += zero_index as f64;
        transpose = anchor - anchor_diff;
    }
    let len = steps.len() as f64;
    let oct_offset = (step / len).floor() * 12.0;
    if !step.is_finite() {
        return Ok(f64::NAN);
    }
    // `_mod`'s `((i % n) + n) % n` maps a tiny negative step to 0;
    // `rem_euclid` would round it up to `len`. The clamp keeps it in bounds.
    let index = (crate::util::modulo_f64(step, len) as usize).min(steps.len() - 1);
    Ok(steps[index] + transpose + oct_offset)
}

/// Parses a scale-step string: a plain number, or an integer with
/// `#`/`b`/`s`/`f` accidental suffixes. Numeric values never reach here.
pub fn convert_step_string(step: &str) -> Result<(f64, i64), String> {
    if let Some(n) = js_number(step) {
        return Ok((n, 0));
    }
    let (digits, accidentals) = match step.find(['#', 'b', 's', 'f']) {
        Some(i) => step.split_at(i),
        None => (step, ""),
    };
    let valid_digits = {
        let d = digits.strip_prefix('-').unwrap_or(digits);
        !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
    };
    if !valid_digits
        || !accidentals
            .chars()
            .all(|c| matches!(c, '#' | 'b' | 's' | 'f'))
    {
        return Err(format!(
            "invalid scale step \"{step}\", expected number or integer with optional # b suffixes"
        ));
    }
    let number: f64 = digits.parse().map_err(|_| {
        format!(
            "invalid scale step \"{step}\", expected number or integer with optional # b suffixes"
        )
    })?;
    // '#'/'s' raise, 'b'/'f' lower.
    let offset = accidentals
        .chars()
        .map(|c| match c {
            '#' | 's' => 1,
            'b' | 'f' => -1,
            _ => 0,
        })
        .sum();
    Ok((number, offset))
}

/// JS `Number(str)` coercion for the shapes the scale layer meets: decimal
/// strings and "" (which is 0). `None` where the coercion would be NaN.
pub fn js_number(str: &str) -> Option<f64> {
    let trimmed = str.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    let body = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    if body.is_empty() || !body.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale_offset_walk_reference(scale: &str, offset: f64, note: &str) -> String {
        let resolved = get_scale(scale).expect("test scale");
        let notes: Vec<String> = resolved
            .notes
            .iter()
            .map(|name| note_get(name).expect("scale note").pc)
            .collect();
        let from = note_get(note).expect("test note");
        let note_index = notes
            .iter()
            .position(|name| *name == from.pc)
            .expect("note in scale") as i64;
        let mut i = note_index;
        let mut octave = from.oct.unwrap_or(3);
        let mut name = from.pc;
        let direction = offset.signum() as i64;
        while ((i - note_index).abs() as f64) < offset.abs() {
            i += direction;
            let index = i.rem_euclid(notes.len() as i64) as usize;
            if direction < 0 && name.starts_with('C') {
                octave += direction;
            }
            name = notes[index].clone();
            if direction > 0 && name.starts_with('C') {
                octave += direction;
            }
        }
        format!("{name}{octave}")
    }

    /// Expected values are pinned against strudel.cc.
    #[test]
    fn note_parsing_matches_tonaljs() {
        let c4 = note_get("c4").expect("c4");
        assert_eq!(c4.name, "C4");
        assert_eq!(c4.midi, Some(60));
        assert_eq!(c4.coord, (0, Some(4)));
        let ab = note_get("ab").expect("ab pitch class");
        assert_eq!(ab.name, "Ab");
        assert_eq!(ab.oct, None);
        assert_eq!(ab.coord, (-4, None));
        let fx2 = note_get("fx2").expect("double sharp via x");
        assert_eq!(fx2.name, "F##2");
        assert_eq!(fx2.midi, Some(43));
        assert!(note_get("h3").is_none());
        assert!(note_get("c3q").is_none());
    }

    #[test]
    fn interval_algebra_matches_tonaljs() {
        assert_eq!(interval_semitones("3M"), Some(4));
        assert_eq!(interval_semitones("-2M"), Some(-2));
        assert_eq!(interval_semitones("5d"), Some(6));
        assert_eq!(interval_semitones("P5"), Some(7)); // shorthand form
        assert_eq!(interval_from_semitones(-24), "-15P");
        assert_eq!(interval_from_semitones(7), "5P");
        assert_eq!(interval_from_semitones(6), "5d");
        assert_eq!(interval_add("3m", "-1P"), Some("3m".into()));
        assert_eq!(interval_add("5P", "8P"), Some("12P".into()));
        assert_eq!(interval_add("2M", "-8P"), Some("-7m".into()));
        assert_eq!(interval_subtract("3M", "3M"), Some("1P".into()));
        assert_eq!(interval_subtract("5P", "3M"), Some("3m".into()));
    }

    /// Small counts match tonal's fromSemitones; the i64 extremes, i64::MIN
    /// among them, name their interval instead of panicking in `abs` (debug)
    /// or indexing out of bounds through `MIN % 12 == -8` (release).
    #[test]
    fn interval_from_semitones_is_total() {
        for (semitones, name) in [
            (0, "1P"),
            (1, "2m"),
            (-1, "-2m"),
            (-6, "-5d"),
            (11, "7M"),
            (12, "8P"),
            (-13, "-9m"),
            (23, "14M"),
            (i64::MAX, "5380300354831952555P"),
            (i64::MIN + 1, "-5380300354831952555P"),
            (i64::MIN, "-5380300354831952556m"),
        ] {
            assert_eq!(interval_from_semitones(semitones), name, "{semitones}");
        }
    }

    #[test]
    fn transpose_matches_tonaljs() {
        assert_eq!(note_transpose("C3", "3M"), "E3");
        assert_eq!(note_transpose("Ab3", "-15P"), "Ab1");
        assert_eq!(note_transpose("Eb3", "-2M"), "Db3");
        assert_eq!(note_transpose("B3", "2m"), "C4");
        assert_eq!(note_transpose("Cb2", "8P"), "Cb3");
        // Pitch class stays a pitch class.
        assert_eq!(note_transpose("F#", "5P"), "C#");
        assert_eq!(note_transpose("nope", "5P"), "");
        assert_eq!(note_transpose("C3", "nope"), "");
        assert_eq!(note_enharmonic("F2", "E#"), "E#2");
        assert_eq!(note_enharmonic("B#3", "C"), "C4");
        assert_eq!(note_enharmonic("Db4", "C#"), "C#4");
        assert_eq!(note_enharmonic("C4", "D"), "");
    }

    #[test]
    fn scale_layer_matches_strudel_tonal() {
        let ab = get_scale("ab:major").expect("ab:major");
        assert_eq!(ab.tonic.as_deref(), Some("Ab"));
        assert_eq!(ab.notes, vec!["Ab", "Bb", "C", "Db", "Eb", "F", "G"]);
        assert_eq!(scale_step(0.0, "ab:major").unwrap(), "Ab3");
        assert_eq!(scale_step(2.0, "ab:major").unwrap(), "C4");
        assert_eq!(scale_step(-1.0, "ab:major").unwrap(), "G3");
        assert_eq!(scale_step(7.0, "ab:major").unwrap(), "Ab4");
        assert_eq!(scale_step(-8.0, "C4:minor").unwrap(), "Bb2");
        assert_eq!(scale_offset("C:major", 2.0, "E3").unwrap(), "G3");
        assert_eq!(scale_offset("C:major", -3.0, "C4").unwrap(), "G3");
        // Fractional offsets take ceil steps, not a truncating cast.
        assert_eq!(scale_offset("C:major", 1.1, "C4").unwrap(), "E4");
        assert_eq!(scale_offset("C:major", -1.1, "C4").unwrap(), "A3");
        // This previously performed one million loop iterations.
        assert_eq!(
            scale_offset("C:major", 1_000_000.0, "C4").unwrap(),
            "D142861"
        );
        assert!(scale_offset("C:major", f64::INFINITY, "C4").is_err());
        assert!(get_scale("wat").is_err());
        assert!(get_scale("c3").is_err()); // incomplete: bare note
    }

    #[test]
    fn constant_time_scale_offset_matches_the_pinned_walk() {
        for scale in ["C:major", "Ab:major", "F#:minor", "C:chromatic"] {
            let notes = get_scale(scale).expect("test scale").notes;
            for pitch_class in notes {
                let note = format!("{pitch_class}4");
                for offset in [-20.25, -8.0, -1.1, -0.25, 0.0, 0.25, 1.1, 8.0, 20.25] {
                    assert_eq!(
                        scale_offset(scale, offset, &note).unwrap(),
                        scale_offset_walk_reference(scale, offset, &note),
                        "{scale} {note} offset {offset}"
                    );
                }
            }
        }
    }
}
