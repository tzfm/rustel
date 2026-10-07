/*
xen.rs - the xen layer: xenharmonic scales, tunings, and edo scales
Adapted from Strudel packages/xen/xen.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! The xen layer: `xen`, `withBase`, `ftrans`, `tuning`, `tune`, and
//! `edoScale`. The layer matches strudel.cc wherever a score can observe the
//! result: fractional scale indices yield NaN frequencies, context values
//! stay string-typed, and `root` is a fixed-4 string.

use crate::ops::PatOps;
use crate::value::OrderedMap;
use crate::{Hap, Value, signal_query_error};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Float modulo wrapped into `[0, m)`.
fn js_mod(n: f64, m: f64) -> f64 {
    ((n % m) + m) % m
}

/// Trims a frequency to 10 significant digits.
fn trim_freq(freq: f64) -> f64 {
    if freq == 0.0 || !freq.is_finite() {
        return freq;
    }
    let magnitude = freq.abs().log10().floor();
    let decimals = (9.0 - magnitude).clamp(0.0, 16.0) as usize;
    format!("{freq:.decimals$}").parse().unwrap_or(freq)
}

/// `Number(value)` coercion for the value shapes a hap can carry.
///
/// An empty list is 0 and a single-element list coerces recursively;
/// anything richer stringifies comma-joined and lands on NaN.
pub fn js_number(value: &Value) -> f64 {
    match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::F64(n) => *n,
        Value::Str(s) => js_string_to_number(s),
        Value::List(items) => match items.len() {
            0 => 0.0,
            1 => js_number(&items[0]),
            _ => {
                // "1,2" is not a number.
                f64::NAN
            }
        },
        Value::Object(_)
        | Value::Pattern(_)
        | Value::Haps(_)
        | Value::JsValue(_)
        | Value::Function(_) => f64::NAN,
    }
}

/// String-to-number coercion: trimmed, empty is zero, radix prefixes
/// honored, everything else must parse whole.
fn js_string_to_number(s: &str) -> f64 {
    let trimmed = s.trim_matches(|c: char| {
        matches!(
            u32::from(c),
            0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0xFEFF
        ) || c.is_whitespace()
    });
    if trimmed.is_empty() {
        return 0.0;
    }
    let rest = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let (negative, unsigned) = match rest.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, rest),
    };
    let magnitude = if let Some(hex) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        i128::from_str_radix(hex, 16)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN)
    } else if let Some(oct) = unsigned
        .strip_prefix("0o")
        .or_else(|| unsigned.strip_prefix("0O"))
    {
        i128::from_str_radix(oct, 8)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN)
    } else if let Some(bin) = unsigned
        .strip_prefix("0b")
        .or_else(|| unsigned.strip_prefix("0B"))
    {
        i128::from_str_radix(bin, 2)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN)
    } else if unsigned == "Infinity" {
        f64::INFINITY
    } else {
        unsigned.parse::<f64>().unwrap_or(f64::NAN)
    };
    if negative { -magnitude } else { magnitude }
}

/// A number, a note name's MIDI value, or the query-fatal parse error.
fn parse_numeral(value: &Value) -> Result<f64, String> {
    let as_number = js_number(value);
    if !as_number.is_nan() {
        return Ok(as_number);
    }
    if let Value::Str(s) = value
        && crate::tonaljs::is_score_note(s)
    {
        return crate::util::note_to_midi(s, 3);
    }
    Err(format!(
        "cannot parse as numeral: \"{}\"",
        js_display(value)
    ))
}

/// Error-message display: strings bare, other values via [`Value::show`].
fn js_display(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        other => other.show(),
    }
}

fn is_js_object(value: &Value) -> bool {
    matches!(
        value,
        Value::Object(_) | Value::List(_) | Value::Pattern(_) | Value::JsValue(_)
    )
}

/// Splits an object value into the named control plus the remaining
/// entries. A list or nested pattern has no named entries, so both halves
/// come back empty.
fn destructure(value: &Value, key: &str) -> (Option<Value>, OrderedMap) {
    match value {
        Value::Object(map) => {
            let picked = map.get(key).cloned();
            let mut rest = OrderedMap::new();
            for (k, v) in map.iter() {
                if k != key {
                    rest.insert(k.to_string(), v.clone());
                }
            }
            (picked, rest)
        }
        _ => (None, OrderedMap::new()),
    }
}

const XEN_DEFAULT_BASE: f64 = 220.0;
const XEN_PRESETS: &[(&str, &[f64])] = &[(
    "12ji",
    &[
        1.0,
        16.0 / 15.0,
        9.0 / 8.0,
        6.0 / 5.0,
        5.0 / 4.0,
        4.0 / 3.0,
        45.0 / 32.0,
        3.0 / 2.0,
        8.0 / 5.0,
        5.0 / 3.0,
        16.0 / 9.0,
        15.0 / 8.0,
    ],
)];

/// The most divisions an edo name, or steps an `edoScale` sequence, may ask
/// for.
///
/// Both come from the score and size the tables built from them; 65_536 is
/// far past any tuning in use (1200edo is one step per cent).
const MAX_EDO_DIVISIONS: usize = 65_536;

/// The ratio table behind `"31edo"`-style names.
pub fn edo(name: &str) -> Result<Vec<f64>, String> {
    let digits = edo_digits(name).ok_or_else(|| format!("not an edo scale: \"{name}\""))?;
    // `edo_digits` proves these are DIGITS, not that they fit a usize:
    // "99999999999999999999edo" passed validation and then panicked here,
    // which ends the set rather than refusing the note.
    let divisions = digits
        .parse::<usize>()
        .map_err(|_| format!("edo scale \"{name}\" asks for more divisions than can be counted"))?;
    if divisions > MAX_EDO_DIVISIONS {
        return Err(format!(
            "edo scale \"{name}\" asks for {divisions} divisions, past the \
             {MAX_EDO_DIVISIONS} that can be built without exhausting memory"
        ));
    }
    Ok((0..divisions)
        .map(|index| 2f64.powf(index as f64 / divisions as f64))
        .collect())
}

fn edo_digits(name: &str) -> Option<&str> {
    let digits = name.strip_suffix("edo")?;
    (!digits.is_empty() && !digits.starts_with('0') && digits.chars().all(|ch| ch.is_ascii_digit()))
        .then_some(digits)
}

fn is_edo(name: &str) -> bool {
    edo_digits(name).is_some()
}

/// A named scale resolved to ratios, or a raw ratio list used as-is - then
/// EVERY path is multiplied by the 220 Hz default base unconditionally.
fn get_xen_scale(arg: &Value) -> Result<Vec<f64>, String> {
    let ratios = match arg {
        Value::List(items) => items.iter().filter_map(Value::as_f64).collect::<Vec<f64>>(),
        Value::Str(name) => {
            if is_edo(name) {
                edo(name)?
            } else if let Some((_, ratios)) = XEN_PRESETS.iter().find(|(preset, _)| preset == name)
            {
                ratios.to_vec()
            } else {
                let spec = crate::tune::ScaleSpec::Name(name.clone());
                if crate::tune::Tune::is_valid_scale(&spec) {
                    let mut tune = crate::tune::Tune::new();
                    tune.load_scale(&spec)?;
                    tune.scale
                } else {
                    return Err(format!("unknown scale name: \"{name}\""));
                }
            }
        }
        // Anything else fails with the verbatim scale.map TypeError.
        _ => return Err("scale.map is not a function".into()),
    };
    Ok(ratios
        .iter()
        .map(|ratio| ratio * XEN_DEFAULT_BASE)
        .collect())
}

/// Scale lookup with octave folding. Fractional offsets miss the table and
/// yield NaN - kept, not "fixed": scores can observe it.
fn xen_offset(scale: &[f64], offset: f64) -> f64 {
    if scale.is_empty() {
        return f64::NAN;
    }
    let index = js_mod(offset, scale.len() as f64);
    let octave = (offset / scale.len() as f64).floor();
    let ratio = if index.fract() == 0.0 && index >= 0.0 && (index as usize) < scale.len() {
        scale[index as usize]
    } else {
        f64::NAN
    };
    ratio * 2f64.powf(octave)
}

/// Aborts the query the way a thrown score error would.
fn query_error(message: impl FnOnce() -> String) -> Option<Hap> {
    signal_query_error(message);
    None
}

/// `xen(scaleNameOrRatios, pat)`: resolve each hap's `i` control to a
/// frequency in the given xenharmonic scale.
pub fn xen<P: PatOps>(pat: &P, arg: Value) -> P {
    let arg = materialized_arg(arg);
    let scale = match get_xen_scale(&arg) {
        Ok(scale) => scale,
        Err(message) => return query_error_pattern::<P>(move || message.clone()),
    };
    let edo_size: Option<f64> = match &arg {
        // The digit run of an "NNedo" name, coerced when read.
        Value::Str(name) if is_edo(name) => edo_digits(name).and_then(|d| d.parse().ok()),
        _ => None,
    };
    pat.map_pitch_haps_native(move |hap| {
        let hap = &*crate::combinators::materialized_hap(hap);
        if !is_js_object(&hap.value) {
            // A primitive hap value has no `i` control.
            return query_error(|| {
                "Expected hap to have control 'i' set, but received undefined, try wrapping input in i()".to_string()
            });
        }
        if matches!(hap.value, Value::Null) {
            return query_error(|| {
                "Cannot destructure property 'i' of 'hVal' as it is null.".to_string()
            });
        }
        let (step, others) = destructure(&hap.value, "i");
        let Some(step) = step else {
            // A missing `i` fails numeral parsing as "undefined".
            return query_error(|| {
                "cannot parse as numeral: \"undefined\"".to_string()
            });
        };
        let offset = match parse_numeral(&step) {
            Ok(offset) => offset,
            Err(message) => return query_error(move || message),
        };
        let freq = trim_freq(xen_offset(&scale, offset));
        let mut rest = others;
        rest.insert("freq".into(), Value::F64(freq));
        let value = Value::Object(rest);
        let mut out = hap.with_value(move |_| value.clone());
        if let Some(edo_size) = edo_size {
            out = out.with_edo_size_context(edo_size);
        }
        Some(out)
    })
}

/// Materialize an opaque script value so list/string dispatch sees the real shape.
fn materialized_arg(arg: Value) -> Value {
    if matches!(arg, Value::JsValue(_)) {
        crate::materialize_js_value(&arg)
    } else {
        arg
    }
}
fn query_error_pattern<P: PatOps>(message: impl FnOnce() -> String + Send + Sync + 'static) -> P {
    // Abort at query time: a bad argument fails the whole query, not one hap.
    let captured = message();
    let leaked: &'static str = Box::leak(captured.into_boxed_str());
    P::pat_query_error(leaked)
}

/// `withBase(base, pat)`: rescale `freq` from the 220 Hz default base, or
/// from an explicit `[base, originalBase]` pair.
pub fn with_base<P: PatOps>(pat: &P, arg: Value) -> P {
    let arg = materialized_arg(arg);
    let (base, original_base) = match &arg {
        Value::List(items) if !items.is_empty() => {
            let base = js_number(items.first().expect("non-empty"));
            let original = items.get(1).map(js_number);
            (base, original.unwrap_or(XEN_DEFAULT_BASE))
        }
        other => (js_number(other), XEN_DEFAULT_BASE),
    };
    pat.map_pitch_haps_native(move |hap| {
        let hap = &*crate::combinators::materialized_hap(hap);
        let is_object = is_js_object(&hap.value);
        let raw_freq = if is_object {
            destructure(&hap.value, "freq").0
        } else {
            Some(hap.value.clone())
        };
        // Falsy frequencies pass through untouched.
        let Some(raw_freq) = raw_freq.filter(|f| !is_falsy(f)) else {
            return Some(hap.clone());
        };
        let freq = js_number(&raw_freq) * base / original_base;
        let value = if is_object {
            let (_, _) = ((), ());
            let mut rest = match &hap.value {
                Value::Object(map) => {
                    let mut rest = OrderedMap::new();
                    for (k, v) in map.iter() {
                        rest.insert(k.to_string(), v.clone());
                    }
                    rest
                }
                _ => OrderedMap::new(),
            };
            rest.insert("freq".into(), Value::F64(freq));
            Value::Object(rest)
        } else {
            let mut map = OrderedMap::new();
            map.insert("freq".into(), Value::F64(freq));
            Value::Object(map)
        };
        Some(hap.with_value(move |_| value.clone()))
    })
}

/// Truthiness for the values `withBase` can see.
fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null => true,
        Value::Bool(b) => !*b,
        Value::F64(n) => *n == 0.0 || n.is_nan(),
        Value::Str(s) => s.is_empty(),
        _ => false,
    }
}

/// `ftrans(steps, pat)`: transpose `freq` by equal-division steps; the edo
/// size comes from the argument, the hap's context, or 12.
pub fn ftrans<P: PatOps>(pat: &P, arg: Value) -> P {
    let arg = materialized_arg(arg);
    // List form carries [steps, edoSize]; null and undefined both mean unset.
    let (num_steps, explicit_edo) = match &arg {
        Value::List(items) if !items.is_empty() => {
            let steps = js_number(items.first().expect("non-empty"));
            let edo = items
                .get(1)
                .filter(|v| !matches!(v, Value::Undefined | Value::Null))
                .map(js_number);
            (steps, edo)
        }
        other => (js_number(other), None),
    };
    pat.map_pitch_haps_native(move |hap| {
        let hap = &*crate::combinators::materialized_hap(hap);
        let is_object = is_js_object(&hap.value);
        if matches!(hap.value, Value::Null) {
            return query_error(|| {
                "Cannot destructure property 'freq' of 'hVal' as it is null.".to_string()
            });
        }
        let (raw_freq, others) = if is_object {
            destructure(&hap.value, "freq")
        } else {
            (Some(hap.value.clone()), OrderedMap::new())
        };
        let edo_size = explicit_edo
            .or_else(|| hap.edo_size_context())
            .unwrap_or(12.0);
        let raw_freq = raw_freq.unwrap_or(Value::Undefined);
        let freq = js_number(&raw_freq) * 2f64.powf(num_steps / edo_size);
        let freq = trim_freq(freq);
        let value = if is_object {
            let mut rest = others;
            rest.insert("freq".into(), Value::F64(freq));
            Value::Object(rest)
        } else {
            Value::F64(freq)
        };
        Some(
            hap.with_value(move |_| value.clone())
                .with_edo_size_context(edo_size),
        )
    })
}

/// `tuning(ratios, pat)`: index each hap's numeral straight into a ratio
/// table - no base scaling.
pub fn tuning<P: PatOps>(pat: &P, arg: Value) -> P {
    let arg = materialized_arg(arg);
    // Indexed directly, no base scaling; a non-list argument is an empty
    // table, which makes every lookup NaN.
    let ratios: Vec<f64> = match &arg {
        Value::List(items) => items.iter().map(js_number).collect(),
        _ => Vec::new(),
    };
    pat.map_haps_native(move |hap| {
        let hap = &*crate::combinators::materialized_hap(hap);
        let step = match parse_numeral(&hap.value) {
            Ok(step) => step,
            Err(message) => return query_error(move || message),
        };
        let frequency = xen_offset(&ratios, step);
        Some(hap.with_value(move |_| Value::F64(frequency)))
    })
}

/// `tune(scale, pat)`: resolve each hap's `i` step through the tune scale
/// engine ([`crate::tune`]).
pub fn tune<P: PatOps>(pat: &P, arg: Value) -> P {
    let arg = materialized_arg(arg);
    let spec_ok = match &arg {
        Value::List(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| matches!(item, Value::F64(n) if !n.is_nan()))
        }
        Value::Str(name) => {
            crate::tune::Tune::is_valid_scale(&crate::tune::ScaleSpec::Name(name.clone()))
        }
        _ => false,
    };
    if !spec_ok {
        let shown = js_display(&arg);
        return query_error_pattern::<P>(move || {
            format!(
                "not a valid tune.js scale name: \"{shown}\". See http://abbernie.github.io/tune/scales.html"
            )
        });
    }
    let mut tune_engine = crate::tune::Tune::new();
    let spec = match &arg {
        Value::Str(name) => crate::tune::ScaleSpec::Name(name.clone()),
        Value::List(items) => {
            crate::tune::ScaleSpec::Frequencies(items.iter().filter_map(Value::as_f64).collect())
        }
        _ => unreachable!("validated above"),
    };
    if let Err(message) = tune_engine.load_scale(&spec) {
        return query_error_pattern::<P>(move || message);
    }
    tune_engine.tonicize(1.0);
    let engine = Arc::new(tune_engine);
    pat.map_haps_native(move |hap| {
        let hap = &*crate::combinators::materialized_hap(hap);
        if !is_js_object(&hap.value) {
            return query_error(|| {
                "Expected hap to have control 'i' set, but received undefined, try wrapping input in i()".to_string()
            });
        }
        // A missing `i` flows into the arithmetic as NaN and stays NaN.
        let step = match &hap.value {
            Value::Object(map) => map.get("i").map(js_number).unwrap_or(f64::NAN),
            Value::List(_) | Value::Pattern(_) | Value::JsValue(_) => f64::NAN,
            _ => unreachable!("object check above"),
        };
        let frequency = tune_note_nan_safe(&engine, step);
        Some(hap.with_value(move |_| Value::F64(frequency)))
    })
}

/// NaN-safe note lookup: a non-finite step yields NaN without ever
/// reaching the table read.
fn tune_note_nan_safe(engine: &crate::tune::Tune, step: f64) -> f64 {
    if !step.is_finite() {
        return f64::NAN;
    }
    engine.note(step, None)
}

// -- edoScale ---------------------------------------------------------------

/// The immutable pitch tables one scale definition compiles to.
struct EdoPitches {
    /// Cumulative step counts per degree.
    divisions: Vec<f64>,
    /// Total steps across the sequence.
    edivisions: f64,
    /// Interval labels; index 0 is a hole and serializes as null.
    int_labels: Vec<Option<String>>,
    /// Per-octave degree frequencies, indexed [oct][deg+1].
    oct_deg_freqs: Vec<Vec<Option<f64>>>,
    /// Per-octave degree MIDI values, indexed [oct][deg+1].
    oct_deg_midis: Vec<Vec<Option<f64>>>,
    root_octave: f64,
    /// Root frequency as a fixed-4 STRING; scores can observe the type.
    base_freq: String,
}

/// The most definition text the edoScale cache keeps, in bytes. A key spells
/// out its whole sequence, so this bounds the steps cached as well.
const MAX_CACHED_EDO_KEY_BYTES: usize = 65_536;

/// The pitch tables built so far, by flattened definition.
#[derive(Default)]
struct EdoPitchesCache {
    tables: HashMap<String, Arc<EdoPitches>>,
    /// Total length of the keys in `tables`.
    key_bytes: usize,
}

impl EdoPitchesCache {
    /// The tables for `key`, built from `definition` on a miss. A key longer
    /// than [`MAX_CACHED_EDO_KEY_BYTES`] is built but not kept, and the cache
    /// empties itself before a new key would take it past that budget.
    fn get_or_build(
        &mut self,
        key: String,
        definition: &[Value],
    ) -> Result<Arc<EdoPitches>, String> {
        if let Some(pitches) = self.tables.get(&key) {
            return Ok(Arc::clone(pitches));
        }
        let pitches = Arc::new(build_edo_pitches(&key, definition)?);
        if key.len() > MAX_CACHED_EDO_KEY_BYTES {
            return Ok(pitches);
        }
        if self.key_bytes + key.len() > MAX_CACHED_EDO_KEY_BYTES {
            self.tables.clear();
            self.key_bytes = 0;
        }
        self.key_bytes += key.len();
        self.tables.insert(key, Arc::clone(&pitches));
        Ok(pitches)
    }
}

static EDO_PITCHES_CACHE: Lazy<Mutex<EdoPitchesCache>> = Lazy::new(Mutex::default);

/// Just-intonation ratio → interval-label table.
static RATIO_INTERVALS: Lazy<Vec<(f64, &'static str)>> = Lazy::new(|| {
    vec![
        (1.0, "P1"),
        (16.0 / 15.0, "m2"),
        (15.0 / 14.0, "A1"),
        (13.0 / 12.0, "t2"),
        (12.0 / 11.0, "N2"),
        (11.0 / 10.0, "n2"),
        (10.0 / 9.0, "T2"),
        (9.0 / 8.0, "M2"),
        (8.0 / 7.0, "S2"),
        (7.0 / 6.0, "s3"),
        (19.0 / 16.0, "o3"),
        (6.0 / 5.0, "m3"),
        (17.0 / 14.0, "t3"),
        (11.0 / 9.0, "n3"),
        (5.0 / 4.0, "M3"),
        (9.0 / 7.0, "S3"),
        (13.0 / 10.0, "d4"),
        (4.0 / 3.0, "P4"),
        (19.0 / 14.0, "N4"),
        (11.0 / 8.0, "n4"),
        (25.0 / 18.0, "a4"),
        (7.0 / 5.0, "sT"),
        (45.0 / 32.0, "A4"),
        (17.0 / 12.0, "d5"),
        (10.0 / 7.0, "ST"),
        (13.0 / 9.0, "t5"),
        (3.0 / 2.0, "P5"),
        (14.0 / 9.0, "s6"),
        (25.0 / 16.0, "a5"),
        (11.0 / 7.0, "A5"),
        (8.0 / 5.0, "m6"),
        (13.0 / 8.0, "N6"),
        (18.0 / 11.0, "n6"),
        (5.0 / 3.0, "M6"),
        (128.0 / 75.0, "d7"),
        (17.0 / 10.0, "T6"),
        (12.0 / 7.0, "S6"),
        (7.0 / 4.0, "s7"),
        (16.0 / 9.0, "m7"),
        (9.0 / 5.0, "g7"),
        (11.0 / 6.0, "n7"),
        (13.0 / 7.0, "N7"),
        (15.0 / 8.0, "M7"),
        (17.0 / 9.0, "T7"),
        (19.0 / 10.0, "d8"),
        (2.0, "P8"),
    ]
});

/// Closest table entry within 1% relative error.
fn nearest_interval(v: f64) -> Option<(f64, &'static str)> {
    let mut min = 1.0f64;
    let mut matched: Option<(f64, &'static str)> = None;
    for (ratio, label) in RATIO_INTERVALS.iter() {
        let diff = ((*ratio - v) / *ratio).abs();
        if diff < min {
            min = diff;
            matched = Some((*ratio, label));
        }
    }
    // `filter`, not `then_some`: `then_some` evaluates its argument before it
    // tests the condition, so an `expect` there panics when no interval is
    // within 100%. That is the case for a NaN ratio, and
    // `edoScale("C:LLsLLLs:0:0")` makes one from zero divisions.
    matched.filter(|_| min < 0.01)
}

fn fixed(value: f64, places: usize) -> f64 {
    format!("{value:.places$}").parse().unwrap_or(value)
}

fn build_edo_pitches(definition_key: &str, definition: &[Value]) -> Result<EdoPitches, String> {
    let field = |index: usize, what: &str| -> Result<Value, String> {
        definition.get(index).cloned().ok_or_else(|| {
            format!("Cannot destructure property '{what}' of 'undefined' as it is undefined.")
        })
    };
    let base_note = field(0, "base_note")?;
    let sequence = field(1, "sequence")?;
    let large = field(2, "large")?;
    let small = field(3, "small")?;

    let Value::Str(sequence) = &sequence else {
        return Err(format!(
            "Cannot read properties of undefined (reading '{}')",
            'L'
        ));
    };

    let large_steps = js_number(&large);
    let small_steps = js_number(&small);

    let length = sequence.chars().count();
    if length > MAX_EDO_DIVISIONS {
        return Err(format!(
            "edoScale sequence has {length} steps, past the {MAX_EDO_DIVISIONS} \
             that can be built without exhausting memory"
        ));
    }
    // L is large, M defaults to large, everything else small.
    let step_values: Vec<f64> = sequence
        .chars()
        .map(|ch| match ch {
            'L' => large_steps,
            'M' => large_steps, // medium defaults to large
            _ => small_steps,
        })
        .collect();
    let mut divisions = Vec::with_capacity(length);
    let mut running = 0.0f64;
    for value in &step_values {
        divisions.push(running);
        running += value;
    }
    let edivisions = running;

    // Step first, THEN record: index 0 stays a hole and serializes as null.
    let mut ratios = Vec::with_capacity(length + 1);
    ratios.push(1.0);
    let mut int_labels: Vec<Option<String>> = Vec::with_capacity(length + 1);
    int_labels.push(None);
    let mut division = 0.0f64;
    for value in &step_values {
        division += *value;
        let ratio = 2f64.powf(division / edivisions);
        ratios.push(ratio);
        int_labels.push(Some(
            nearest_interval(ratio).map_or(String::new(), |(_, label)| label.to_string()),
        ));
    }

    // Pitches over octaves 0..=8 from the definition's root note.
    let Value::Str(base_note) = &base_note else {
        return Err("Cannot read properties of undefined (reading 'octave')".to_string());
    };
    let token = crate::util::tokenize_note(base_note)
        .ok_or_else(|| format!("not a note: \"{base_note}\""))?;
    let root_octave = token.octave.unwrap_or(3.0);
    let midi_start = crate::util::note_to_midi(base_note, 3)?;
    let tuning = 440.0f64;
    let midi_to_hz = |n: f64| tuning * 2f64.powf((n - 69.0) / 12.0);
    let hz_to_midi = |freq: f64| 12.0 * (freq / tuning).log2() + 69.0;
    let base_freq_hz = midi_to_hz(midi_start);
    let tonic = 1.0;

    let get_freq = |oct: f64| {
        let mut f = base_freq_hz
            * if tonic == 0.0 {
                1.0
            } else {
                2f64.powf((tonic - 1.0) / edivisions)
            };
        if oct < root_octave {
            f /= 2f64.powf(root_octave - oct);
        } else if oct > root_octave {
            f *= 2f64.powf(oct - root_octave);
        }
        f
    };

    let mut oct_deg_freqs = Vec::with_capacity(9);
    let mut oct_deg_midis = Vec::with_capacity(9);
    for oct in 0..=8i64 {
        let mut freq_row = vec![None];
        let mut midi_row = vec![None];
        freq_row.resize(length + 1, None);
        midi_row.resize(length + 1, None);
        let f = get_freq(oct as f64);
        for deg in 0..length {
            // Degree deg reads ratios[deg] (unison at 0), stored at deg+1.
            let product = f * ratios[deg];
            freq_row[deg + 1] = Some(fixed(product, 3));
            midi_row[deg + 1] = Some(fixed(hz_to_midi(product), 4));
        }
        oct_deg_freqs.push(freq_row);
        oct_deg_midis.push(midi_row);
    }

    let _ = definition_key;
    Ok(EdoPitches {
        divisions,
        edivisions,
        int_labels,
        oct_deg_freqs,
        oct_deg_midis,
        root_octave,
        base_freq: format!("{base_freq_hz:.4}"),
    })
}

fn cached_pitches(key: String, definition: &[Value]) -> Result<Arc<EdoPitches>, String> {
    EDO_PITCHES_CACHE
        .lock()
        .expect("pitches cache")
        .get_or_build(key, definition)
}

/// Splits an arbitrary degree into octave + in-scale degree.
fn oct_deg(pitches: &EdoPitches, deg: f64) -> (f64, f64) {
    let len = pitches.divisions.len() as f64;
    let higher_octave = deg > len;
    let octave = if higher_octave {
        pitches.root_octave + ((deg - 1.0) / len).floor()
    } else {
        pitches.root_octave
    };
    let degree = if higher_octave {
        let rem = deg % len;
        if rem == 0.0 { len } else { rem }
    } else {
        deg
    };
    (octave, degree)
}

/// `edoScale(definition, pat)`: resolve each hap's `n` degree through a
/// `[root, sequence, large, small]` scale definition.
pub fn edo_scale<P: PatOps>(pat: &P, arg: Value) -> P {
    let arg = materialized_arg(arg);
    // Arrays only. A string definition fails with the verbatim flat()
    // TypeError rather than inventing string parsing that never existed.
    let definition: Vec<Value> = match &arg {
        Value::List(items) => items.clone(),
        _ => {
            return query_error_pattern::<P>(|| {
                "scaleDefinition.flat is not a function".to_string()
            });
        }
    };
    // Cache key: the flattened definition joined with ':'.
    let key = {
        let mut parts: Vec<String> = Vec::new();
        fn flatten(items: &[Value], out: &mut Vec<String>) {
            for item in items {
                match item {
                    Value::List(inner) => flatten(inner, out),
                    other => out.push(other.show()),
                }
            }
        }
        flatten(&definition, &mut parts);
        parts.join(":")
    };
    let pitches = match cached_pitches(key, &definition) {
        Ok(pitches) => pitches,
        Err(message) => return query_error_pattern::<P>(move || message),
    };

    let mapped = pat.outer_bind(move |value| {
        // A pure object argument can still arrive as an opaque script-owned
        // reference; materialize it so the named fields are readable.
        let owned;
        let value = if matches!(value, Value::JsValue(_)) {
            owned = crate::materialize_js_value(value);
            &owned
        } else {
            value
        };
        let is_object = is_js_object(value);
        let n_raw = if is_object {
            match value {
                Value::Object(map) => map.get("n").cloned(),
                _ => None,
            }
        } else {
            Some(value.clone())
        };
        let Some(n_raw) = n_raw else {
            // {n: undefined}: a missing n is a NaN degree.
            return <P as PatOps>::pat_pure(Value::F64(f64::NAN));
        };
        if matches!(&n_raw, Value::Str(s) if crate::tonaljs::is_score_note(s)) {
            // legacy: notes pass through as pure patterns
            return <P as PatOps>::pat_pure(n_raw.clone());
        }
        let degree_in = match &n_raw {
            Value::Str(s) => js_parse_int(s),
            other => {
                let n = js_number(other);
                if n.fract() == 0.0 && n.is_finite() {
                    n
                } else {
                    round_js(n)
                }
            }
        } + 1.0;
        let (octave, degree) = oct_deg(&pitches, degree_in);
        let oct_index = if octave.is_finite() && (0.0..=8.0).contains(&octave) {
            octave as usize
        } else {
            usize::MAX
        };
        let deg_index = if degree.is_finite() && degree >= 1.0 {
            degree
        } else {
            f64::NAN
        };
        // Two distinct misses, both visible in serialized output. A missing
        // octave row is an explicit null (`"freq": null`). A miss inside a
        // row (degree at or below zero, or past the end) is an absent key,
        // so `n("-8")` has no `freq` key at all.
        let lookup = |table: &[Vec<Option<f64>>]| -> Value {
            if oct_index == usize::MAX {
                return Value::Null;
            }
            if deg_index.is_nan() {
                return Value::Undefined;
            }
            let idx = deg_index as usize;
            table[oct_index]
                .get(idx)
                .copied()
                .flatten()
                .map_or(Value::Undefined, Value::F64)
        };
        let freq = lookup(&pitches.oct_deg_freqs);
        let note = lookup(&pitches.oct_deg_midis);
        if is_object {
            let mut rest = match value {
                Value::Object(map) => {
                    let mut rest = OrderedMap::new();
                    for (k, v) in map.iter() {
                        if k != "n" {
                            rest.insert(k.to_string(), v.clone());
                        }
                    }
                    rest
                }
                _ => OrderedMap::new(),
            };
            rest.insert("degree".into(), Value::F64(degree));
            rest.insert(
                "degreeIndexes".into(),
                Value::List(pitches.divisions.iter().map(|d| Value::F64(*d)).collect()),
            );
            rest.insert(
                "intLabels".into(),
                Value::List(
                    pitches
                        .int_labels
                        .iter()
                        .map(|l| l.clone().map_or(Value::Null, Value::Str))
                        .collect(),
                ),
            );
            rest.insert("root".into(), Value::Str(pitches.base_freq.clone()));
            // An undefined freq means no key at all.
            if !matches!(freq, Value::Undefined) {
                rest.insert("freq".into(), freq);
            }
            rest.insert("edo".into(), Value::F64(pitches.edivisions));
            <P as PatOps>::pat_pure(Value::Object(rest))
        } else {
            <P as PatOps>::pat_pure(note)
        }
    });

    let tag: Arc<Value> = Arc::new(arg.clone());
    mapped.map_haps_native(move |hap| {
        Some(hap.clone().with_scale_definition_context(Arc::clone(&tag)))
    })
}

/// Lenient integer-prefix parse: optional sign, digits until the first
/// non-digit, NaN when none.
fn js_parse_int(s: &str) -> f64 {
    let trimmed = s.trim_start();
    let bytes = trimmed.as_bytes();
    let mut end = 0usize;
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == 0 || (end == 1 && (bytes[0] == b'+' || bytes[0] == b'-')) {
        return f64::NAN;
    }
    trimmed[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// Rounding where half goes toward +infinity.
fn round_js(n: f64) -> f64 {
    (n + 0.5).floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edo_names_validate_like_the_regex() {
        assert_eq!(edo("12edo").unwrap().len(), 12);
        assert_eq!(edo("5edo").unwrap()[2], 2f64.powf(2.0 / 5.0));
        for invalid in ["edo", "0edo", "05edo", "-5edo", "5EDO", "5.0edo"] {
            assert!(edo(invalid).is_err(), "{invalid}");
        }
    }

    /// `nearest_interval` returns None when no interval is within 100%, as
    /// for the NaN ratio that `edoScale("C:LLsLLLs:0:0")` makes from zero
    /// divisions. It must not panic.
    #[test]
    fn a_ratio_with_no_near_interval_is_none_rather_than_a_panic() {
        assert_eq!(nearest_interval(f64::NAN), None);
        assert_eq!(nearest_interval(0.0), None);
        assert_eq!(nearest_interval(f64::INFINITY), None);
        assert_eq!(nearest_interval(-1.0), None);
        assert_eq!(nearest_interval(1e308), None);

        // A ratio that IS in the table still resolves, within the 1% window.
        assert_eq!(nearest_interval(1.5).map(|(_, label)| label), Some("P5"));
        assert_eq!(nearest_interval(2.0).map(|(_, label)| label), Some("P8"));
        // The table is dense enough that most plausible ratios land inside
        // the 1% window; what matters here is that a MISS is None rather than
        // a panic, which the non-finite and out-of-range cases above cover.
    }

    /// The division count comes from the SCORE, and this table is allocated
    /// from it. `xen("999999999edo")` asked for a billion f64s -- eight
    /// gigabytes -- and took a machine down; a longer digit run overflowed
    /// the parse and panicked outright, which ends the set rather than
    /// refusing the note.
    #[test]
    fn an_absurd_edo_is_refused_rather_than_allocated() {
        // Panicked before: `edo_digits` proves these are digits, not that
        // they fit a usize.
        assert!(edo("99999999999999999999edo").is_err());
        assert!(edo("999999999edo").is_err());
        let over = edo(&format!("{}edo", MAX_EDO_DIVISIONS + 1));
        assert!(over.is_err(), "one past the ceiling was built");

        // Everything a tuning actually uses is untouched, including the
        // ceiling itself.
        assert_eq!(edo("31edo").unwrap().len(), 31);
        assert_eq!(edo("1200edo").unwrap().len(), 1200);
        assert_eq!(
            edo(&format!("{MAX_EDO_DIVISIONS}edo")).unwrap().len(),
            MAX_EDO_DIVISIONS
        );
    }

    /// An `edoScale` sequence longer than [`MAX_EDO_DIVISIONS`] is refused, while
    /// one at the ceiling and an ordinary scale still build.
    #[test]
    fn an_absurd_edoscale_sequence_is_refused_rather_than_built() {
        let definition = |sequence: String| {
            vec![
                Value::Str("C3".into()),
                Value::Str(sequence),
                Value::F64(2.0),
                Value::F64(1.0),
            ]
        };
        let over = build_edo_pitches("over", &definition("L".repeat(MAX_EDO_DIVISIONS + 1)));
        assert!(over.is_err(), "one past the ceiling was built");

        assert_eq!(
            build_edo_pitches("at", &definition("L".repeat(MAX_EDO_DIVISIONS)))
                .unwrap()
                .divisions
                .len(),
            MAX_EDO_DIVISIONS
        );
        assert_eq!(
            build_edo_pitches_for_test("C:LLsLLLs:2:1").divisions.len(),
            7
        );
    }

    /// The edoScale cache holds at most [`MAX_CACHED_EDO_KEY_BYTES`] of keys
    /// however many distinct definitions pass through it, builds but does not
    /// keep a definition whose key alone is over that budget, and serves a repeat
    /// from what it holds.
    #[test]
    fn the_edoscale_cache_stays_within_its_key_budget() {
        let padding = "x".repeat(MAX_CACHED_EDO_KEY_BYTES / 8);
        let definition = |large: f64| {
            vec![
                Value::Str("C3".into()),
                Value::Str("LLsLLLs".into()),
                Value::F64(large),
                Value::F64(1.0),
                Value::Str(padding.clone()),
            ]
        };
        let key = |definition: &[Value]| {
            definition
                .iter()
                .map(Value::show)
                .collect::<Vec<_>>()
                .join(":")
        };
        let mut cache = EdoPitchesCache::default();
        for large in 2..40 {
            let definition = definition(f64::from(large));
            cache.get_or_build(key(&definition), &definition).unwrap();
            assert!(
                cache.key_bytes <= MAX_CACHED_EDO_KEY_BYTES,
                "the cache grew to {} key bytes",
                cache.key_bytes
            );
            assert_eq!(
                cache.key_bytes,
                cache.tables.keys().map(String::len).sum::<usize>()
            );
        }

        let repeat = definition(39.0);
        let first = cache.get_or_build(key(&repeat), &repeat).unwrap();
        let again = cache.get_or_build(key(&repeat), &repeat).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "a repeat was built again");

        let held = cache.key_bytes;
        let mut oversized = definition(40.0);
        oversized[4] = Value::Str("x".repeat(MAX_CACHED_EDO_KEY_BYTES));
        let built = cache.get_or_build(key(&oversized), &oversized).unwrap();
        assert_eq!(built.divisions.len(), 7);
        assert_eq!(
            cache.key_bytes, held,
            "a definition over the budget was kept"
        );
        assert!(cache.tables.contains_key(&key(&repeat)));
    }

    /// `i("0 8 18").xen("31edo")`: base 220, ratios scaled by it
    /// unconditionally.
    #[test]
    fn xen_scale_carries_the_220_base() {
        let scale = get_xen_scale(&Value::Str("31edo".into())).unwrap();
        assert_eq!(scale[0], 220.0);
        assert!((scale[8] - 220.0 * 2f64.powf(8.0 / 31.0)).abs() < 1e-9);
        // A raw ratio list is scaled by the base too.
        let raw = get_xen_scale(&Value::List(vec![Value::F64(1.0), Value::F64(2.0)])).unwrap();
        assert_eq!(raw, vec![220.0, 440.0]);
        // The '12ji' preset resolves through the table.
        assert_eq!(get_xen_scale(&Value::Str("12ji".into())).unwrap().len(), 12);
    }

    /// The 10-significant-digit frequency trim.
    #[test]
    fn trim_freq_matches_to_precision_ten() {
        assert_eq!(trim_freq(191.52112388531343), 191.5211239);
        assert_eq!(trim_freq(440.0), 440.0);
        assert_eq!(trim_freq(0.000123456789012), 0.000123456789);
    }

    #[test]
    fn js_number_tracks_number_coercions() {
        assert!(js_number(&Value::Undefined).is_nan());
        assert_eq!(js_number(&Value::Null), 0.0);
        assert_eq!(js_number(&Value::Bool(true)), 1.0);
        assert_eq!(js_number(&Value::Str(" 42 ".into())), 42.0);
        assert_eq!(js_number(&Value::Str("".into())), 0.0);
        assert_eq!(js_number(&Value::Str("0x10".into())), 16.0);
        assert_eq!(js_number(&Value::Str("-7".into())), -7.0);
        assert!(js_number(&Value::Str("12ed".into())).is_nan());
        assert_eq!(js_number(&Value::List(vec![Value::Str("5".into())])), 5.0);
        assert!(js_number(&Value::List(vec![Value::F64(1.0), Value::F64(2.0)])).is_nan());
    }

    /// Fractional offsets miss the table and stay NaN through the octave
    /// scaling.
    #[test]
    fn fractional_offsets_yield_nan_like_javascript() {
        let five_edo = edo("5edo").unwrap();
        let base = get_xen_scale(&Value::Str("5edo".into())).unwrap();
        assert!((xen_offset(&base, 0.0) - 220.0).abs() < 1e-9);
        assert_eq!(xen_offset(&base, 5.0), 440.0);
        assert!(xen_offset(&base, 0.5).freq_is_nan());
        let _ = five_edo;
    }

    #[test]
    fn tune_tiny_negative_step_yields_nan_for_named_and_inline_scales() {
        let pat = crate::pure(Value::Object({
            let mut controls = OrderedMap::new();
            controls.insert("i".into(), Value::F64(-f64::MIN_POSITIVE));
            controls
        }));
        for scale in [
            Value::Str("hexany15".into()),
            Value::Str("tranh3".into()),
            Value::List(vec![
                Value::F64(220.0),
                Value::F64(330.0),
                Value::F64(440.0),
            ]),
        ] {
            let haps = tune(&pat, scale.clone()).query_arc_sorted(
                rustel_fraction::Fraction::ZERO,
                rustel_fraction::Fraction::ONE,
            );
            assert_eq!(haps.len(), 1, "{scale:?}");
            assert!(
                matches!(&haps[0].value, Value::F64(freq) if freq.is_nan()),
                "tiny negative step should yield NaN for {scale:?}: {haps:?}"
            );
        }
    }

    trait FreqIsNan {
        fn freq_is_nan(self) -> bool;
    }
    impl FreqIsNan for f64 {
        fn freq_is_nan(self) -> bool {
            self.is_nan()
        }
    }

    #[test]
    fn oct_deg_splits_high_degrees_across_octaves() {
        let pitches = build_edo_pitches_for_test("C:LLsLLLs:2:1");
        // Degree 1 stays in the root octave; degree 8 of a 7-note scale is one up.
        let (oct, deg) = oct_deg(&pitches, 1.0);
        assert_eq!((oct, deg), (3.0, 1.0));
        let (oct, deg) = oct_deg(&pitches, 8.0);
        assert_eq!((oct, deg), (4.0, 1.0));
        // Exact multiples land on the top degree of the new octave.
        let (oct, deg) = oct_deg(&pitches, 7.0);
        assert_eq!((oct, deg), (3.0, 7.0));
    }

    /// C major as LLsLLLs:2:1 - degree 3 is E, whose 12-EDO ratio is exactly
    /// the just major third's neighborhood, so the nearest interval is M3.
    #[test]
    fn interval_labels_pick_the_nearest_just_ratio() {
        let pitches = build_edo_pitches_for_test("C:LLsLLLs:2:1");
        assert_eq!(pitches.edivisions, 12.0);
        assert_eq!(pitches.divisions[2], 4.0); // two L steps = 4 semitones
        assert_eq!(pitches.int_labels[2].as_deref(), Some("M3"));
        assert_eq!(pitches.int_labels[1].as_deref(), Some("M2"));
        assert!(pitches.int_labels[0].is_none());
    }

    fn build_edo_pitches_for_test(definition: &str) -> EdoPitches {
        let parts: Vec<&str> = definition.split(':').collect();
        let values = vec![
            Value::Str(parts[0].to_string()),
            Value::Str(parts[1].to_string()),
            Value::Str(parts[2].to_string()),
            Value::Str(parts[3].to_string()),
        ];
        build_edo_pitches(definition, &values).unwrap()
    }

    #[test]
    fn edoscale_end_to_end_matches_the_strudel_fixture() {
        let pat = crate::pure(Value::Object({
            let mut m = OrderedMap::new();
            m.insert("n".into(), Value::F64(2.0));
            m
        }));
        let arg = Value::List(vec![
            Value::Str("C".into()),
            Value::Str("LLsLLLs".into()),
            Value::F64(2.0),
            Value::F64(1.0),
        ]);
        let out = edo_scale(&pat, arg);
        let state = crate::State::new(crate::TimeSpan::new(
            rustel_fraction::Fraction::new(0, 1),
            rustel_fraction::Fraction::new(1, 1),
        ));
        let haps = out.query(&state);
        assert_eq!(haps.len(), 1, "haps: {haps:?}");
        match &haps[0].value {
            Value::Object(map) => {
                assert_eq!(map.get("degree"), Some(&Value::F64(3.0)));
                assert_eq!(map.get("freq"), Some(&Value::F64(164.814)));
            }
            other => panic!("unexpected value {other:?}"),
        }
    }
}
