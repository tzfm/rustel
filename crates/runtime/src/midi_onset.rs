/*
midi_onset.rs - Read the MIDI intent of one scheduled onset
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Turning a scheduled onset's value into MIDI controls.
//!
//! This module is pure. The live router and the MIDI file exporter both read
//! the same controls here, so a hap sounds the same on a device and in a file.

use std::borrow::Cow;

use rustel_midi::MidiControls;

use crate::hap_json::{OnsetEventJson, ValueJson};

/// The port a hap names, plus the controls it carries.
#[cfg_attr(not(any(feature = "midi", test)), allow(dead_code))]
pub struct MidiOnset {
    pub onset_id: u64,
    pub generation: u64,
    pub port: String,
    pub controls: MidiControls,
    /// Session-clock seconds, the same scale as `OnsetEventJson::target_time`.
    pub target_time: f64,
    pub duration_secs: f64,
}

fn object(value: &ValueJson) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match value {
        ValueJson::Raw(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn number(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    map.get(key)?.as_f64()
}

/// A pitch control. The score can write it as a number or as a note name.
///
/// `note("c3")` arrives here as the string `"c3"`. `rustel-voice` resolves
/// names where the audio path needs a frequency, so the scheduler leaves the
/// value as the score wrote it. This function resolves the name for MIDI. A
/// hap with no usable note plans no messages.
///
/// A bare name uses octave 3, the default of `note_to_hz`, so a name gives
/// the same pitch on both paths.
pub(crate) fn pitch(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    pitch_value(map.get(key)?)
}

/// A pitch written as a number or as a note name.
pub(crate) fn pitch_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => {
            let text = text.trim();
            match text.parse::<f64>() {
                Ok(number) => Some(number),
                // A name that does not parse is not a note; `plan` refuses the
                // NaN `note_to_midi` returns for a half-written one.
                Err(_) => rustel_core::util::note_to_midi(text, 3).ok(),
            }
        }
        _ => None,
    }
}

/// Retain only transport commands the wire planner understands.
///
/// Returning fresh fixed strings is deliberate: an unknown `midicmd` may be
/// arbitrarily large score input and must not be cloned into the live queue
/// merely to be discarded later by `rustel_midi::plan`.
fn midi_command(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    match map.get("midicmd")?.as_str()? {
        "clock" => Some("clock".into()),
        "midiClock" => Some("midiClock".into()),
        "start" => Some("start".into()),
        "stop" => Some("stop".into()),
        "continue" => Some("continue".into()),
        _ => None,
    }
}

/// Borrow the route named by one onset without cloning an attacker-controlled
/// score string. Numeric selectors are formatted into a small owned value.
pub fn midi_route(onset: &OnsetEventJson) -> Option<Cow<'_, str>> {
    let map = object(&onset.value)?;
    match map.get("midiport")? {
        serde_json::Value::String(port) => Some(Cow::Borrowed(port)),
        serde_json::Value::Number(port) => Some(Cow::Owned(port.to_string())),
        _ => None,
    }
}

/// Extract the MIDI intent of one onset after its route has been bounded and
/// copied by the caller.
pub fn midi_onset_with_port(onset: &OnsetEventJson, port: String) -> Option<MidiOnset> {
    let map = object(&onset.value)?;
    let options = map.get("midiopts").and_then(serde_json::Value::as_object);
    let option_number = |key: &str| {
        options
            .and_then(|options| options.get(key))
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite())
    };
    Some(MidiOnset {
        onset_id: onset.onset_id,
        generation: onset.generation,
        port,
        target_time: onset.target_time,
        duration_secs: onset.duration_secs,
        controls: MidiControls {
            // Read `note` only. `n` is the sample-index control, and the
            // audio path does not read it as a pitch either. For example,
            // `s("bd:3 sd:7").midi(port)` must not send note-ons at the
            // sample index.
            note: pitch(map, "note"),
            gain: number(map, "gain").or_else(|| option_number("gain")),
            velocity: number(map, "velocity").or_else(|| option_number("velocity")),
            midichan: number(map, "midichan").or_else(|| option_number("midichannel")),
            ccn: number(map, "ccn").or_else(|| number(map, "ctlNum")),
            ccv: number(map, "ccv"),
            prog_num: number(map, "progNum"),
            midibend: number(map, "midibend"),
            miditouch: number(map, "miditouch"),
            midicmd: midi_command(map),
            is_controller: options
                .and_then(|options| options.get("isController"))
                .is_some_and(|value| match value {
                    serde_json::Value::Bool(flag) => *flag,
                    serde_json::Value::Number(number) => {
                        number.as_f64().is_some_and(|value| value != 0.0)
                    }
                    _ => false,
                }),
            note_offset_ms: option_number("noteOffsetMs"),
            // A midimap turns named controls into CC messages: for every entry
            // of the selected map whose control the hap carries, one CC with
            // the value normalised into the entry's range. The hap's `midimap`
            // control names the map; absent, the map registered as "default"
            // applies, and a hap naming a map nobody registered sends no
            // mapped CCs. This reads the bound settings. The caller
            // (session.rs) binds the runtime's settings around the collection
            // pass, because an unbound read falls back to the process-global
            // state and sees nothing a Session registered.
            mapped_ccs: {
                let name = match map.get("midimap") {
                    None => match options.and_then(|options| options.get("midimap")) {
                        None => Some("default"),
                        Some(serde_json::Value::String(name)) => Some(name.as_str()),
                        Some(_) => None,
                    },
                    Some(serde_json::Value::String(name)) => Some(name.as_str()),
                    // Map lookup is strict: null and numeric values do not
                    // select the string-named default/map.
                    Some(_) => None,
                };
                match name.and_then(rustel_core::midimap::midi_map) {
                    // Never scan the score-controlled hap object: an object
                    // pattern may carry arbitrarily many unrelated keys, and
                    // this conversion runs after the bounded query turn. Walk
                    // the pinned alias table plus the registered map instead;
                    // both have hard native ceilings (currently 499 aliases
                    // and at most 256 map entries), then perform O(1) hap
                    // lookups. Alias canonicalisation survives without giving
                    // one onset unbounded post-query work.
                    Some(entries) => {
                        let controls = rustel_core::controls::default_control_registry();
                        let mut mapped = Vec::new();
                        for (alias, canonical) in controls.alias_entries() {
                            let Some(entry) = entries.get(canonical) else {
                                continue;
                            };
                            let Some(value) = map.get(alias).and_then(serde_json::Value::as_f64)
                            else {
                                continue;
                            };
                            if value.is_finite() {
                                mapped.push((entry.ccn, entry.normalise(value)));
                            }
                        }
                        // User-defined controls have no pinned aliases. The
                        // retained map is a HashMap, so sort its bounded (at
                        // most 256) custom subset before emitting wire work;
                        // otherwise two identical runs can reorder their CCs.
                        let mut custom_entries = entries
                            .iter()
                            .filter(|(control, _)| controls.canonical_name(control).is_none())
                            .collect::<Vec<_>>();
                        custom_entries.sort_unstable_by_key(|(name, _)| *name);
                        for (control, entry) in custom_entries {
                            let Some(value) = map
                                .get(control)
                                .and_then(serde_json::Value::as_f64)
                                .filter(|value| value.is_finite())
                            else {
                                continue;
                            };
                            mapped.push((entry.ccn, entry.normalise(value)));
                        }
                        mapped
                    }
                    None => Vec::new(),
                }
            },
        },
    })
}

/// Extract the MIDI intent of one onset, or `None` if it names no port.
#[cfg_attr(not(any(feature = "midi", test)), allow(dead_code))]
pub fn midi_onset(onset: &OnsetEventJson) -> Option<MidiOnset> {
    let port = midi_route(onset)?.into_owned();
    midi_onset_with_port(onset, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A midimap turns a named control into a CC message. The whole chain:
    /// registration (settings-scoped), the hap's `midimap` selector, the range
    /// normalisation, and the planner emitting the wire bytes.
    #[test]
    fn a_midimap_derives_ccs_from_named_controls() {
        let settings = rustel_core::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        rustel_core::midimap::register_midi_map_json(
            "mymap",
            r#"{"lpf": {"ccn": 74, "min": 0, "max": 4000}}"#,
        )
        .expect("registers");
        rustel_core::midimap::register_midi_map_json("default", r#"{"gain": 7}"#)
            .expect("registers");

        // The named map scales lpf into 0..1.
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x", "midimap": "mymap", "cutoff": 1000.0,
        })))
        .expect("midi hap");
        assert_eq!(got.controls.mapped_ccs, vec![(74, 0.25)]);
        // And the planner sends it as a real CC: 0.25 * 127 rounds to 32.
        let planned = rustel_midi::plan(&got.controls, 0.5);
        assert!(
            planned
                .iter()
                .any(|(_, message)| message.as_slice() == [0xb0, 74, 32]),
            "no CC 74 on the wire: {planned:?}"
        );

        // No `midimap` control means the map registered as "default".
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x", "gain": 0.5,
        })))
        .expect("midi hap");
        assert_eq!(got.controls.mapped_ccs, vec![(7, 0.5)]);

        // A map nobody registered derives nothing - and must not panic.
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x", "midimap": "nope", "cutoff": 1000.0,
        })))
        .expect("midi hap");
        assert!(got.controls.mapped_ccs.is_empty());
    }

    #[test]
    fn mapped_aliases_and_custom_controls_keep_a_deterministic_order() {
        let settings = rustel_core::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        // Registration canonicalises `cutoff` to `lpf`; the hap deliberately
        // uses the canonical spelling to cover both directions of the alias
        // surface. Custom entries remain literal and are emitted by name.
        rustel_core::midimap::register_midi_map_json(
            "mixed",
            r#"{
            "z_custom": 12,
            "cutoff": {"ccn": 74, "min": 0, "max": 4000},
            "a_custom": 11
        }"#,
        )
        .expect("register mixed map");

        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "midimap": "mixed",
            "lpf": 1000.0,
            "z_custom": 0.8,
            "a_custom": 0.2,
        })))
        .expect("midi hap");
        assert_eq!(
            got.controls.mapped_ccs,
            vec![(74, 0.25), (11, 0.2), (12, 0.8)],
            "known aliases must be preserved and custom controls sorted"
        );
    }

    #[test]
    fn a_huge_unrelated_hap_object_does_not_change_the_bounded_map_surface() {
        let settings = rustel_core::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        rustel_core::midimap::register_midi_map_json("bounded", r#"{"mapped_custom": 23}"#)
            .expect("register custom map");

        // The post-query MIDI conversion must walk the capped registered map,
        // not this score-controlled object. Keep this far above every native
        // map/alias ceiling so a future reintroduction of hap scanning is
        // conspicuous in both profiling and review.
        let mut value = serde_json::Map::new();
        for index in 0..20_000 {
            value.insert(format!("unrelated_{index:05}"), serde_json::json!(index));
        }
        value.insert("midiport".into(), serde_json::json!("x"));
        value.insert("midimap".into(), serde_json::json!("bounded"));
        value.insert("mapped_custom".into(), serde_json::json!(0.5));

        let got = midi_onset(&onset(serde_json::Value::Object(value))).expect("midi hap");
        assert_eq!(got.controls.mapped_ccs, vec![(23, 0.5)]);
    }

    fn onset(value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0/1".into(),
            duration_secs: 0.5,
            target_time: 12.5,
            live_controls: [0; 3],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }

    #[test]
    fn a_hap_without_a_port_is_not_a_midi_hap() {
        assert!(midi_onset(&onset(serde_json::json!({ "note": 60, "s": "bd" }))).is_none());
    }

    #[test]
    fn the_port_may_be_a_name_or_an_index() {
        let named = midi_onset(&onset(serde_json::json!({ "midiport": "IAC Bus 1" }))).unwrap();
        assert_eq!(named.port, "IAC Bus 1");
        // Mini-notation reduces a bare 0 to a number, not a string.
        let indexed = midi_onset(&onset(serde_json::json!({ "midiport": 0 }))).unwrap();
        assert_eq!(indexed.port, "0");
    }

    #[test]
    fn controls_and_timing_come_through_intact() {
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "gain": 0.5,
            "midichan": 3,
            "ccn": 74,
            "ccv": 0.25,
            "midicmd": "start",
        })))
        .unwrap();
        assert_eq!(got.onset_id, 1);
        assert_eq!(got.generation, 1);
        assert_eq!(got.target_time, 12.5);
        assert_eq!(got.duration_secs, 0.5);
        assert_eq!(got.controls.note, Some(64.0));
        assert_eq!(got.controls.gain, Some(0.5));
        assert_eq!(got.controls.midichan, Some(3.0));
        assert_eq!(got.controls.ccn, Some(74.0));
        assert_eq!(got.controls.ccv, Some(0.25));
        assert_eq!(got.controls.midicmd.as_deref(), Some("start"));
    }

    #[test]
    fn midi_options_are_defaults_that_the_scores_own_controls_outrank() {
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "gain": 0.25,
            "velocity": 0.4,
            "midichan": 3,
            "midiopts": {
                "gain": 0.9,
                "velocity": 0.8,
                "midichannel": 9
            }
        })))
        .expect("MIDI intent");
        assert_eq!(got.controls.gain, Some(0.25));
        assert_eq!(got.controls.velocity, Some(0.4));
        assert_eq!(got.controls.midichan, Some(3.0));

        let fallback = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "midiopts": {
                "gain": 0.7,
                "velocity": 0.6,
                "midichannel": 11
            }
        })))
        .expect("MIDI intent");
        assert_eq!(fallback.controls.gain, Some(0.7));
        assert_eq!(fallback.controls.velocity, Some(0.6));
        assert_eq!(fallback.controls.midichan, Some(11.0));
    }

    #[test]
    fn is_controller_and_note_offset_cross_the_boundary() {
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "midiopts": { "isController": true, "noteOffsetMs": 10 }
        })))
        .expect("MIDI intent");
        assert!(got.controls.is_controller);
        assert_eq!(got.controls.note_offset_ms, Some(10.0));

        let plain = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64
        })))
        .expect("MIDI intent");
        assert!(!plain.controls.is_controller);
        assert_eq!(plain.controls.note_offset_ms, None);
    }

    #[test]
    fn unknown_midi_commands_are_not_cloned_into_live_intents() {
        let oversized = "not-a-command".repeat(100_000);
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "midicmd": oversized,
        })))
        .unwrap();
        assert!(got.controls.midicmd.is_none());

        for command in ["clock", "midiClock", "start", "stop", "continue"] {
            let got = midi_onset(&onset(serde_json::json!({
                "midiport": "x",
                "midicmd": command,
            })))
            .unwrap();
            assert_eq!(got.controls.midicmd.as_deref(), Some(command));
        }
    }

    /// A note written as a name must send MIDI. Names arrive here as strings,
    /// because `rustel-voice` resolves them where the audio path needs a
    /// frequency.
    #[test]
    fn a_note_name_is_a_pitch_the_same_way_a_number_is() {
        let got = midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": "c3" }))).unwrap();
        // Octave 3 is the default the audio path uses, so both paths agree.
        assert_eq!(got.controls.note, Some(48.0));

        for (name, expected) in [("e3", 52.0), ("g3", 55.0), ("a4", 69.0), ("cs3", 49.0)] {
            let got =
                midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": name }))).unwrap();
            assert_eq!(got.controls.note, Some(expected), "note name {name}");
        }
    }

    /// A number that arrived as text is still a number, and a word that is not
    /// a note is not one - it must not become a pitch by accident.
    #[test]
    fn a_numeric_string_is_a_number_and_a_non_note_is_neither() {
        let numeric =
            midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": "60" }))).unwrap();
        assert_eq!(numeric.controls.note, Some(60.0));

        let nonsense =
            midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": "bd" }))).unwrap();
        assert_eq!(
            nonsense.controls.note, None,
            "a sound name is not a pitch and must plan no note"
        );
    }

    /// `n` is the sample index, not a pitch. `s("bd:3").midi(port)` sets `n`
    /// through the sample-variant syntax without the score ever naming it, so
    /// reading it as a note turned every drum line into note-ons at the sample
    /// index.
    #[test]
    fn n_is_a_sample_index_and_never_a_note() {
        let variant = midi_onset(&onset(
            serde_json::json!({ "midiport": "x", "s": "bd", "n": 3 }),
        ))
        .unwrap();
        assert_eq!(
            variant.controls.note, None,
            "a sample variant became a note number"
        );
        // A real note still wins, alongside an `n` that selects the sample.
        let both = midi_onset(&onset(
            serde_json::json!({ "midiport": "x", "n": 3, "note": 60 }),
        ))
        .unwrap();
        assert_eq!(both.controls.note, Some(60.0));
    }
}
