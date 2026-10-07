/*
Voicing operations adapted from Strudel packages/tonal/voicings.mjs:
Copyright (C) 2022 Strudel contributors
Also adapts Strudel packages/tonal/tonleiter.mjs.

The chord-voicings helpers are by Felix Roos; see crates/core/LICENSE-chord-voicings.
See NOTICE.md for the embedded dictionaries' sources.

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Native ports of the pinned tonal voicing module.
//!
//! `voicing()` uses tonleiter's semitone dictionaries, while `voicings()`
//! retains chord-voicings' interval spellings and session-local voice-leading
//! state. Both resolve on the native per-hap path.
//!
//! Effective defaults are `renderVoicing`'s own (mode "below", anchor "c5",
//! octaves 1, offset 0): `voicing()`'s explicit spread overrides the
//! registry's per-dict mode/anchor with `undefined`, so only the DICTIONARY
//! is taken from the registry - faithfully replicated here.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::value::{OrderedMap, Value};

const PINNED_DICTS: &str = include_str!("../assets/voicing-dicts.json");
const PINNED_REGISTRY: &str = include_str!("../assets/voicing-registry.json");

struct Dictionaries {
    dicts: HashMap<String, HashMap<String, Vec<Vec<f64>>>>,
    pinned_default: String,
}

fn dictionaries() -> &'static Dictionaries {
    static DICTS: OnceLock<Dictionaries> = OnceLock::new();
    DICTS.get_or_init(|| {
        let parsed: serde_json::Value =
            serde_json::from_str(PINNED_DICTS).expect("pinned voicing-dicts.json parses");
        let mut dicts = HashMap::new();
        for (name, dict) in parsed["dicts"].as_object().expect("dicts object") {
            let mut symbols = HashMap::new();
            for (symbol, voicings) in dict.as_object().expect("dict object") {
                let voicings: Vec<Vec<f64>> = voicings
                    .as_array()
                    .expect("voicing list")
                    .iter()
                    .map(|voicing| {
                        voicing
                            .as_array()
                            .expect("semitone list")
                            .iter()
                            .map(|step| step.as_f64().expect("semitone number"))
                            .collect()
                    })
                    .collect();
                symbols.insert(symbol.clone(), voicings);
            }
            dicts.insert(name.clone(), symbols);
        }
        Dictionaries {
            dicts,
            pinned_default: parsed["default"]
                .as_str()
                .expect("default dict name")
                .to_owned(),
        }
    })
}

/// Whether a chord symbol resolves in a dictionary, the way `voicing()`
/// will resolve it: `Ok` for `Cm7` in a dictionary that lists `m7`, an
/// error naming the chord otherwise. `dictionary` names one; `None` uses
/// the current default. A score's own `addVoicings` registrations are
/// consulted first, as the renderer does.
/// The notes a chord is made of, as MIDI numbers, for anything that wants
/// to show or sound one: the dictionary's first voicing of the symbol,
/// laid on the root in the octave `root_octave` (3 puts middle C's octave
/// under the hand). The voicing's numbers are semitones above the root, so
/// a spread chord keeps its spread.
pub fn chord_notes(
    chord: &str,
    dictionary: Option<&str>,
    root_octave: i32,
) -> Result<Vec<f64>, String> {
    let Some((root, symbol)) = tokenize_chord(chord) else {
        return Err(format!(
            "unknown chord \"{chord}\": a chord starts with a root, A to G"
        ));
    };
    let name = dictionary.map(str::to_owned).unwrap_or_else(|| {
        crate::settings::default_voicings().unwrap_or_else(|| dictionaries().pinned_default.clone())
    });
    let voicing = match crate::settings::user_voicing_dict(&name) {
        Some(dict) => dict.get(&symbol).and_then(|shapes| shapes.first().cloned()),
        None => dictionaries()
            .dicts
            .get(&name)
            .ok_or_else(|| format!("unknown voicing dictionary \"{name}\""))?
            .get(&symbol)
            .and_then(|shapes| shapes.first().cloned()),
    };
    let Some(voicing) = voicing else {
        return Err(format!("unknown chord \"{chord}\" in the {name} voicings"));
    };
    let root_midi = crate::util::note_to_midi(&root, root_octave)?;
    Ok(voicing.iter().map(|offset| root_midi + offset).collect())
}

pub fn chord_lookup(chord: &str, dictionary: Option<&str>) -> Result<(), String> {
    let Some((_, symbol)) = tokenize_chord(chord) else {
        return Err(format!(
            "unknown chord \"{chord}\": a chord starts with a root, A to G"
        ));
    };
    let name = dictionary.map(str::to_owned).unwrap_or_else(|| {
        crate::settings::default_voicings().unwrap_or_else(|| dictionaries().pinned_default.clone())
    });
    let known = match crate::settings::user_voicing_dict(&name) {
        Some(dict) => dict.contains_key(&symbol),
        None => dictionaries()
            .dicts
            .get(&name)
            .ok_or_else(|| format!("unknown voicing dictionary \"{name}\""))?
            .contains_key(&symbol),
    };
    if known {
        Ok(())
    } else {
        Err(format!("unknown chord \"{chord}\" in the {name} voicings"))
    }
}

/// Every chord symbol any pinned dictionary knows, so a score that switches
/// dictionaries can be checked without knowing which one wins.
pub fn chord_symbol_in_any_dictionary(chord: &str) -> bool {
    let Some((_, symbol)) = tokenize_chord(chord) else {
        return false;
    };
    dictionaries()
        .dicts
        .values()
        .any(|dict| dict.contains_key(&symbol))
}

#[derive(Debug)]
pub(crate) struct LegacyVoicingDict {
    dictionary: LegacyDictionary,
    range: Vec<i64>,
}

type LegacyDictionary = HashMap<String, Vec<Vec<String>>>;

impl LegacyVoicingDict {
    pub(crate) fn retention_parts(&self) -> (&LegacyDictionary, &Vec<i64>) {
        (&self.dictionary, &self.range)
    }
}

type ParsedVoicingDictionaries = (crate::settings::VoicingDict, LegacyDictionary);

struct LegacyDictionaries {
    dicts: HashMap<String, std::sync::Arc<LegacyVoicingDict>>,
}

fn legacy_dictionaries() -> &'static LegacyDictionaries {
    static DICTS: OnceLock<LegacyDictionaries> = OnceLock::new();
    DICTS.get_or_init(|| {
        let parsed: serde_json::Value =
            serde_json::from_str(PINNED_REGISTRY).expect("pinned voicing-registry.json parses");
        let mut dicts = HashMap::new();
        for (name, entry) in parsed["registry"].as_object().expect("registry object") {
            let dictionary = parse_legacy_dictionary(
                entry["dictionary"]
                    .as_object()
                    .expect("registry dictionary object"),
            );
            let range = parse_legacy_range(entry.get("range"), default_legacy_range(false))
                .expect("pinned voicing range parses");
            dicts.insert(
                name.clone(),
                std::sync::Arc::new(LegacyVoicingDict { dictionary, range }),
            );
        }
        LegacyDictionaries { dicts }
    })
}

/// Exact pinned registry data used to create the JavaScript-visible
/// compatibility object without evaluating the former 114 KiB bundle.
#[doc(hidden)]
pub fn registry_json() -> &'static str {
    PINNED_REGISTRY
}

pub fn set_default_voicings(name: &str) {
    crate::settings::set_default_voicings(Some(name.to_owned()));
}

/// Select a host-owned dictionary for the lifetime of this settings state.
#[doc(hidden)]
pub fn set_default_voicings_with_lease(
    name: String,
    lease: crate::settings::VoicingDictionaryLease,
) {
    crate::settings::set_default_voicings_with_lease(name, lease);
}

pub fn reset_default_voicings() {
    crate::settings::set_default_voicings(None);
}

/// Restore voicing dictionary state for the current runtime.
pub fn reset_voicings() {
    crate::settings::reset_legacy_voicing_top_note();
    crate::settings::set_default_voicings(Some(dictionaries().pinned_default.clone()));
}

/// Return the currently selected registry name or host-private dictionary key.
#[doc(hidden)]
pub fn selected_default_voicings() -> String {
    crate::settings::default_voicings().unwrap_or_else(|| dictionaries().pinned_default.clone())
}

#[doc(hidden)]
pub fn selected_default_voicings_is_host_owned() -> bool {
    crate::settings::default_voicings_is_host_owned()
}

/// Refresh the bounded native view of the currently selected object-valued
/// default. The object itself remains rooted in QuickJS; this only replaces
/// the parsed view held by its settings snapshot.
#[doc(hidden)]
pub fn sync_host_default_json(name: &str, json: Option<&str>) -> Result<(), String> {
    let parsed = match json {
        Some(json) => Some(parse_user_dictionary_json("setDefaultVoicings", json)?.0),
        None => None,
    };
    let _ = crate::settings::sync_host_voicing_dict(name, parsed);
    Ok(())
}

const FLATS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];
const PCS: [&str; 12] = [
    "c", "db", "d", "eb", "e", "f", "gb", "g", "ab", "a", "bb", "b",
];

/// `pc2chroma` - pitch-class index plus accidentals; raw (may leave 0..11).
fn pc2chroma(pc: &str) -> Option<i64> {
    let mut chars = pc.chars();
    let letter = chars.next()?.to_ascii_lowercase().to_string();
    let index = PCS.iter().position(|entry| **entry == *letter)? as i64;
    let mut offset = 0i64;
    for accidental in chars {
        match accidental {
            '#' => offset += 1,
            'b' => offset -= 1,
            _ => return None,
        }
    }
    Some(index + offset)
}

/// `tokenizeChord` - `^([A-G][b#]*)([^/]*)[/]?([A-G][b#]*)?$`, first two.
fn tokenize_chord(chord: &str) -> Option<(String, String)> {
    let mut chars = chord.chars();
    let root_letter = chars.next()?;
    if !('A'..='G').contains(&root_letter) {
        return None;
    }
    let rest: String = chars.collect();
    let accidentals: String = rest
        .chars()
        .take_while(|c| *c == 'b' || *c == '#')
        .collect();
    let tail = &rest[accidentals.len()..];
    let symbol = match tail.split_once('/') {
        Some((symbol, _bass)) => symbol,
        None => tail,
    };
    Some((format!("{root_letter}{accidentals}"), symbol.to_owned()))
}

fn midi2note(midi: i64) -> String {
    let oct = midi.div_euclid(12) - 1;
    let pc = FLATS[midi.rem_euclid(12) as usize];
    format!("{pc}{oct}")
}

fn value_str(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

fn defined_control<'a>(map: &'a OrderedMap, name: &str) -> Option<&'a Value> {
    match map.get(name) {
        None | Some(Value::Undefined) => None,
        Some(value) => Some(value),
    }
}

fn control_number(map: &OrderedMap, name: &str, default: f64) -> f64 {
    defined_control(map, name)
        .map(crate::pick_js_number)
        .unwrap_or(default)
}

/// JavaScript evaluates `bestIndex + offset` before modulo. A string offset
/// therefore concatenates (1 + "1" → "11") while booleans/null add
/// numerically. Preserve that odd but observable distinction.
fn index_with_offset(best_index: usize, offset: Option<&Value>) -> f64 {
    let Some(offset) = offset else {
        return best_index as f64;
    };
    if matches!(offset, Value::Undefined) {
        return best_index as f64;
    }
    let best = crate::pick_js_number_string(best_index as f64);
    match offset {
        Value::Str(value) => crate::pick_js_string_number(&format!("{best}{value}")),
        Value::List(values) => {
            crate::pick_js_string_number(&format!("{best}{}", crate::pick_array_string(values)))
        }
        Value::Object(_)
        | Value::Function(_)
        | Value::Pattern(_)
        | Value::Haps(_)
        | Value::JsValue(_) => f64::NAN,
        Value::Undefined | Value::Null | Value::Bool(_) | Value::F64(_) => {
            best_index as f64 + crate::pick_js_number(offset)
        }
    }
}

fn exact_i64(value: f64) -> Option<i64> {
    const I64_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    (value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value < I64_MAX_EXCLUSIVE)
        .then_some(value as i64)
}

/// `x2midi(anchor?.note || anchor, 4)`.
fn anchor_midi(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Str(s)) => crate::util::note_to_midi(s, 4).ok(),
        Some(Value::F64(f)) => Some(*f),
        Some(Value::Object(map)) => anchor_midi(map.get("note")),
        _ => None,
    }
}

/// The notes of one voicing for a hap value, or None for strudel.cc's caught
/// throw (unknown chord → logger + silence).
pub fn render_voicing(map: &OrderedMap) -> Option<Vec<Value>> {
    let chord = value_str(map.get("chord"))?;
    // A dictionary is a registry NAME or a custom `{symbol: [voicings]}`
    // object (`.dict({'': ['0 4 7', …]})`); strudel.cc uses objects directly.
    let custom;
    let user;
    let live_cell;
    let live_guard;
    // Materialize before the match. A `.dict('name')` argument crosses the JS
    // boundary as a JsValue that holds a string. Without this step the raw
    // variant takes the object arm, where a string returns None, and every
    // named dictionary, built-in ones included, renders zero haps with no
    // error.
    let materialized;
    let selector = match map.get("dictionary") {
        Some(value @ Value::JsValue(_)) => {
            materialized = crate::materialize_js_value(value);
            Some(&materialized)
        }
        other => other,
    };
    let dictionary: &HashMap<String, Vec<Vec<f64>>> = match selector {
        Some(Value::Str(name)) => {
            // A score's own `addVoicings` registration wins over a pinned
            // name, exactly as strudel.cc's `Object.assign(voicingRegistry, …)`
            // lets a user shadow a built-in.
            match crate::settings::user_voicing_dict(name) {
                Some(dict) => {
                    user = dict;
                    &user
                }
                None => dictionaries().dicts.get(name)?,
            }
        }
        Some(Value::Object(object)) => {
            custom = parse_custom_dictionary(object)?;
            &custom
        }
        None | Some(Value::Undefined) => {
            if let Some(cell) = crate::settings::default_host_voicing_dict() {
                live_cell = cell;
                live_guard = live_cell
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                live_guard.as_ref()?
            } else {
                let name = crate::settings::default_voicings()
                    .unwrap_or_else(|| dictionaries().pinned_default.clone());
                match crate::settings::user_voicing_dict(&name) {
                    Some(dict) => {
                        user = dict;
                        &user
                    }
                    None => dictionaries().dicts.get(&name)?,
                }
            }
        }
        // JavaScript's destructuring default selects the global dictionary
        // only for missing/undefined. Explicit null and other non-dictionary
        // values reach the custom-dictionary path on strudel.cc, throw, and are
        // caught by `voicing()` as silence.
        Some(_) => return None,
    };

    let (root, symbol) = tokenize_chord(chord)?;
    let root_chroma = pc2chroma(&root)?;
    let anchor = match map.get("anchor") {
        None | Some(Value::Undefined) => 72.0, // c5
        value => anchor_midi(value)?,
    };
    if !anchor.is_finite() {
        return None;
    }
    let anchor_chroma = anchor % 12.0;
    let mode = match map.get("mode") {
        None | Some(Value::Undefined) => "below",
        Some(Value::Str(mode)) => mode.as_str(),
        _ => return None,
    };
    let offset = control_number(map, "offset", 0.0);
    let octaves = control_number(map, "octaves", 1.0);

    // strudel.cc has six explicit modes. Calling an absent modeTarget
    // throws and the public wrapper turns that into silence, so an unknown
    // spelling must not silently inherit `below`.
    let (target_first, mult, force_first, duck) = match mode {
        "below" => (false, 1.0f64, false, false),
        "duck" => (false, 1.0, false, true),
        "above" => (true, -1.0, false, false),
        "root" => (true, -1.0, true, false),
        "oldabove" => (true, 1.0, false, false),
        "oldroot" => (true, 1.0, true, false),
        _ => return None,
    };

    let voicings = dictionary.get(&symbol)?;
    if voicings.is_empty() {
        return None;
    }
    // `modeTarget`: below/duck use the last step; every other mode the first.
    let target_of = |voicing: &Vec<f64>| -> Option<f64> {
        if target_first {
            voicing.first().copied()
        } else {
            voicing.last().copied()
        }
    };

    let mut min_distance: Option<f64> = None;
    let mut best_index = 0usize;
    let mut chroma_diffs = Vec::with_capacity(voicings.len());
    for (index, voicing) in voicings.iter().enumerate() {
        let target_step = target_of(voicing)?;
        let raw = (anchor_chroma - target_step - root_chroma as f64) * mult;
        let distance = crate::util::modulo_f64(raw, 12.0);
        if min_distance.is_none_or(|current| distance < current) {
            min_distance = Some(distance);
            best_index = index;
        }
        chroma_diffs.push(distance * mult);
    }
    if force_first {
        best_index = 0;
    }

    let len = i64::try_from(voicings.len()).ok()?;
    let oct_diff = (offset / len as f64).ceil() * 12.0;
    let selected_index = exact_i64(crate::util::modulo_f64(
        index_with_offset(best_index, map.get("offset")),
        len as f64,
    ))?;
    if !(0..len).contains(&selected_index) {
        return None;
    }
    let selected_index = selected_index as usize;
    let voicing = &voicings[selected_index];
    let target_step = target_of(voicing)?;
    let anchor_midi_value = anchor - chroma_diffs[selected_index] + oct_diff;

    let voicing_midi: Vec<f64> = voicing
        .iter()
        .map(|step| anchor_midi_value - target_step + *step)
        .collect();

    let mut notes: Vec<Value> = Vec::with_capacity(voicing_midi.len());
    let mut selected_midis = Vec::with_capacity(voicing_midi.len());
    for midi in &voicing_midi {
        if duck && *midi == anchor {
            continue;
        }
        let midi = exact_i64(*midi)?;
        selected_midis.push(midi);
        notes.push(Value::Str(midi2note(midi)));
    }

    if let Some(n) = defined_control(map, "n").map(crate::pick_js_number) {
        // `scaleStep(notes, n, octaves)` over midi numbers → ONE number.
        if selected_midis.is_empty() {
            return Some(vec![Value::F64(f64::NAN)]);
        }
        let len = selected_midis.len() as f64;
        let index = crate::util::modulo_f64(n, len);
        let Some(index) = exact_i64(index).and_then(|index| usize::try_from(index).ok()) else {
            return Some(vec![Value::F64(f64::NAN)]);
        };
        let oct_offset = (n / len).floor() * octaves * 12.0;
        return Some(vec![Value::F64(selected_midis[index] as f64 + oct_offset)]);
    }
    Some(notes)
}

pub const MAX_USER_VOICING_JSON_BYTES: usize = 256 * 1024;
pub const MAX_USER_VOICING_NAME_BYTES: usize = 256;
pub const MAX_USER_VOICING_SYMBOLS: usize = 512;
pub const MAX_USER_VOICINGS_PER_SYMBOL: usize = 256;
pub const MAX_USER_VOICING_STEPS: usize = 128;
pub const MAX_USER_VOICING_TOTAL_STEPS: usize = 32_768;
pub const MAX_REGISTERED_USER_VOICING_DICTS: usize = 64;
pub const MAX_USER_VOICING_RANGE_POINTS: usize = 16;

/// Register a dictionary under a name, from JSON.
///
/// The host's `addVoicings`/`registerVoicings` bindings serialise their object
/// argument and pass it as one string. Registration runs at evaluation time,
/// where the cost of a JSON round-trip does not matter, and the value bridge
/// needs no new object-crossing path. Steps parse exactly as
/// [`parse_custom_dictionary`] parses them: numbers, numeric strings, and
/// interval names.
pub fn register_user_dict_json(name: &str, json: &str) -> Result<(), String> {
    register_user_dict_json_with_range(name, json, None, false)
}

/// Registration bridge shared by `addVoicings` and `registerVoicings`.
/// `addVoicings` has an F3-A4 default; the newer call lets chord-voicings use
/// its own D3-A4 default when `options.range` is absent.
#[doc(hidden)]
pub fn register_user_dict_json_with_range(
    name: &str,
    json: &str,
    range_json: Option<&str>,
    add_signature: bool,
) -> Result<(), String> {
    if name.len() > MAX_USER_VOICING_NAME_BYTES {
        return Err(format!(
            "addVoicings: dictionary name is {} bytes, above the {MAX_USER_VOICING_NAME_BYTES}-byte limit",
            name.len()
        ));
    }
    let (semantic, dictionary) = parse_user_dictionary_json(name, json)?;
    let range_value = match range_json {
        Some(json) => Some(
            serde_json::from_str(json)
                .map_err(|error| format!("addVoicings {name}: range: {error}"))?,
        ),
        None => None,
    };
    let range = parse_legacy_range(range_value.as_ref(), default_legacy_range(add_signature))?;
    crate::settings::register_voicing_dict(
        name.to_string(),
        std::sync::Arc::new(semantic),
        std::sync::Arc::new(LegacyVoicingDict { dictionary, range }),
    )
}

fn parse_user_dictionary_json(
    label: &str,
    json: &str,
) -> Result<ParsedVoicingDictionaries, String> {
    if json.len() > MAX_USER_VOICING_JSON_BYTES {
        return Err(format!(
            "addVoicings {label}: JSON is {} bytes, above the {MAX_USER_VOICING_JSON_BYTES}-byte limit",
            json.len()
        ));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("addVoicings {label}: {error}"))?;
    let serde_json::Value::Object(entries) = parsed else {
        return Err(format!(
            "addVoicings {label}: the dictionary must be an object of {{symbol: [voicings]}}"
        ));
    };
    if entries.len() > MAX_USER_VOICING_SYMBOLS {
        return Err(format!(
            "addVoicings {label}: {} chord symbols exceed the {MAX_USER_VOICING_SYMBOLS}-symbol limit",
            entries.len()
        ));
    }
    let step = |value: &serde_json::Value| -> Option<f64> {
        match value {
            serde_json::Value::Number(semitones) => semitones.as_f64(),
            serde_json::Value::String(step) => match step.parse::<f64>() {
                Ok(semitones) => Some(semitones),
                Err(_) => crate::tonaljs::interval_semitones(step).map(|s| s as f64),
            },
            _ => None,
        }
    };
    let mut dict = HashMap::new();
    let mut legacy = HashMap::new();
    let mut total_steps = 0usize;
    for (symbol, voicings) in &entries {
        let serde_json::Value::Array(voicings) = voicings else {
            return Err(format!(
                "addVoicings {label}: voicings for \"{symbol}\" must be an array"
            ));
        };
        if voicings.len() > MAX_USER_VOICINGS_PER_SYMBOL {
            return Err(format!(
                "addVoicings {label}: \"{symbol}\" has {} voicings, above the {MAX_USER_VOICINGS_PER_SYMBOL} limit",
                voicings.len()
            ));
        }
        let mut parsed = Vec::with_capacity(voicings.len());
        let mut legacy_voicings = Vec::with_capacity(voicings.len());
        let mut legacy_valid = true;
        for voicing in voicings {
            match voicing {
                serde_json::Value::String(text) => {
                    legacy_voicings.push(text.split(' ').map(ToOwned::to_owned).collect::<Vec<_>>())
                }
                _ => legacy_valid = false,
            }
            let steps: Option<Vec<f64>> = match voicing {
                serde_json::Value::String(text) => text
                    .split_whitespace()
                    .take(MAX_USER_VOICING_STEPS + 1)
                    .map(|token| match token.parse::<f64>() {
                        Ok(semitones) => Some(semitones),
                        Err(_) => crate::tonaljs::interval_semitones(token).map(|s| s as f64),
                    })
                    .collect(),
                serde_json::Value::Array(entries) if entries.len() <= MAX_USER_VOICING_STEPS => {
                    entries.iter().map(step).collect()
                }
                serde_json::Value::Array(_) => None,
                _ => None,
            };
            let Some(steps) = steps else {
                return Err(format!(
                    "addVoicings {label}: voicing {voicing} for \"{symbol}\" did not parse"
                ));
            };
            if steps.len() > MAX_USER_VOICING_STEPS {
                return Err(format!(
                    "addVoicings {label}: one voicing for \"{symbol}\" exceeds the {MAX_USER_VOICING_STEPS}-step limit"
                ));
            }
            total_steps = total_steps.saturating_add(steps.len());
            if total_steps > MAX_USER_VOICING_TOTAL_STEPS {
                return Err(format!(
                    "addVoicings {label}: total steps exceed the {MAX_USER_VOICING_TOTAL_STEPS} limit"
                ));
            }
            parsed.push(steps);
        }
        dict.insert(symbol.clone(), parsed);
        legacy.insert(
            symbol.clone(),
            if legacy_valid {
                legacy_voicings
            } else {
                Vec::new()
            },
        );
    }
    Ok((dict, legacy))
}

fn parse_legacy_dictionary(
    entries: &serde_json::Map<String, serde_json::Value>,
) -> LegacyDictionary {
    entries
        .iter()
        .map(|(symbol, voicings)| {
            let parsed = voicings
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|voicing| voicing.as_str())
                .map(|voicing| voicing.split(' ').map(ToOwned::to_owned).collect())
                .collect();
            (symbol.clone(), parsed)
        })
        .collect()
}

fn default_legacy_range(add_signature: bool) -> Vec<i64> {
    if add_signature {
        vec![53, 69] // F3-A4
    } else {
        vec![50, 69] // chord-voicings' D3-A4 default
    }
}

fn parse_legacy_range(
    value: Option<&serde_json::Value>,
    default: Vec<i64>,
) -> Result<Vec<i64>, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    let Some(points) = value.as_array() else {
        return Ok(Vec::new());
    };
    if points.len() > MAX_USER_VOICING_RANGE_POINTS {
        return Err(format!(
            "voicing range has {} points, above the {MAX_USER_VOICING_RANGE_POINTS}-point limit",
            points.len()
        ));
    }
    let mut range = Vec::with_capacity(points.len());
    for point in points {
        let midi = match point {
            serde_json::Value::String(note) => crate::tonaljs::note_get(note).and_then(|n| n.midi),
            serde_json::Value::Number(number) => {
                number.as_i64().filter(|midi| (0..=127).contains(midi))
            }
            _ => None,
        };
        let Some(midi) = midi else {
            return Ok(Vec::new());
        };
        range.push(midi);
    }
    Ok(range)
}

/// A user dictionary: `{symbol: ["0 4 7" | [0, 4, 7] | "1P 3M 5P", …]}`.
/// Steps parse as numbers first, then as interval names (`step2semitones`).
fn parse_custom_dictionary(object: &OrderedMap) -> Option<HashMap<String, Vec<Vec<f64>>>> {
    let step = |value: &Value| -> Option<f64> {
        match value {
            Value::F64(semitones) => Some(*semitones),
            Value::Str(step) => match step.parse::<f64>() {
                Ok(semitones) => Some(semitones),
                Err(_) => crate::tonaljs::interval_semitones(step).map(|s| s as f64),
            },
            _ => None,
        }
    };
    let mut dict = HashMap::new();
    for (symbol, voicings) in object.iter() {
        let Value::List(voicings) = voicings else {
            continue;
        };
        let mut parsed = Vec::with_capacity(voicings.len());
        for voicing in voicings {
            let steps: Option<Vec<f64>> = match voicing {
                Value::Str(text) => text
                    .split_whitespace()
                    .map(|token| match token.parse::<f64>() {
                        Ok(semitones) => Some(semitones),
                        Err(_) => crate::tonaljs::interval_semitones(token).map(|s| s as f64),
                    })
                    .collect(),
                Value::List(entries) => entries.iter().map(step).collect(),
                _ => None,
            };
            parsed.push(steps?);
        }
        dict.insert(symbol.to_string(), parsed);
    }
    Some(dict)
}

fn legacy_dictionary(name: &str) -> Option<std::sync::Arc<LegacyVoicingDict>> {
    crate::settings::user_legacy_voicing_dict(name).or_else(|| {
        legacy_dictionaries()
            .dicts
            .get(name)
            .map(std::sync::Arc::clone)
    })
}

fn chromatic_midis(range: &[i64]) -> Vec<i64> {
    let Some(first) = range.first().copied() else {
        return Vec::new();
    };
    let mut notes = vec![first];
    for target in range.iter().copied().skip(1) {
        let mut current = *notes.last().expect("range starts nonempty");
        let step = if target >= current { 1 } else { -1 };
        while current != target {
            current += step;
            notes.push(current);
        }
    }
    notes
}

/// Parse the actual last emitted string, not the picker's candidate pitch.
/// Empty results and notes without a valid MIDI value preserve the legacy
/// fallback to zero; the caller separately retains the presence of a result.
fn legacy_top_note_midi(notes: &[String]) -> i64 {
    notes
        .last()
        .and_then(|note| crate::tonaljs::note_get(note))
        .and_then(|note| note.midi)
        .unwrap_or(0)
}

/// Stream chord-voicings' candidate order and retain only the current winner.
/// The JavaScript package materialises the whole Cartesian product; streaming
/// is observationally identical for `minTopNoteDiff` and places a hard bound
/// on retained memory for user dictionaries.
fn best_legacy_voicing(
    chord: &str,
    entry: &LegacyVoicingDict,
    last_top_note: Option<i64>,
) -> Vec<String> {
    const MAX_WORK: usize = 1_000_000;

    let Some((tonic, symbol)) = tokenize_chord(chord) else {
        return Vec::new();
    };
    let Some(voicings) = entry.dictionary.get(&symbol) else {
        return Vec::new();
    };
    let Some(top_limit) = entry.range.get(1).copied() else {
        return Vec::new();
    };
    let range = chromatic_midis(&entry.range);
    let previous_top = last_top_note.unwrap_or(0);
    let has_previous = last_top_note.is_some();
    let mut best = Vec::new();
    let mut best_diff = i64::MAX;
    let mut work = 0usize;

    for voicing in voicings {
        let Some(first) = voicing.first() else {
            continue;
        };
        let Some(relative): Option<Vec<String>> = voicing
            .iter()
            .map(|interval| crate::tonaljs::interval_subtract(interval, first))
            .collect()
        else {
            continue;
        };
        let Some(top_semitones) = relative
            .last()
            .and_then(|interval| crate::tonaljs::interval_semitones(interval))
        else {
            continue;
        };
        let bottom_pc = crate::tonaljs::note_transpose(&tonic, first);
        let Some(bottom_chroma) = crate::tonaljs::note_get(&bottom_pc).map(|note| note.chroma)
        else {
            continue;
        };

        for start_midi in range.iter().copied() {
            work = work.saturating_add(relative.len());
            if work > MAX_WORK {
                return Vec::new();
            }
            if start_midi.rem_euclid(12) != bottom_chroma {
                continue;
            }
            let top_midi = start_midi.saturating_add(top_semitones);
            if top_midi > top_limit || !(0..=127).contains(&top_midi) {
                continue;
            }
            let diff = (previous_top - top_midi).abs();
            if has_previous && diff >= best_diff {
                continue;
            }
            let start = crate::tonaljs::note_enharmonic(&midi2note(start_midi), &bottom_pc);
            if start.is_empty() {
                continue;
            }
            let candidate: Vec<String> = relative
                .iter()
                .map(|interval| crate::tonaljs::note_transpose(&start, interval))
                .collect();
            if candidate.iter().any(String::is_empty) {
                continue;
            }
            if !has_previous {
                return candidate;
            }
            best_diff = diff;
            best = candidate;
        }
    }
    best
}

/// `voicings(dictionary)`: expand chord strings into note strings and preserve
/// its module-local previous voicing in the current runtime.
pub fn voicings<P: crate::ops::PatOps>(pat: &P, dictionary: Value) -> P {
    let dictionary = crate::materialize_js_value(&dictionary);
    let name = crate::pick_property_key(&dictionary);
    // The previous voicing is read and written across queries, so the same
    // query answers differently the second time: volatile, never cached.
    let voiced = pat.expand_haps_native(move |hap| {
        let chord = match crate::materialize_js_value(&hap.value) {
            Value::Str(chord) => chord,
            _ => return Vec::new(),
        };
        let Some(dictionary) = legacy_dictionary(&name) else {
            return Vec::new();
        };
        let notes = crate::settings::with_legacy_voicing_top_note(|top_note| {
            let notes = best_legacy_voicing(&chord, &dictionary, *top_note);
            *top_note = Some(legacy_top_note_midi(&notes));
            notes
        });
        notes
            .into_iter()
            .map(|note| {
                let mut out = hap.clone();
                out.value = Value::Str(note);
                out
            })
            .collect()
    });
    voiced.mark_volatile()
}

fn root_note(chord: &str) -> Option<&str> {
    let bytes = chord.as_bytes();
    if !matches!(bytes.first(), Some(b'a'..=b'g' | b'A'..=b'G')) {
        return None;
    }
    let end = if matches!(bytes.get(1), Some(b'b' | b'#')) {
        2
    } else {
        1
    };
    Some(&chord[..end])
}

/// `rootNotes(octave)`, including its distinction between a string hap and a
/// control object with a truthy `chord` property.
pub fn root_notes<P: crate::ops::PatOps>(pat: &P, octave: Value) -> P {
    let octave = crate::pick_property_key(&crate::materialize_js_value(&octave));
    pat.map_haps_native(move |hap| {
        let value = crate::materialize_js_value(&hap.value);
        let (chord, object_result) = match &value {
            Value::Str(chord) => (chord.as_str(), false),
            Value::Object(map) => match map.get("chord") {
                Some(Value::Str(chord)) if !chord.is_empty() => (chord.as_str(), true),
                Some(Value::JsValue(_)) => return None,
                _ => return None,
            },
            _ => return None,
        };
        let root = root_note(chord)?;
        let note = Value::Str(format!("{root}{octave}"));
        let mut out = hap.clone();
        out.value = if object_result {
            Value::Object(OrderedMap::from_entries([("note".into(), note)]))
        } else {
            note
        };
        Some(out)
    })
}

/// The voicing() combinator body: per hap, expand to one hap per note with
/// `{note, ...rest}` (rest = value minus the voicing controls), exactly
/// `stack(...notes).note().set(rest)` + outerJoin.
pub fn voicing<P: crate::ops::PatOps>(pat: &P) -> P {
    // A host-owned dictionary is refreshed in place at query entry, so the
    // answer can move without any settings snapshot changing: volatile.
    let voiced = pat.expand_pitch_haps_native(|hap| {
        // A plain string value is `{chord: value}`.
        let owned;
        let map: &OrderedMap = match &hap.value {
            Value::Str(chord) => {
                owned =
                    OrderedMap::from_entries([("chord".to_string(), Value::Str(chord.clone()))]);
                &owned
            }
            Value::Object(map) => map,
            Value::JsValue(_) => {
                owned = match crate::materialize_js_value(&hap.value) {
                    Value::Object(map) => map,
                    _ => return Vec::new(),
                };
                &owned
            }
            _ => return Vec::new(),
        };
        let Some(notes) = render_voicing(map) else {
            // Upstream: caught throw → `[voicing]: unknown chord` + silence.
            return Vec::new();
        };
        const VOICING_CONTROLS: [&str; 7] = [
            "dictionary",
            "chord",
            "anchor",
            "offset",
            "mode",
            "n",
            "octaves",
        ];
        let mut rest = OrderedMap::new();
        for (key, value) in map.iter() {
            if !VOICING_CONTROLS.contains(&key) {
                rest.insert(key.to_string(), value.clone());
            }
        }
        notes
            .into_iter()
            .map(|note| {
                let mut value = OrderedMap::from_entries([("note".to_string(), note)]);
                for (key, entry) in rest.iter() {
                    value.insert(key.to_string(), entry.clone());
                }
                let mut out = hap.clone();
                out.value = Value::Object(value);
                out
            })
            .collect()
    });
    voiced.mark_volatile()
}

/// Every chord symbol a dictionary knows, in the order it was written.
pub fn dictionary_symbols(dictionary: Option<&str>) -> Vec<String> {
    let name = dictionary.map(str::to_owned).unwrap_or_else(|| {
        crate::settings::default_voicings().unwrap_or_else(|| dictionaries().pinned_default.clone())
    });
    if let Some(dict) = crate::settings::user_voicing_dict(&name) {
        return dict.keys().cloned().collect();
    }
    dictionaries()
        .dicts
        .get(&name)
        .map(|dict| dict.keys().cloned().collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The chord catalogue's reference entries.
//
// One entry per symbol the voicing dictionaries know, documented where the
// dictionaries live. Names and lead-sheet spellings follow the curated
// CHORD_ORDER the studio panel lists in; the prose is this port's own.
// ---------------------------------------------------------------------------

use crate::reference::ReferenceEntry;

/// One entry per chord symbol, in [`CHORD_ORDER`] first and the rest of the
/// dictionary alphabet after - the same order the panel lists them.
pub static CHORD_REFERENCE: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "^",
        synonyms: &[],
        summary: "the major chord",
        description: "The plain major triad - root, major third, fifth; home base. On a lead sheet: C. The score spells the symbol after a root - chord(\"C^\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-",
        synonyms: &[],
        summary: "the minor chord",
        description: "The plain minor triad - root, minor third, fifth. On a lead sheet: Cm. The score spells the symbol after a root - chord(\"C-\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7",
        synonyms: &[],
        summary: "the dominant 7 chord",
        description: "A major triad with a minor seventh - the sound that pulls home; blues and functional harmony run on it. On a lead sheet: C7. The score spells the symbol after a root - chord(\"C7\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "^7",
        synonyms: &[],
        summary: "the major 7 chord",
        description: "A major triad with a major seventh - dreamy and floating; jazz ballads and neo-soul. On a lead sheet: Cmaj7. The score spells the symbol after a root - chord(\"C^7\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-7",
        synonyms: &[],
        summary: "the minor 7 chord",
        description: "A minor triad with a minor seventh - soft and mellow; the default colour of jazz and funk. On a lead sheet: Cm7. The score spells the symbol after a root - chord(\"C-7\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "sus",
        synonyms: &[],
        summary: "the suspended 4th chord",
        description: "The third swapped for a fourth - open, neither major nor minor, wanting to resolve. On a lead sheet: Csus4. The score spells the symbol after a root - chord(\"Csus\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "2",
        synonyms: &[],
        summary: "the suspended 2nd chord",
        description: "The third swapped for a second - airy and open. On a lead sheet: Csus2. The score spells the symbol after a root - chord(\"C2\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "5",
        synonyms: &[],
        summary: "the power (no third) chord",
        description: "Root and fifth only - the power chord, no colour either way. On a lead sheet: C5. The score spells the symbol after a root - chord(\"C5\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "add9",
        synonyms: &[],
        summary: "the added 9th chord",
        description: "A major triad with the ninth added and no seventh - shimmering pop colour. On a lead sheet: Cadd9. The score spells the symbol after a root - chord(\"Cadd9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "6",
        synonyms: &[],
        summary: "the major 6 chord",
        description: "A major triad with a sixth - sweet and settled. On a lead sheet: C6. The score spells the symbol after a root - chord(\"C6\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-6",
        synonyms: &[],
        summary: "the minor 6 chord",
        description: "A minor triad with a sixth - bittersweet. On a lead sheet: Cm6. The score spells the symbol after a root - chord(\"C-6\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "9",
        synonyms: &[],
        summary: "the dominant 9 chord",
        description: "A dominant seventh with the ninth - funk and jazz dominant, fuller than a plain 7. On a lead sheet: C9. The score spells the symbol after a root - chord(\"C9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "^9",
        synonyms: &[],
        summary: "the major 9 chord",
        description: "A major seventh with the ninth - lush. On a lead sheet: Cmaj9. The score spells the symbol after a root - chord(\"C^9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-9",
        synonyms: &[],
        summary: "the minor 9 chord",
        description: "A minor seventh with the ninth - the neo-soul colour. On a lead sheet: Cm9. The score spells the symbol after a root - chord(\"C-9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "69",
        synonyms: &[],
        summary: "the 6 add 9 chord",
        description: "A sixth chord with the ninth, no seventh - lush and settled. On a lead sheet: C6/9. The score spells the symbol after a root - chord(\"C69\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7sus",
        synonyms: &[],
        summary: "the dominant 7 suspended chord",
        description: "A dominant seventh with the third suspended to a fourth - the gospel dominant. On a lead sheet: C7sus4. The score spells the symbol after a root - chord(\"C7sus\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "11",
        synonyms: &[],
        summary: "the dominant 11 chord",
        description: "Stacked to the eleventh - a wide, open dominant. On a lead sheet: C11. The score spells the symbol after a root - chord(\"C11\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "13",
        synonyms: &[],
        summary: "the dominant 13 chord",
        description: "Stacked to the thirteenth - the widest plain dominant. On a lead sheet: C13. The score spells the symbol after a root - chord(\"C13\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "^13",
        synonyms: &[],
        summary: "the major 13 chord",
        description: "A major stack to the thirteenth - as lush as major gets. On a lead sheet: Cmaj13. The score spells the symbol after a root - chord(\"C^13\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-11",
        synonyms: &[],
        summary: "the minor 11 chord",
        description: "A minor stack to the eleventh - dark and wide. On a lead sheet: Cm11. The score spells the symbol after a root - chord(\"C-11\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "+",
        synonyms: &[],
        summary: "the augmented chord",
        description: "A triad with a raised fifth - restless, symmetric, going somewhere. Spelled Caug on a lead sheet; in a score write it after a root as chord(\"Caug\"), and voicing() spreads it into notes. (The bare + suffix does not parse in mini-notation, so use aug.)",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "o",
        synonyms: &[],
        summary: "the diminished chord",
        description: "Stacked minor thirds - tense and symmetric. On a lead sheet: Cdim. The score spells the symbol after a root - chord(\"Co\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "h",
        synonyms: &[],
        summary: "the half-diminished chord",
        description: "A diminished triad with a minor seventh - the ii of a minor ii-V. On a lead sheet: Cm7b5. The score spells the symbol after a root - chord(\"Ch\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "o7",
        synonyms: &[],
        summary: "the diminished 7 chord",
        description: "Stacked minor thirds all the way - fully diminished, maximum tension. On a lead sheet: Cdim7. The score spells the symbol after a root - chord(\"Co7\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "h7",
        synonyms: &[],
        summary: "the half-diminished 7 chord",
        description: "The half-diminished seventh spelled out - same chord the panel lists as h. On a lead sheet: Cm7b5. The score spells the symbol after a root - chord(\"Ch7\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-^7",
        synonyms: &[],
        summary: "the minor major 7 chord",
        description: "A minor triad with a major seventh - film-noir, the detective theme. On a lead sheet: CmMaj7. The score spells the symbol after a root - chord(\"C-^7\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9",
        synonyms: &[],
        summary: "the dominant 7 flat 9 chord",
        description: "A dominant with a flattened ninth - darker cadences, secondary dominants. On a lead sheet: C7b9. The score spells the symbol after a root - chord(\"C7b9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7#9",
        synonyms: &[],
        summary: "the dominant 7 sharp 9 chord",
        description: "A dominant with a sharpened ninth - the Hendrix chord. On a lead sheet: C7#9. The score spells the symbol after a root - chord(\"C7#9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7#11",
        synonyms: &[],
        summary: "the dominant 7 sharp 11 chord",
        description: "A dominant with a sharpened eleventh - lydian-dominant colour. On a lead sheet: C7#11. The score spells the symbol after a root - chord(\"C7#11\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b13",
        synonyms: &[],
        summary: "the dominant 7 flat 13 chord",
        description: "A dominant with a flattened thirteenth - bittersweet resolution pull. On a lead sheet: C7b13. The score spells the symbol after a root - chord(\"C7b13\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b5",
        synonyms: &[],
        summary: "the dominant 7 flat 5 chord",
        description: "A dominant with a flattened fifth - unsettled, half-diminished's cousin. On a lead sheet: C7b5. The score spells the symbol after a root - chord(\"C7b5\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "^7#11",
        synonyms: &[],
        summary: "the major 7 sharp 11 chord",
        description: "A major seventh with a sharpened eleventh - the lydian dream. On a lead sheet: Cmaj7#11. The score spells the symbol after a root - chord(\"C^7#11\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "^7#5",
        synonyms: &[],
        summary: "the major 7 sharp 5 chord",
        description: "A major seventh with a raised fifth - floating and strange. On a lead sheet: Cmaj7#5. The score spells the symbol after a root - chord(\"C^7#5\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-b6",
        synonyms: &[],
        summary: "the minor flat 6 chord",
        description: "A minor triad with a flattened sixth - the harmonic-minor colour. On a lead sheet: Cmb6. The score spells the symbol after a root - chord(\"C-b6\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-add9",
        synonyms: &[],
        summary: "the minor added 9th chord",
        description: "A minor triad with the ninth added and no seventh. On a lead sheet: Cmadd9. The score spells the symbol after a root - chord(\"C-add9\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-69",
        synonyms: &[],
        summary: "the minor 6 add 9 chord",
        description: "A minor sixth chord with the ninth. On a lead sheet: Cm6/9. The score spells the symbol after a root - chord(\"C-69\") - and voicing() spreads it into notes.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-#5",
        synonyms: &[],
        summary: "the minor sharp 5 chord",
        description: "A minor triad with a raised fifth - augmented's minor cousin. On a lead sheet: Cm#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-7b5",
        synonyms: &[],
        summary: "the half-diminished 7 chord",
        description: "The same half-diminished 7 chord the panel lists as h7 - the voicing dictionaries take both spellings. On a lead sheet: Cm7b5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-M7",
        synonyms: &[],
        summary: "the minor major 7 chord",
        description: "The same minor major 7 chord the panel lists as -^7 - the voicing dictionaries take both spellings. On a lead sheet: CmMaj7.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-M9",
        synonyms: &[],
        summary: "the minor major 9 chord",
        description: "The same minor major 9 chord the panel lists as -^9 - the voicing dictionaries take both spellings. On a lead sheet: CmMaj9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "-^9",
        synonyms: &[],
        summary: "the minor major 9 chord",
        description: "A minor triad with a major seventh and the ninth - the noir colour, extended. On a lead sheet: CmMaj9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "13#11",
        synonyms: &[],
        summary: "the dominant 13 sharp 11 chord",
        description: "A dominant stacked to the thirteenth with a sharp eleventh. On a lead sheet: C13#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "13#9",
        synonyms: &[],
        summary: "the dominant 13 sharp 9 chord",
        description: "A dominant stacked to the thirteenth with a sharp ninth. On a lead sheet: C13#9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "13b9",
        synonyms: &[],
        summary: "the dominant 13 flat 9 chord",
        description: "A dominant stacked to the thirteenth with a flat ninth. On a lead sheet: C13b9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "13sus",
        synonyms: &[],
        summary: "the dominant 13 suspended chord",
        description: "A suspended dominant stacked to the thirteenth. On a lead sheet: C13sus.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7#5",
        synonyms: &[],
        summary: "the dominant 7 sharp 5 chord",
        description: "A dominant with a raised fifth - the whole-tone dominant. On a lead sheet: C7#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7#9#11",
        synonyms: &[],
        summary: "the dominant 7 sharp 9 sharp 11 chord",
        description: "A dominant stacked with a sharp ninth and sharp eleventh. On a lead sheet: C7#9#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7#9#5",
        synonyms: &[],
        summary: "the dominant 7 sharp 9 sharp 5 chord",
        description: "A dominant with both the fifth and ninth raised. On a lead sheet: C7#9#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7#9b5",
        synonyms: &[],
        summary: "the dominant 7 sharp 9 flat 5 chord",
        description: "A dominant with a sharp ninth and a flat fifth. On a lead sheet: C7#9b5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7alt",
        synonyms: &[],
        summary: "the altered dominant chord",
        description: "The altered dominant - a shorthand for the tensions of the altered scale (flat or sharp nine, sharp eleven, flat thirteen); the voicing dictionary picks a spelling of them. On a lead sheet: C7alt.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b13sus",
        synonyms: &[],
        summary: "the dominant 7 flat 13 suspended chord",
        description: "A suspended dominant with a flat thirteenth - the minor-key dominant suspended. On a lead sheet: C7b13sus.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9#11",
        synonyms: &[],
        summary: "the dominant 7 flat 9 sharp 11 chord",
        description: "A dominant with a flat ninth and a sharp eleventh. On a lead sheet: C7b9#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9#5",
        synonyms: &[],
        summary: "the dominant 7 flat 9 sharp 5 chord",
        description: "A dominant with a flat ninth and a raised fifth. On a lead sheet: C7b9#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9#9",
        synonyms: &[],
        summary: "the dominant 7 flat 9 sharp 9 chord",
        description: "A dominant carrying both ninths, flat and sharp, at once. On a lead sheet: C7b9#9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9b13",
        synonyms: &[],
        summary: "the dominant 7 flat 9 flat 13 chord",
        description: "A dominant with a flat ninth and flat thirteenth - the darkest plain cadence. On a lead sheet: C7b9b13.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9b5",
        synonyms: &[],
        summary: "the dominant 7 flat 9 flat 5 chord",
        description: "A dominant with a flat ninth and flat fifth. On a lead sheet: C7b9b5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7b9sus",
        synonyms: &[],
        summary: "the dominant 7 flat 9 suspended chord",
        description: "A suspended dominant with a flat ninth - phrygian cadence colour. On a lead sheet: C7b9sus.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "7susadd3",
        synonyms: &[],
        summary: "the dominant 7 suspended, added 3rd chord",
        description: "A suspended dominant that keeps the third too - quartal colour over a dominant. On a lead sheet: C7sus4(add3).",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "9#11",
        synonyms: &[],
        summary: "the dominant 9 sharp 11 chord",
        description: "A dominant ninth with a sharp eleventh - lydian dominant, extended. On a lead sheet: C9#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "9#5",
        synonyms: &[],
        summary: "the dominant 9 sharp 5 chord",
        description: "A dominant ninth with a raised fifth. On a lead sheet: C9#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "9b5",
        synonyms: &[],
        summary: "the dominant 9 flat 5 chord",
        description: "A dominant ninth with a flat fifth. On a lead sheet: C9b5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "9sus",
        synonyms: &[],
        summary: "the dominant 9 suspended chord",
        description: "A suspended dominant with the ninth - the floating sus sound. On a lead sheet: C9sus.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M",
        synonyms: &[],
        summary: "the major chord",
        description: "The same major chord the panel lists as ^ - the voicing dictionaries take both spellings. On a lead sheet: C.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M13",
        synonyms: &[],
        summary: "the major 13 chord",
        description: "The same major 13 chord the panel lists as ^13 - the voicing dictionaries take both spellings. On a lead sheet: Cmaj13.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M7",
        synonyms: &[],
        summary: "the major 7 chord",
        description: "The same major 7 chord the panel lists as ^7 - the voicing dictionaries take both spellings. On a lead sheet: Cmaj7.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M7#11",
        synonyms: &[],
        summary: "the major 7 sharp 11 chord",
        description: "The same major 7 sharp 11 chord the panel lists as ^7#11 - the voicing dictionaries take both spellings. On a lead sheet: Cmaj7#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M7#5",
        synonyms: &[],
        summary: "the major 7 sharp 5 chord",
        description: "The same major 7 sharp 5 chord the panel lists as ^7#5 - the voicing dictionaries take both spellings. On a lead sheet: Cmaj7#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M9",
        synonyms: &[],
        summary: "the major 9 chord",
        description: "The same major 9 chord the panel lists as ^9 - the voicing dictionaries take both spellings. On a lead sheet: Cmaj9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "M9#11",
        synonyms: &[],
        summary: "the major 9 sharp 11 chord",
        description: "The same major 9 sharp 11 chord the panel lists as ^9#11 - the voicing dictionaries take both spellings. On a lead sheet: Cmaj9#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "^9#11",
        synonyms: &[],
        summary: "the major 9 sharp 11 chord",
        description: "A major ninth with a sharp eleventh - the lydian dream, extended. On a lead sheet: Cmaj9#11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "aug",
        synonyms: &[],
        summary: "the augmented chord",
        description: "The same augmented chord the panel lists as + - the voicing dictionaries take both spellings. On a lead sheet: Caug.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "h9",
        synonyms: &[],
        summary: "the half-diminished 9 chord",
        description: "A half-diminished chord with the ninth added. On a lead sheet: Cm9b5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m",
        synonyms: &[],
        summary: "the minor chord",
        description: "The same minor chord the panel lists as - - the voicing dictionaries take both spellings. On a lead sheet: Cm.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m#5",
        synonyms: &[],
        summary: "the minor sharp 5 chord",
        description: "The same minor sharp 5 chord the panel lists as -#5 - the voicing dictionaries take both spellings. On a lead sheet: Cm#5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m11",
        synonyms: &[],
        summary: "the minor 11 chord",
        description: "The same minor 11 chord the panel lists as -11 - the voicing dictionaries take both spellings. On a lead sheet: Cm11.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m6",
        synonyms: &[],
        summary: "the minor 6 chord",
        description: "The same minor 6 chord the panel lists as -6 - the voicing dictionaries take both spellings. On a lead sheet: Cm6.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m69",
        synonyms: &[],
        summary: "the minor 6 add 9 chord",
        description: "The same minor 6 add 9 chord the panel lists as -69 - the voicing dictionaries take both spellings. On a lead sheet: Cm6/9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m7",
        synonyms: &[],
        summary: "the minor 7 chord",
        description: "The same minor 7 chord the panel lists as -7 - the voicing dictionaries take both spellings. On a lead sheet: Cm7.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m7b5",
        synonyms: &[],
        summary: "the half-diminished 7 chord",
        description: "The same half-diminished 7 chord the panel lists as h7 - the voicing dictionaries take both spellings. On a lead sheet: Cm7b5.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m9",
        synonyms: &[],
        summary: "the minor 9 chord",
        description: "The same minor 9 chord the panel lists as -9 - the voicing dictionaries take both spellings. On a lead sheet: Cm9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "mM7",
        synonyms: &[],
        summary: "the minor major 7 chord",
        description: "The same minor major 7 chord the panel lists as -^7 - the voicing dictionaries take both spellings. On a lead sheet: CmMaj7.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m^7",
        synonyms: &[],
        summary: "the minor major 7 chord",
        description: "The same minor major 7 chord the panel lists as -^7 - the voicing dictionaries take both spellings. On a lead sheet: CmMaj7.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "m^9",
        synonyms: &[],
        summary: "the minor major 9 chord",
        description: "The same minor major 9 chord the panel lists as -^9 - the voicing dictionaries take both spellings. On a lead sheet: CmMaj9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "madd9",
        synonyms: &[],
        summary: "the minor added 9th chord",
        description: "The same minor added 9th chord the panel lists as -add9 - the voicing dictionaries take both spellings. On a lead sheet: Cmadd9.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "mb6",
        synonyms: &[],
        summary: "the minor flat 6 chord",
        description: "The same minor flat 6 chord the panel lists as -b6 - the voicing dictionaries take both spellings. On a lead sheet: Cmb6.",
        params: &[],
        examples: &[],
        tags: &["chord"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn controls(chord: &str, anchor: &str, mode: &str) -> OrderedMap {
        OrderedMap::from_entries([
            ("chord".into(), Value::Str(chord.into())),
            ("anchor".into(), Value::Str(anchor.into())),
            ("mode".into(), Value::Str(mode.into())),
        ])
    }

    fn c7() -> OrderedMap {
        controls("C7", "C5", "below")
    }

    fn rendered_notes(controls: &OrderedMap) -> Vec<String> {
        render_voicing(controls)
            .expect("voicing")
            .into_iter()
            .map(|value| match value {
                Value::Str(note) => note,
                other => panic!("expected note, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn voicings_picker_matches_chord_voicings_and_reset_shape() {
        let entry = LegacyVoicingDict {
            dictionary: HashMap::from([(
                String::new(),
                vec![
                    ["1P", "3M", "5P"].map(str::to_owned).to_vec(),
                    ["3M", "5P", "8P"].map(str::to_owned).to_vec(),
                ],
            )]),
            range: vec![48, 72],
        };
        let first = best_legacy_voicing("C", &entry, None);
        assert_eq!(first, ["C3", "E3", "G3"]);
        let g = best_legacy_voicing("G", &entry, Some(legacy_top_note_midi(&first)));
        assert_eq!(g, ["G3", "B3", "D4"]);
        let led = best_legacy_voicing("C", &entry, Some(legacy_top_note_midi(&g)));
        assert_eq!(led, ["E3", "G3", "C4"]);
        assert_eq!(best_legacy_voicing("C", &entry, None), first);
    }

    fn legacy_history_settings() -> crate::settings::RuntimeSettings {
        let settings = crate::settings::RuntimeSettings::default();
        settings.with(|| {
            register_user_dict_json_with_range(
                "history",
                r#"{"": ["3M 5P 8P", "1P 3M 5P"], "empty": []}"#,
                Some(r#"["C3", "C5"]"#),
                false,
            )
            .expect("native history dictionary");
        });
        settings
    }

    fn legacy_history_notes(value: Value, dictionary: &str) -> Vec<String> {
        voicings(&crate::pure(value), Value::Str(dictionary.to_owned()))
            .query_arc(
                rustel_fraction::Fraction::ZERO,
                rustel_fraction::Fraction::ONE,
            )
            .into_iter()
            .map(|hap| match hap.value {
                Value::Str(note) => note,
                other => panic!("expected emitted note string, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn legacy_history_empty_and_invalid_results_are_not_fresh_or_reset() {
        for chord in ["not-a-chord", "Cmissing", "Cempty"] {
            let settings = legacy_history_settings();
            settings.with(|| {
                assert_eq!(
                    legacy_history_notes(Value::Str("C".into()), "history"),
                    ["E3", "G3", "C4"],
                    "fresh history uses the first candidate, not the lowest top"
                );
                assert!(legacy_history_notes(Value::Str(chord.into()), "history").is_empty());
                assert_eq!(
                    legacy_history_notes(Value::Str("C".into()), "history"),
                    ["C3", "E3", "G3"],
                    "an empty result is previous history with MIDI-zero fallback"
                );
                reset_voicings();
                assert_eq!(
                    legacy_history_notes(Value::Str("C".into()), "history"),
                    ["E3", "G3", "C4"],
                    "reset clears history but retains registered dictionaries"
                );
            });
        }
    }

    #[test]
    fn legacy_history_non_string_and_unknown_dictionary_leave_previous_notes_unchanged() {
        for (value, dictionary) in [
            (Value::F64(1.0), "history"),
            (Value::Str("C".into()), "not-registered"),
        ] {
            let settings = legacy_history_settings();
            settings.with(|| {
                assert_eq!(
                    legacy_history_notes(Value::Str("C".into()), "history"),
                    ["E3", "G3", "C4"]
                );
                assert_eq!(
                    legacy_history_notes(Value::Str("G".into()), "history"),
                    ["G3", "B3", "D4"]
                );
                assert!(legacy_history_notes(value, dictionary).is_empty());
                assert_eq!(
                    legacy_history_notes(Value::Str("A".into()), "history"),
                    ["A3", "C#4", "E4"],
                    "both fresh history and MIDI-zero history would choose C#3/E3/A3"
                );
            });
        }
    }

    #[test]
    fn legacy_history_picker_reads_last_string_and_preserves_midi_zero_fallback() {
        let settings = legacy_history_settings();
        settings.with(|| {
            let entry = legacy_dictionary("history").expect("native dictionary");
            for previous in [
                vec![],
                vec!["not-a-note".to_owned()],
                vec!["C".to_owned()],
                vec!["C99".to_owned()],
            ] {
                let previous_top = legacy_top_note_midi(&previous);
                assert_eq!(previous_top, 0);
                assert_eq!(
                    best_legacy_voicing("C", &entry, Some(previous_top)),
                    ["C3", "E3", "G3"]
                );
            }
            for previous in [
                vec!["C4".to_owned()],
                vec!["C9".to_owned(), "C4".to_owned()],
            ] {
                let previous_top = legacy_top_note_midi(&previous);
                assert_eq!(previous_top, 60);
                assert_eq!(
                    best_legacy_voicing("C", &entry, Some(previous_top)),
                    ["E3", "G3", "C4"]
                );
            }
            assert_eq!(best_legacy_voicing("C", &entry, None), ["E3", "G3", "C4"]);
        });
    }

    #[test]
    fn legacy_history_wide_emission_retains_only_the_shared_top_note() {
        let settings = crate::settings::RuntimeSettings::default();
        let intervals = ["1P"; MAX_USER_VOICING_STEPS].join(" ");
        let dictionary = serde_json::json!({"": [intervals]}).to_string();
        settings.with(|| {
            register_user_dict_json_with_range("wide", &dictionary, Some(r#"["C3", "C5"]"#), false)
                .expect("wide native dictionary at the step limit");
        });
        let snapshot = settings.detached_snapshot();
        let charged_before = snapshot.retained_snapshot_bytes(1024 * 1024).unwrap();
        let emitted = settings.with(|| legacy_history_notes(Value::Str("C".into()), "wide"));
        assert_eq!(emitted.len(), MAX_USER_VOICING_STEPS);
        assert!(emitted.iter().all(|note| note == "C3"));
        let (retained_top, inline_bytes) = snapshot.with(|| {
            crate::settings::with_legacy_voicing_top_note(|top_note| {
                let retained: &Option<i64> = top_note;
                (*retained, std::mem::size_of_val(retained))
            })
        });
        assert_eq!(retained_top, Some(48));
        assert_eq!(
            snapshot.retained_snapshot_bytes(charged_before),
            Some(charged_before)
        );
        // The typed Option<i64> history owns no variable-size heap payload.
        // Its shared Arc/mutex allocation is charged above. This does not
        // measure output haps, temporary picker allocations, or process RSS.
        eprintln!(
            "{}",
            serde_json::json!({
                "observation": "legacy-history-retained-scalar",
                "emitted_notes": emitted.len(), "top_note_midi": retained_top,
                "inline_value_bytes": inline_bytes, "history_heap_bytes": 0,
            })
        );
        settings.with(reset_voicings);
        snapshot.with(|| {
            crate::settings::with_legacy_voicing_top_note(|top_note| assert!(top_note.is_none()))
        });
    }

    #[test]
    fn every_pinned_mode_matches_strudel_pitches() {
        let cases = [
            ("below", ["Gb3", "C4", "D4", "Gb4", "C5"].as_slice()),
            ("duck", ["Gb3", "C4", "D4", "Gb4"].as_slice()),
            ("above", ["C5", "Gb5", "A5", "C6", "D6"].as_slice()),
            ("root", ["D5", "A5", "C6", "D6", "Gb6"].as_slice()),
            ("oldabove", ["C5", "Gb5", "A5", "C6", "D6"].as_slice()),
            ("oldroot", ["D4", "A4", "C5", "D5", "Gb5"].as_slice()),
        ];
        for (mode, expected) in cases {
            assert_eq!(rendered_notes(&controls("D7", "C5", mode)), expected);
        }

        assert_eq!(
            rendered_notes(&controls("D7", "B4", "above")),
            ["C5", "Gb5", "A5", "C6", "D6"]
        );
        assert_eq!(
            rendered_notes(&controls("D7", "B4", "oldabove")),
            ["Gb4", "C5", "D5", "Gb5", "A5"]
        );
    }

    #[test]
    fn invalid_modes_and_non_integral_index_controls_follow_caught_throws() {
        assert_eq!(render_voicing(&controls("D7", "C5", "unknown")), None);
        let mut non_string_mode = controls("D7", "C5", "below");
        non_string_mode.insert("mode".into(), Value::Bool(true));
        assert_eq!(render_voicing(&non_string_mode), None);
        for (control, value) in [("offset", 0.5), ("offset", f64::INFINITY)] {
            let mut controls = c7();
            controls.insert(control.into(), Value::F64(value));
            assert_eq!(
                render_voicing(&controls),
                None,
                "{control}={value} should follow strudel.cc's caught throw"
            );
        }
    }

    #[test]
    fn chord_lookup_answers_like_the_renderer() {
        assert!(chord_lookup("Cm7", None).is_ok());
        assert!(chord_lookup("F#^7", None).is_ok());
        assert!(
            chord_lookup("Bb7/D", None).is_ok(),
            "a bass note is allowed"
        );
        let unknown = chord_lookup("Cmajorr", None).unwrap_err();
        assert!(unknown.contains("Cmajorr"), "{unknown}");
        assert!(chord_lookup("xyz", None).is_err());
        assert!(chord_lookup("Cm7", Some("no-such-dict")).is_err());
        assert!(chord_lookup("Cm7", Some("lefthand")).is_ok());
        assert!(chord_symbol_in_any_dictionary("Cm7"));
        assert!(!chord_symbol_in_any_dictionary("Cmajorr"));
    }

    /// The dictionary has the altered dominants that iReal lists, under its
    /// own spelling: `7b9` is the key in `ireal.mjs`. With a mini-notation
    /// colon the chord is a pair of words, and that voices nothing:
    /// `render_voicing` needs a string, and strudel.cc passes its
    /// `tokenizeChord` an array and catches the throw.
    #[test]
    fn the_altered_dominants_resolve_joined_and_never_with_a_colon() {
        for symbol in ["7b9", "7#9", "7b13", "13b9", "7alt", "7b9b13", "7#9#11"] {
            assert!(
                chord_lookup(&format!("G{symbol}"), None).is_ok(),
                "G{symbol} is an ireal chord"
            );
        }
        let colon = chord_lookup("G:7b9", None).unwrap_err();
        assert!(colon.contains("G:7b9"), "{colon}");
        assert!(render_voicing(&controls("G7b9", "c5", "below")).is_some());
    }

    #[test]
    fn offsets_and_scale_step_controls_match_strudel() {
        let mut fractional_anchor = controls("D7", "C5", "below");
        fractional_anchor.insert("anchor".into(), Value::F64(60.5));
        assert_eq!(
            rendered_notes(&fractional_anchor),
            ["Gb2", "C3", "D3", "Gb3", "C4"]
        );

        let mut shifted = controls("D7", "C5", "below");
        shifted.insert("offset".into(), Value::F64(1.0));
        assert_eq!(rendered_notes(&shifted), ["Gb3", "C4", "Gb4", "A4", "D5"]);
        shifted.insert("offset".into(), Value::F64(-1.0));
        assert_eq!(rendered_notes(&shifted), ["Gb3", "C4", "D4", "Gb4", "A4"]);

        let mut selected = controls("D7", "C5", "below");
        selected.insert("octaves".into(), Value::F64(0.5));
        for (n, expected) in [(5.0, 60.0), (-1.0, 66.0)] {
            selected.insert("n".into(), Value::F64(n));
            assert_eq!(render_voicing(&selected), Some(vec![Value::F64(expected)]));
        }
        selected.insert("n".into(), Value::F64(0.5));
        let Some(values) = render_voicing(&selected) else {
            panic!("fractional n should produce strudel.cc NaN, not silence");
        };
        assert!(matches!(values.as_slice(), [Value::F64(value)] if value.is_nan()));
    }

    #[test]
    fn javascript_control_coercions_are_preserved() {
        let mut shifted = controls("D7", "C5", "below");
        shifted.insert("offset".into(), Value::Bool(true));
        assert_eq!(rendered_notes(&shifted), ["Gb3", "C4", "Gb4", "A4", "D5"]);
        shifted.insert("offset".into(), Value::Str("1".into()));
        assert_eq!(rendered_notes(&shifted), ["Gb4", "C5", "D5", "Gb5", "C6"]);
        shifted.insert("offset".into(), Value::Str("0.5".into()));
        assert_eq!(render_voicing(&shifted), None);

        let mut selected = controls("D7", "C5", "below");
        for (n, expected) in [
            (Value::Str("1".into()), 60.0),
            (Value::Bool(true), 60.0),
            (Value::Null, 54.0),
        ] {
            selected.insert("n".into(), n);
            assert_eq!(render_voicing(&selected), Some(vec![Value::F64(expected)]));
        }

        selected.insert("n".into(), Value::F64(5.0));
        selected.insert("octaves".into(), Value::Str("0.5".into()));
        assert_eq!(render_voicing(&selected), Some(vec![Value::F64(60.0)]));
        selected.insert("octaves".into(), Value::Null);
        assert_eq!(render_voicing(&selected), Some(vec![Value::F64(54.0)]));
    }

    #[test]
    fn an_empty_custom_voicing_does_not_reach_an_expect() {
        let mut controls = c7();
        controls.insert(
            "dictionary".into(),
            Value::Object(OrderedMap::from_entries([(
                "7".into(),
                Value::List(vec![Value::List(Vec::new())]),
            )])),
        );
        assert_eq!(render_voicing(&controls), None);
    }

    #[test]
    fn dictionary_defaults_only_for_missing_or_undefined() {
        let controls = c7();
        let expected = render_voicing(&controls).expect("default dictionary");

        let mut undefined = controls.clone();
        undefined.insert("dictionary".into(), Value::Undefined);
        assert_eq!(render_voicing(&undefined), Some(expected));

        for invalid in [Value::Null, Value::Bool(false), Value::F64(0.0)] {
            let mut explicit = controls.clone();
            explicit.insert("dictionary".into(), invalid);
            assert_eq!(render_voicing(&explicit), None);
        }
    }

    #[test]
    fn ducking_the_only_note_then_selecting_n_returns_nan() {
        let mut controls = c7();
        controls.insert("mode".into(), Value::Str("duck".into()));
        controls.insert("n".into(), Value::F64(0.0));
        controls.insert(
            "dictionary".into(),
            Value::Object(OrderedMap::from_entries([(
                "7".into(),
                Value::List(vec![Value::List(vec![Value::F64(0.0)])]),
            )])),
        );
        let Some(values) = render_voicing(&controls) else {
            panic!("an empty scaleStep source returns NaN on strudel.cc");
        };
        assert!(matches!(values.as_slice(), [Value::F64(value)] if value.is_nan()));
    }

    #[test]
    fn user_dictionary_resource_limits_are_bounded_before_retention() {
        {
            let settings = crate::settings::RuntimeSettings::default();
            let _scope = settings.bind();

            let exact_name = "n".repeat(MAX_USER_VOICING_NAME_BYTES);
            register_user_dict_json(&exact_name, "{}").expect("name limit is inclusive");
            let long_name = "n".repeat(MAX_USER_VOICING_NAME_BYTES + 1);
            let error =
                register_user_dict_json(&long_name, "{}").expect_err("name limit + 1 must refuse");
            assert!(error.contains("dictionary name"), "{error}");

            let exact_json = format!("{{}}{}", " ".repeat(MAX_USER_VOICING_JSON_BYTES - 2));
            register_user_dict_json("at-json", &exact_json).expect("JSON limit is inclusive");
            let error = register_user_dict_json("over-json", &format!("{exact_json} "))
                .expect_err("JSON limit + 1 must refuse");
            assert!(error.contains("byte limit"), "{error}");

            let steps = std::iter::repeat_n(0, MAX_USER_VOICING_STEPS).collect::<Vec<_>>();
            register_user_dict_json("at-steps", &serde_json::json!({"7": [steps]}).to_string())
                .expect("step limit is inclusive");
            let too_many_steps =
                std::iter::repeat_n(0, MAX_USER_VOICING_STEPS + 1).collect::<Vec<_>>();
            let error = register_user_dict_json(
                "over-steps",
                &serde_json::json!({"7": [too_many_steps]}).to_string(),
            )
            .expect_err("step limit + 1 must refuse");
            assert!(
                error.contains("did not parse") || error.contains("step limit"),
                "{error}"
            );
        }

        let settings = crate::settings::RuntimeSettings::default();
        let _scope = settings.bind();
        for index in 0..MAX_REGISTERED_USER_VOICING_DICTS {
            register_user_dict_json(&format!("d{index}"), "{}")
                .expect("dictionary-count limit is inclusive");
        }
        let error = register_user_dict_json("one-too-many", "{}")
            .expect_err("dictionary-count limit + 1 must refuse");
        assert!(error.contains("at most"), "{error}");
        assert!(crate::settings::user_voicing_dict("one-too-many").is_none());
    }
}
