/*
midimap.rs - Named maps from control names to MIDI CC numbers
Normalization and aliases follow Strudel packages/midi/midi.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! `midimaps({ mymap: { lpf: 74 } })` - a map from control names to CC
//! numbers, so `.lpf(...)` on a MIDI-bound pattern goes out as CC 74 scaled
//! into the control's range, the way strudel.cc's `mapCC` sends it
//!
//! Maps are settings-scoped like the voicing dictionaries: two Sessions must
//! not see each other's registrations. The map named "default" is what
//! `defaultmidimap` sets and what a hap with no `midimap` control uses.

use std::collections::HashMap;
use std::sync::Arc;

/// Resource ceilings keep score-time registration bounded before it crosses
/// from interruptible JavaScript into Rust parsing and retained settings.
pub const MAX_MIDI_MAP_JSON_BYTES: usize = 64 * 1024;
pub const MAX_MIDI_MAP_BATCH_JSON_BYTES: usize = 256 * 1024;
pub const MAX_MIDI_MAP_ENTRIES: usize = 256;
pub const MAX_MIDI_MAP_NAME_BYTES: usize = 256;
pub const MAX_REGISTERED_MIDI_MAPS: usize = 64;

/// One control-to-CC mapping, keys already canonicalised.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiMapEntry {
    /// The MAIN control name - aliases are resolved at registration, as
    /// strudel.cc's `unifyMapping` runs every key through `getControlName`.
    pub control: String,
    pub ccn: u8,
    pub min: f64,
    pub max: f64,
    pub exp: f64,
}

impl MidiMapEntry {
    /// The control's value as a 0..1 CC value: strudel.cc's `normalize`
    /// clamp before the exponent.
    pub fn normalise(&self, value: f64) -> f64 {
        let normalised = ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        normalised.powf(self.exp)
    }
}

/// Register one named map from JSON.
///
/// The host's `midimaps`/`defaultmidimap` bindings serialise their object and
/// hand it over as one string, exactly as the voicing registration does:
/// evaluation-time work where a round-trip costs nothing. A bad map refuses
/// the evaluation with a message naming the map - strudel.cc instead throws from
/// `normalize` at SEND time, and nothing may end a live set here.
pub fn register_midi_map_json(name: &str, json: &str) -> Result<(), String> {
    validate_map_name(name)?;
    if json.len() > MAX_MIDI_MAP_JSON_BYTES {
        return Err(format!(
            "midimaps {name}: JSON is {} bytes, above the {MAX_MIDI_MAP_JSON_BYTES}-byte limit",
            json.len()
        ));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("midimaps {name}: {error}"))?;
    let entries = parse_midi_map(name, &parsed)?;
    crate::settings::register_midi_maps(vec![(name.to_string(), Arc::new(entries))])
}

/// Atomically register the outer object accepted by strudel.cc `midimaps()`.
/// Parsing and validating every map before one settings publication avoids
/// partial batches and the O(N²) snapshot cloning of one native call per name.
pub fn register_midi_maps_json(json: &str) -> Result<(), String> {
    if json.len() > MAX_MIDI_MAP_BATCH_JSON_BYTES {
        return Err(format!(
            "midimaps: JSON is {} bytes, above the {MAX_MIDI_MAP_BATCH_JSON_BYTES}-byte batch limit",
            json.len()
        ));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("midimaps: {error}"))?;
    let serde_json::Value::Object(mappings) = parsed else {
        return Err("midimaps: expected an object of named maps".into());
    };
    if mappings.len() > MAX_REGISTERED_MIDI_MAPS {
        return Err(format!(
            "midimaps: one batch contains {} maps, above the {MAX_REGISTERED_MIDI_MAPS}-map limit",
            mappings.len()
        ));
    }
    let mut parsed_maps = Vec::with_capacity(mappings.len());
    for (name, mapping) in mappings {
        validate_map_name(&name)?;
        let entries = parse_midi_map(&name, &mapping)?;
        parsed_maps.push((name, Arc::new(entries)));
    }
    crate::settings::register_midi_maps(parsed_maps)
}

fn validate_map_name(name: &str) -> Result<(), String> {
    if name.len() > MAX_MIDI_MAP_NAME_BYTES {
        Err(format!(
            "midimaps: map name is {} bytes, above the {MAX_MIDI_MAP_NAME_BYTES}-byte limit",
            name.len()
        ))
    } else {
        Ok(())
    }
}

fn parse_midi_map(
    name: &str,
    parsed: &serde_json::Value,
) -> Result<HashMap<String, MidiMapEntry>, String> {
    let serde_json::Value::Object(mapping) = parsed else {
        return Err(format!(
            "midimaps {name}: expected an object of {{control: ccn | {{ccn, min, max, exp}}}}"
        ));
    };
    if mapping.len() > MAX_MIDI_MAP_ENTRIES {
        return Err(format!(
            "midimaps {name}: {} entries exceed the {MAX_MIDI_MAP_ENTRIES}-entry limit",
            mapping.len()
        ));
    }
    let mut entries = HashMap::with_capacity(mapping.len());
    let controls = crate::controls::default_control_registry();
    for (key, value) in mapping {
        // `unifyMapping` runs every key through `getControlName`, so `cutoff`
        // and `lpf` land on the same control.
        let control = controls.canonical_name(key).unwrap_or(key).to_string();
        let (ccn, min, max, exp) = match value {
            serde_json::Value::Number(ccn) => (ccn.as_f64(), 0.0, 1.0, 1.0),
            serde_json::Value::Object(spec) => {
                let field =
                    |field_name: &str, default: Option<f64>| -> Result<Option<f64>, String> {
                        match spec.get(field_name) {
                            None => Ok(default),
                            Some(value) => value.as_f64().map(Some).ok_or_else(|| {
                                format!("midimaps {name}: \"{key}.{field_name}\" must be a number")
                            }),
                        }
                    };
                (
                    field("ccn", None)?,
                    field("min", Some(0.0))?.expect("defaulted min"),
                    field("max", Some(1.0))?.expect("defaulted max"),
                    field("exp", Some(1.0))?.expect("defaulted exp"),
                )
            }
            _ => (None, 0.0, 1.0, 1.0),
        };
        let Some(ccn) =
            ccn.filter(|n| n.is_finite() && n.fract() == 0.0 && (0.0..=127.0).contains(n))
        else {
            return Err(format!(
                "midimaps {name}: \"{key}\" needs an integer ccn between 0 and 127"
            ));
        };
        if !min.is_finite() || !max.is_finite() || min == max || !(max - min).is_finite() {
            // Upstream throws this from `normalize` per hap; here it refuses
            // the registration, which is the last moment it can fail safely.
            return Err(format!(
                "midimaps {name}: \"{key}\" needs a finite, non-empty scaling range"
            ));
        }
        if !exp.is_finite() || exp < 0.0 {
            return Err(format!(
                "midimaps {name}: \"{key}\" needs a finite non-negative exponent"
            ));
        }
        if entries.contains_key(&control) {
            return Err(format!(
                "midimaps {name}: \"{key}\" duplicates the canonical control \"{control}\""
            ));
        }
        entries.insert(
            control.clone(),
            MidiMapEntry {
                control,
                ccn: ccn as u8,
                min,
                max,
                exp,
            },
        );
    }
    Ok(entries)
}

/// The entries of a named map, if a score registered one.
pub fn midi_map(name: &str) -> Option<Arc<HashMap<String, MidiMapEntry>>> {
    crate::settings::midi_map(name)
}

/// A deterministic JSON view of the bounded native registry.
///
/// `JsRuntime::midi_maps_json()` returns this view to hosts. The native
/// registry is the only source of truth. Keys are canonical control names
/// and every entry uses the complete `{ccn,min,max,exp}` shape.
pub fn midi_maps_json() -> Option<String> {
    let maps = crate::settings::midi_maps();
    if maps.is_empty() {
        return None;
    }
    let ordered: std::collections::BTreeMap<_, _> = maps
        .into_iter()
        .map(|(name, entries)| {
            let entries: std::collections::BTreeMap<_, _> = entries
                .values()
                .map(|entry| {
                    (
                        entry.control.clone(),
                        serde_json::json!({
                            "ccn": entry.ccn,
                            "min": entry.min,
                            "max": entry.max,
                            "exp": entry.exp,
                        }),
                    )
                })
                .collect();
            (name, entries)
        })
        .collect();
    serde_json::to_string(&ordered).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_number_maps_the_control_over_zero_to_one() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        register_midi_map_json("m", r#"{"lpf": 74}"#).expect("registers");
        let map = midi_map("m").expect("registered");
        assert_eq!(map.len(), 1);
        let entry = map.get("cutoff").expect("canonical lpf entry");
        assert_eq!(entry.ccn, 74);
        assert_eq!(entry.normalise(0.5), 0.5);
        assert_eq!(entry.normalise(2.0), 1.0, "clamped above the range");
    }

    #[test]
    fn a_spec_scales_and_curves_the_way_strudel_normalises() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        register_midi_map_json(
            "m",
            r#"{"lpf": {"ccn": 74, "min": 0, "max": 4000, "exp": 2}}"#,
        )
        .expect("registers");
        let map = midi_map("m").expect("registered");
        let entry = map.get("cutoff").expect("canonical lpf entry");
        assert_eq!(entry.normalise(2000.0), 0.25, "(2000/4000)^2");
        assert_eq!(entry.normalise(-10.0), 0.0);
    }

    #[test]
    fn an_alias_key_lands_on_the_canonical_control() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        register_midi_map_json("m", r#"{"cutoff": 74}"#).expect("registers");
        assert_eq!(
            midi_map("m")
                .expect("registered")
                .get("cutoff")
                .expect("canonical cutoff entry")
                .control,
            "cutoff",
            "the canonical spelling, whatever the score wrote"
        );
    }

    #[test]
    fn a_flat_range_is_refused_at_registration_not_at_send_time() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        let error = register_midi_map_json("m", r#"{"lpf": {"ccn": 74, "min": 1, "max": 1}}"#)
            .expect_err("min == max cannot scale");
        assert!(error.contains("scaling range"), "{error}");
    }

    #[test]
    fn a_fractional_controller_number_is_refused_instead_of_truncated() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        let error = register_midi_map_json("m", r#"{"lpf": 74.9}"#)
            .expect_err("Web MIDI accepts only integer controller numbers");
        assert!(error.contains("integer ccn"), "{error}");
    }

    #[test]
    fn unsafe_range_specs_and_duplicate_aliases_are_refused() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        for (json, expected) in [
            (r#"{"lpf":{"ccn":74,"max":"4000"}}"#, "must be a number"),
            (r#"{"lpf":{"ccn":74,"exp":-1}}"#, "non-negative"),
            (
                r#"{"lpf":{"ccn":74,"min":-1e308,"max":1e308}}"#,
                "scaling range",
            ),
            (r#"{"lpf":74,"cutoff":71}"#, "duplicates"),
        ] {
            let error = register_midi_map_json("m", json).expect_err("unsafe map must refuse");
            assert!(error.contains(expected), "{json}: {error}");
        }
    }

    fn mapping_json(entries: usize) -> String {
        let mapping: serde_json::Map<String, serde_json::Value> = (0..entries)
            .map(|index| {
                (
                    format!("userControl{index}"),
                    serde_json::Value::from((index % 128) as u64),
                )
            })
            .collect();
        serde_json::Value::Object(mapping).to_string()
    }

    #[test]
    fn map_resource_limits_are_exact_and_checked_before_retention() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();

        register_midi_map_json("at-entries", &mapping_json(MAX_MIDI_MAP_ENTRIES))
            .expect("entry limit is inclusive");
        let error = register_midi_map_json("over-entries", &mapping_json(MAX_MIDI_MAP_ENTRIES + 1))
            .expect_err("entry limit + 1 must refuse");
        assert!(error.contains("entry limit"), "{error}");
        assert!(midi_map("over-entries").is_none());

        let exact_name = "n".repeat(MAX_MIDI_MAP_NAME_BYTES);
        register_midi_map_json(&exact_name, "{}").expect("name-byte limit is inclusive");
        let long_name = "n".repeat(MAX_MIDI_MAP_NAME_BYTES + 1);
        let error =
            register_midi_map_json(&long_name, "{}").expect_err("name-byte limit + 1 must refuse");
        assert!(error.contains("map name"), "{error}");

        let exact_json = format!("{{}}{}", " ".repeat(MAX_MIDI_MAP_JSON_BYTES - 2));
        register_midi_map_json("at-bytes", &exact_json).expect("JSON-byte limit is inclusive");
        let over_json = format!("{exact_json} ");
        let error = register_midi_map_json("over-bytes", &over_json)
            .expect_err("JSON-byte limit + 1 must refuse before parse");
        assert!(error.contains("byte limit"), "{error}");
        assert!(midi_map("over-bytes").is_none());
    }

    #[test]
    fn map_batches_are_bounded_and_publish_atomically() {
        let settings = crate::settings::RuntimeSettings::default();
        let _bind = settings.bind();

        let exact_json = format!("{{}}{}", " ".repeat(MAX_MIDI_MAP_BATCH_JSON_BYTES - 2));
        register_midi_maps_json(&exact_json).expect("batch-byte limit is inclusive");
        let error = register_midi_maps_json(&format!("{exact_json} "))
            .expect_err("batch-byte limit + 1 must refuse");
        assert!(error.contains("batch limit"), "{error}");

        let batch: serde_json::Map<String, serde_json::Value> = (0..MAX_REGISTERED_MIDI_MAPS)
            .map(|index| (format!("m{index}"), serde_json::json!({"lpf": index % 128})))
            .collect();
        register_midi_maps_json(&serde_json::Value::Object(batch).to_string())
            .expect("map-count limit is inclusive");
        assert!(midi_map("m0").is_some());

        let error = register_midi_map_json("one-too-many", r#"{"lpf": 1}"#)
            .expect_err("a 65th retained map must refuse");
        assert!(error.contains("at most"), "{error}");
        assert!(midi_map("one-too-many").is_none());

        let fresh = crate::settings::RuntimeSettings::default();
        let _fresh_bind = fresh.bind();
        let oversized: serde_json::Map<String, serde_json::Value> = (0..=MAX_REGISTERED_MIDI_MAPS)
            .map(|index| (format!("x{index}"), serde_json::json!({"lpf": 1})))
            .collect();
        let error = register_midi_maps_json(&serde_json::Value::Object(oversized).to_string())
            .expect_err("an oversized batch must refuse atomically");
        assert!(error.contains("map limit"), "{error}");
        assert!(
            midi_map("x0").is_none(),
            "a failed batch partially published"
        );
    }
}
