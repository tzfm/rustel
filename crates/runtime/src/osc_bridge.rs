/*
osc_bridge.rs - Route scheduled onsets to an OSC listener
Onset control mapping adapted from Strudel packages/osc/osc.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Turning scheduled onsets into `/dirt/play` messages.
//!
//! The translation is strudel.cc's `parseControlsFromHap`: every control of the
//! hap, plus `cps`, `cycle` and `delta`, flattened into alternating key/value
//! arguments. SuperDirt reads them by name, so the set of keys IS the protocol
//! and the derived ones below have to match exactly.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};

use rustel_osc::OscValue;

use crate::hap_json::{OnsetEventJson, ValueJson};

/// Destinations an untrusted score may send UDP to.
///
/// Loopback is always allowed (SuperDirt's default). Any other address needs
/// an explicit host grant, mirroring sample-origin policy: the score names
/// `oschost`, but it cannot pick a private, metadata, or public address the
/// operator did not name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoreOscAccess {
    allowed_hosts: Vec<IpAddr>,
}

impl ScoreOscAccess {
    pub fn loopback_only() -> Self {
        Self::default()
    }

    /// Permit one additional IP. Names are refused here so grant-time cannot
    /// hang on DNS either.
    pub fn permit_host(&mut self, host: &str) -> Result<(), String> {
        let ip = canonical_osc_ip(rustel_osc::parse_osc_ip(host)?);
        if rustel_osc::ip_is_unsendable(ip) {
            return Err(format!("OSC host {host} is not a unicast destination"));
        }
        if !self.allowed_hosts.contains(&ip) {
            self.allowed_hosts.push(ip);
        }
        Ok(())
    }

    /// Resolve `host`:`port` without DNS and check it against this grant.
    pub fn approve(&self, host: &str, port: u16) -> Result<SocketAddr, String> {
        let destination = rustel_osc::parse_osc_destination(host, port)?;
        let ip = canonical_osc_ip(destination.ip());
        if rustel_osc::ip_is_unsendable(ip) {
            return Err(format!(
                "OSC destination {destination} is not a unicast address"
            ));
        }
        if rustel_osc::ip_is_loopback(ip) || self.allowed_hosts.contains(&ip) {
            return Ok(SocketAddr::new(ip, destination.port()));
        }
        Err(format!(
            "OSC destination {destination} is not permitted; pass --allow-osc-host {ip} to grant it"
        ))
    }
}

fn canonical_osc_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        IpAddr::V4(ip) => IpAddr::V4(ip),
    }
}

/// One onset's destination and encoded arguments.
pub struct OscOnset {
    pub onset_id: u64,
    pub generation: u64,
    pub host: String,
    pub port: u16,
    /// Filled after [`ScoreOscAccess::approve`]. Send uses this address so
    /// the live loop never calls `getaddrinfo`.
    pub destination: Option<SocketAddr>,
    pub args: Vec<OscValue>,
    /// Exact final bundle size, preflighted before any sender allocation.
    pub encoded_bytes: usize,
    /// Session-clock seconds, the same scale as `OnsetEventJson::target_time`.
    pub target_time: f64,
}

fn object(value: &ValueJson) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match value {
        ValueJson::Raw(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn finite_f32(value: f64) -> Option<f32> {
    let narrowed = value as f32;
    narrowed.is_finite().then_some(narrowed)
}

/// Routing is parsed separately so the Session can approve a destination
/// before cloning or serializing any score-controlled OSC values.
pub fn osc_route(onset: &OnsetEventJson) -> Option<(&str, u16)> {
    let map = object(&onset.value)?;
    let port = map.get("oscport")?.as_f64()?;
    // Reject fractional ports before the cast can truncate them to a different
    // destination. Per-hap values still need validation after `.osc()` defaults.
    if !port.is_finite() || port.fract() != 0.0 || port <= 0.0 || port >= 65536.0 {
        return None;
    }
    let host = map
        .get("oschost")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(rustel_osc::DEFAULT_OSC_HOST);
    Some((host, port as u16))
}

const MAX_OSC_ARGUMENTS: usize = 512;

struct OscBuildBudget {
    copied_text_bytes: usize,
}

impl OscBuildBudget {
    fn copy_text(&mut self, text: &str) -> Option<String> {
        self.copied_text_bytes = self.copied_text_bytes.checked_add(text.len())?;
        if self.copied_text_bytes > rustel_osc::MAX_DATAGRAM_BYTES {
            return None;
        }
        Some(text.to_owned())
    }

    fn json_text(&mut self, value: &serde_json::Value) -> Option<String> {
        let remaining = rustel_osc::MAX_DATAGRAM_BYTES.checked_sub(self.copied_text_bytes)?;
        let mut nodes = 0usize;
        if !json_shape_is_bounded(value, remaining, &mut nodes) {
            return None;
        }
        let mut writer = BoundedJsonWriter {
            bytes: Vec::with_capacity(remaining.min(256)),
            remaining,
        };
        serde_json::to_writer(&mut writer, value).ok()?;
        self.copied_text_bytes = self.copied_text_bytes.checked_add(writer.bytes.len())?;
        String::from_utf8(writer.bytes).ok()
    }
}

fn json_shape_is_bounded(value: &serde_json::Value, remaining: usize, nodes: &mut usize) -> bool {
    *nodes = (*nodes).saturating_add(1);
    if *nodes > MAX_OSC_ARGUMENTS {
        return false;
    }
    match value {
        serde_json::Value::String(text) => text.len() <= remaining,
        serde_json::Value::Array(values) => values
            .iter()
            .all(|value| json_shape_is_bounded(value, remaining, nodes)),
        serde_json::Value::Object(values) => values.iter().all(|(key, value)| {
            key.len() <= remaining && json_shape_is_bounded(value, remaining, nodes)
        }),
        _ => true,
    }
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    remaining: usize,
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other(
                "OSC JSON value exceeds its wire budget",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A JSON value as an OSC argument.
///
/// Numbers go as floats because SuperDirt coerces per key and floats round-trip
/// every numeric control it has; booleans follow Tidal in travelling as 0/1
/// rather than OSC's `T`/`F`, which SuperDirt does not expect here.
fn osc_value(
    value: &serde_json::Value,
    budget: &mut OscBuildBudget,
) -> Result<Option<OscValue>, ()> {
    Ok(match value {
        serde_json::Value::Number(number) => number.as_f64().and_then(|value| {
            // Finiteness is checked AFTER narrowing, not before: f64::MAX is a
            // perfectly finite f64 and becomes infinity as an f32, which would
            // have put a non-finite float on the wire.
            finite_f32(value).map(OscValue::Float)
        }),
        serde_json::Value::String(text) => Some(OscValue::Str(budget.copy_text(text).ok_or(())?)),
        serde_json::Value::Bool(flag) => Some(OscValue::Float(if *flag { 1.0 } else { 0.0 })),
        // Arrays and objects have no scalar OSC form. Upstream stringifies
        // `channels` specifically; anything else it would send as "[object
        // Object]", which is noise, so it is dropped instead.
        serde_json::Value::Array(_) => Some(OscValue::Str(budget.json_text(value).ok_or(())?)),
        _ => None,
    })
}

/// A pitch control, which the score may have written as a name.
///
/// Octave 3 for a bare name, the default `note_to_hz` uses, so a name reaches
/// the same pitch whether it goes out as audio or as OSC. Mirrors
/// `midi_bridge::pitch`: a defect in one is a defect in the other.
fn pitch(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    match map.get(key)? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => {
            let text = text.trim();
            match text.parse::<f64>() {
                Ok(number) => Some(number),
                Err(_) => rustel_core::util::note_to_midi(text, 3).ok(),
            }
        }
        _ => None,
    }
}

/// Extract the OSC intent of one onset, or `None` if it names no port.
///
/// `cycle` comes from `whole_begin`, which is an exact rational string; parsing
/// it back to a float matches strudel.cc's `hap.wholeOrPart().begin.valueOf()`.
pub fn osc_onset(onset: &OnsetEventJson, cps: f64) -> Option<OscOnset> {
    let map = object(&onset.value)?;
    let (host, port) = osc_route(onset)?;
    if host.len() > rustel_osc::MAX_OSC_HOST_BYTES {
        return None;
    }
    let host = host.to_owned();
    let mut budget = OscBuildBudget {
        copied_text_bytes: 0,
    };

    let cps = finite_f32(cps).filter(|cps| *cps > 0.0)?;
    let cycle = finite_f32(parse_rational(&onset.whole_begin).unwrap_or(0.0))?;
    let delta = finite_f32(onset.duration_secs)?;
    if !onset.target_time.is_finite() {
        return None;
    }
    let mut args = vec![
        OscValue::Str("cps".into()),
        OscValue::Float(cps),
        OscValue::Str("cycle".into()),
        OscValue::Float(cycle),
        OscValue::Str("delta".into()),
        OscValue::Float(delta),
    ];

    // `note` becomes `midinote`, the key SuperDirt reads.
    //
    // The note may be a name. `rustel-voice` resolves names where the audio
    // path needs a frequency, so the scheduler leaves the value as the score
    // wrote it and `pitch` resolves it here. Mirrors strudel.cc's
    // isNote/noteToMidi branch.
    if let Some(note) = pitch(map, "note").and_then(finite_f32) {
        args.push(OscValue::Str("midinote".into()));
        args.push(OscValue::Float(note));
    }

    let mut controls_seen = 0usize;
    for (key, value) in map {
        controls_seen = controls_seen.saturating_add(1);
        if controls_seen > MAX_OSC_ARGUMENTS / 2 {
            return None;
        }
        // Routing, not music: SuperDirt has no use for these.
        if matches!(key.as_str(), "oscport" | "oschost") {
            continue;
        }
        // `roomsize` is SuperDirt's `size`.
        let key = if key == "roomsize" {
            "size"
        } else {
            key.as_str()
        };
        if args.len().saturating_add(2) > MAX_OSC_ARGUMENTS {
            return None;
        }
        let encoded = match osc_value(value, &mut budget) {
            Ok(Some(encoded)) => encoded,
            Ok(None) => continue,
            Err(()) => return None,
        };
        args.push(OscValue::Str(budget.copy_text(key)?));
        args.push(encoded);
    }

    // SuperDirt applies its own CPS adjustment, so undo ours before sending,
    // as upstream does: `unit === 'c' && (speed = speed / cps)`.
    //
    // The search walks key/value pairs, so only a KEY can match: every push
    // above appends a key and then its value. A score whose sound (or bank)
    // is named "speed" puts that string in a value slot ahead of the real key,
    // and searching every slot found the value first, looked for a float in
    // the next KEY slot, failed, and silently skipped the adjustment.
    if map.get("unit").and_then(|value| value.as_str()) == Some("c")
        && let Some(index) = args
            .as_chunks::<2>()
            .0
            .iter()
            .position(|pair| matches!(&pair[0], OscValue::Str(key) if key == "speed"))
            .map(|pair| pair * 2)
        && let Some(OscValue::Float(speed)) = args.get(index + 1).cloned()
    {
        let adjusted = speed / cps;
        if !adjusted.is_finite() {
            return None;
        }
        args[index + 1] = OscValue::Float(adjusted);
    }

    let encoded_bytes = rustel_osc::dirt_bundle_encoded_len(&args)?;
    if encoded_bytes > rustel_osc::MAX_DATAGRAM_BYTES {
        return None;
    }
    Some(OscOnset {
        onset_id: onset.onset_id,
        generation: onset.generation,
        host,
        port,
        destination: None,
        args,
        encoded_bytes,
        target_time: onset.target_time,
    })
}

/// Parse `"3/4"` or `"3"` to a float.
fn parse_rational(text: &str) -> Option<f64> {
    match text.split_once('/') {
        Some((numerator, denominator)) => {
            let numerator: f64 = numerator.trim().parse().ok()?;
            let denominator: f64 = denominator.trim().parse().ok()?;
            (denominator != 0.0).then_some(numerator / denominator)
        }
        None => text.trim().parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onset(value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "3/4".into(),
            duration_secs: 0.25,
            target_time: 9.5,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }

    fn pairs(args: &[OscValue]) -> Vec<(String, OscValue)> {
        args.chunks(2)
            .filter_map(|pair| match pair {
                [OscValue::Str(key), value] => Some((key.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_hap_without_a_port_is_not_an_osc_hap() {
        assert!(osc_onset(&onset(serde_json::json!({ "s": "bd" })), 0.5).is_none());
    }

    #[test]
    fn cps_cycle_and_delta_are_always_present() {
        let got = osc_onset(&onset(serde_json::json!({ "oscport": 57120 })), 0.5).unwrap();
        let pairs = pairs(&got.args);
        assert_eq!(got.onset_id, 1);
        assert_eq!(got.generation, 1);
        assert!(pairs.contains(&("cps".into(), OscValue::Float(0.5))));
        // whole_begin "3/4" is cycle 0.75.
        assert!(pairs.contains(&("cycle".into(), OscValue::Float(0.75))));
        assert!(pairs.contains(&("delta".into(), OscValue::Float(0.25))));
        assert_eq!(got.port, 57120);
        assert_eq!(got.host, rustel_osc::DEFAULT_OSC_HOST);
        assert_eq!(
            got.encoded_bytes,
            rustel_osc::dirt_bundle_encoded_len(&got.args).expect("encoded size")
        );
        assert!(got.encoded_bytes <= rustel_osc::MAX_DATAGRAM_BYTES);
    }

    #[test]
    fn score_controlled_osc_text_is_bounded_before_it_is_cloned_or_encoded() {
        let huge = "x".repeat(rustel_osc::MAX_DATAGRAM_BYTES * 2);
        assert!(
            osc_onset(
                &onset(serde_json::json!({ "oscport": 57120, "s": huge })),
                0.5
            )
            .is_none()
        );

        let huge_array = vec!["xxxxxxxxxxxxxxxx"; rustel_osc::MAX_DATAGRAM_BYTES];
        assert!(
            osc_onset(
                &onset(serde_json::json!({
                    "oscport": 57120,
                    "channels": huge_array
                })),
                0.5
            )
            .is_none()
        );
    }

    /// A pitch written as a name arrives here as a string and must still
    /// become `midinote`.
    #[test]
    fn a_note_name_becomes_midinote_the_same_way_a_number_does() {
        for (name, expected) in [("c3", 48.0), ("e3", 52.0), ("a4", 69.0), ("cs3", 49.0)] {
            let got = osc_onset(
                &onset(serde_json::json!({ "oscport": 57120, "note": name })),
                0.5,
            )
            .unwrap();
            assert!(
                pairs(&got.args).contains(&("midinote".into(), OscValue::Float(expected))),
                "note name {name} did not reach midinote {expected}: {:?}",
                got.args
            );
        }
    }

    /// A word that is not a note must not become a pitch by accident, and a
    /// number written as text is still a number.
    #[test]
    fn a_non_note_sends_no_midinote_and_a_numeric_string_still_does() {
        let nonsense = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "note": "bd" })),
            0.5,
        )
        .unwrap();
        assert!(
            !pairs(&nonsense.args).iter().any(|(k, _)| k == "midinote"),
            "a sound name became a pitch: {:?}",
            nonsense.args
        );

        let numeric = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "note": "60" })),
            0.5,
        )
        .unwrap();
        assert!(pairs(&numeric.args).contains(&("midinote".into(), OscValue::Float(60.0))));
    }

    #[test]
    fn note_is_sent_as_midinote_which_is_the_key_superdirt_reads() {
        let got = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "note": 60 })),
            0.5,
        )
        .unwrap();
        assert!(pairs(&got.args).contains(&("midinote".into(), OscValue::Float(60.0))));
    }

    #[test]
    fn roomsize_is_renamed_to_size() {
        let got = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "roomsize": 0.4 })),
            0.5,
        )
        .unwrap();
        let pairs = pairs(&got.args);
        assert!(pairs.contains(&("size".into(), OscValue::Float(0.4))));
        assert!(!pairs.iter().any(|(key, _)| key == "roomsize"));
    }

    /// Upstream undoes its own CPS adjustment so SuperDirt can apply its.
    #[test]
    fn speed_is_divided_by_cps_only_when_unit_is_c() {
        let adjusted = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "speed": 2.0, "unit": "c" })),
            0.5,
        )
        .unwrap();
        assert!(pairs(&adjusted.args).contains(&("speed".into(), OscValue::Float(4.0))));

        let untouched = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "speed": 2.0 })),
            0.5,
        )
        .unwrap();
        assert!(pairs(&untouched.args).contains(&("speed".into(), OscValue::Float(2.0))));
    }

    /// A sound named "speed" puts the string "speed" in a value slot ahead of
    /// the real `speed` key. The unit "c" adjustment must match keys only.
    #[test]
    fn a_sound_named_speed_does_not_shadow_the_speed_key() {
        let got = osc_onset(
            &onset(serde_json::json!({
                "oscport": 57120,
                "s": "speed",
                "speed": 2.0,
                "unit": "c"
            })),
            0.5,
        )
        .unwrap();
        assert!(
            pairs(&got.args).contains(&("speed".into(), OscValue::Float(4.0))),
            "unit \"c\" speed was not divided by cps: {:?}",
            got.args
        );
    }

    /// Any string value "speed" sorting ahead of the key shadowed it, not
    /// only the sound name: a bank called "speed" did too.
    #[test]
    fn a_bank_named_speed_does_not_shadow_the_speed_key() {
        let got = osc_onset(
            &onset(serde_json::json!({
                "oscport": 57120,
                "bank": "speed",
                "s": "bd",
                "speed": 2.0,
                "unit": "c"
            })),
            0.5,
        )
        .unwrap();
        assert!(
            pairs(&got.args).contains(&("speed".into(), OscValue::Float(4.0))),
            "unit \"c\" speed was not divided by cps: {:?}",
            got.args
        );
    }

    /// Without `unit("c")` the speed goes out as the score wrote it, whatever
    /// the sound is named.
    #[test]
    fn a_sound_named_speed_without_unit_c_is_not_adjusted() {
        for map in [
            serde_json::json!({ "oscport": 57120, "s": "speed", "speed": 2.0 }),
            serde_json::json!({ "oscport": 57120, "s": "speed", "speed": 2.0, "unit": "r" }),
        ] {
            let got = osc_onset(&onset(map), 0.5).unwrap();
            assert!(
                pairs(&got.args).contains(&("speed".into(), OscValue::Float(2.0))),
                "{:?}",
                got.args
            );
        }
    }

    /// The only "speed" text is the sound name itself: there is no key to
    /// adjust, and the message still builds.
    #[test]
    fn a_sound_named_speed_without_a_speed_control_builds_untouched() {
        let got = osc_onset(
            &onset(serde_json::json!({
                "oscport": 57120,
                "s": "speed",
                "unit": "c"
            })),
            0.5,
        )
        .unwrap();
        let pairs = pairs(&got.args);
        assert!(pairs.contains(&("s".into(), OscValue::Str("speed".into()))));
        assert!(!pairs.iter().any(|(key, _)| key == "speed"));
        assert_eq!(got.args.len() % 2, 0, "arguments must stay in pairs");
    }

    #[test]
    fn the_destination_can_be_overridden_per_hap() {
        let got = osc_onset(
            &onset(serde_json::json!({ "oscport": 9000, "oschost": "10.0.0.5" })),
            0.5,
        )
        .unwrap();
        assert_eq!(got.port, 9000);
        assert_eq!(got.host, "10.0.0.5");
        // Routing keys are not musical arguments.
        let keys: Vec<String> = pairs(&got.args).into_iter().map(|(key, _)| key).collect();
        assert!(!keys.contains(&"oscport".to_string()));
        assert!(!keys.contains(&"oschost".to_string()));
    }

    /// A fractional port never reaches the wire: `as u16` would truncate 0.5
    /// to port 0 and 1.9 to port 1. A per-hap `.oscport()` pattern carries the
    /// raw value here, where it is refused.
    #[test]
    fn a_fractional_port_is_refused_rather_than_truncated() {
        for port in [0.5, 0.999, 1.9, 57120.5, 65535.5] {
            assert_eq!(
                osc_route(&onset(serde_json::json!({ "oscport": port }))),
                None,
                "fractional port {port} was routed"
            );
            assert!(
                osc_onset(&onset(serde_json::json!({ "oscport": port })), 0.5).is_none(),
                "fractional port {port} produced an onset"
            );
        }
    }

    /// Whole ports route as themselves - both ends of the valid range
    /// included - and the hostile-value refusals below stay unchanged.
    #[test]
    fn whole_ports_route_unchanged_including_the_range_boundaries() {
        for port in [1u16, 2, 57120, 65534, 65535] {
            assert_eq!(
                osc_route(&onset(serde_json::json!({ "oscport": port }))),
                Some((rustel_osc::DEFAULT_OSC_HOST, port)),
                "whole port {port} did not route"
            );
            let got = osc_onset(&onset(serde_json::json!({ "oscport": port })), 0.5).unwrap();
            assert_eq!(got.port, port);
        }
    }

    /// However strange the score, the arguments must stay encodable: an
    /// alternating key/value list of finite scalars.
    #[test]
    fn hostile_values_never_produce_an_unencodable_argument_list() {
        for port in [0.0, -1.0, 65536.0, f64::NAN, f64::INFINITY] {
            assert!(
                osc_onset(&onset(serde_json::json!({ "oscport": port })), 0.5).is_none(),
                "port {port} was accepted"
            );
        }
        let got = osc_onset(
            &onset(serde_json::json!({
                "oscport": 57120,
                "a": f64::MAX,
                "b": "text",
                "c": true,
                "d": null,
                "e": { "nested": 1 },
            })),
            0.5,
        )
        .unwrap();
        assert_eq!(got.args.len() % 2, 0, "arguments must stay in pairs");
        for pair in got.args.chunks(2) {
            assert!(
                matches!(pair[0], OscValue::Str(_)),
                "a key was not a string: {pair:?}"
            );
            if let OscValue::Float(value) = pair[1] {
                assert!(value.is_finite(), "non-finite float reached the wire");
            }
        }

        for cps in [f64::MIN_POSITIVE, f64::MAX, f64::NAN, f64::INFINITY] {
            assert!(
                osc_onset(
                    &onset(serde_json::json!({ "oscport": 57120, "s": "bd" })),
                    cps
                )
                .is_none(),
                "unrepresentable cps {cps} reached the wire"
            );
        }

        let huge_note = osc_onset(
            &onset(serde_json::json!({ "oscport": 57120, "note": f64::MAX })),
            0.5,
        )
        .expect("an unusable optional note is omitted");
        assert!(
            !pairs(&huge_note.args)
                .iter()
                .any(|(key, _)| key == "midinote")
        );

        let mut invalid_timing = onset(serde_json::json!({ "oscport": 57120, "s": "bd" }));
        invalid_timing.duration_secs = f64::MAX;
        assert!(osc_onset(&invalid_timing, 0.5).is_none());
        invalid_timing.duration_secs = 0.25;
        invalid_timing.target_time = f64::INFINITY;
        assert!(osc_onset(&invalid_timing, 0.5).is_none());
        invalid_timing.target_time = 0.0;
        invalid_timing.whole_begin = "1e400".into();
        assert!(osc_onset(&invalid_timing, 0.5).is_none());

        assert!(
            osc_onset(
                &onset(serde_json::json!({
                    "oscport": 57120,
                    "speed": f32::MAX,
                    "unit": "c"
                })),
                f64::from(f32::MIN_POSITIVE)
            )
            .is_none(),
            "speed/cps overflow reached the wire"
        );
    }

    #[test]
    fn rationals_parse_the_way_the_scheduler_writes_them() {
        assert_eq!(parse_rational("3/4"), Some(0.75));
        assert_eq!(parse_rational("2"), Some(2.0));
        assert_eq!(parse_rational("1/0"), None);
        assert_eq!(parse_rational("nonsense"), None);
    }

    #[test]
    fn osc_access_defaults_to_loopback_and_requires_a_grant_for_anything_else() {
        let access = ScoreOscAccess::loopback_only();
        assert!(access.approve("127.0.0.1", 57120).is_ok());
        assert!(access.approve("localhost", 57120).is_ok());
        assert!(access.approve("::1", 57120).is_ok());
        let private = access
            .approve("10.0.0.5", 57120)
            .expect_err("private addresses need a grant");
        assert!(private.contains("--allow-osc-host"), "{private}");
        let metadata = access
            .approve("169.254.169.254", 80)
            .expect_err("link-local metadata needs a grant");
        assert!(metadata.contains("not permitted"), "{metadata}");
        let name = access
            .approve("evil.example", 57120)
            .expect_err("names must not be resolved");
        assert!(name.contains("not an IP address"), "{name}");

        let mut granted = ScoreOscAccess::loopback_only();
        granted
            .permit_host("10.0.0.5")
            .expect("grant LAN SuperDirt");
        assert!(granted.approve("10.0.0.5", 57120).is_ok());
        assert!(granted.approve("8.8.8.8", 57120).is_err());
        assert!(granted.permit_host("0.0.0.0").is_err());
        assert!(granted.permit_host("studio.local").is_err());
    }
}
