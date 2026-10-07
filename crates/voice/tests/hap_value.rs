use rustel_audio::OnsetEvent;
use rustel_core::Value;
use rustel_voice::{
    BundledOnly, SampleLookup, SampleResolution, VoiceError, resolve_hap_value,
    resolve_voice_with_samples_detailed,
};
use serde_json::json;

/// A library that knows `bd` in two banks, and otherwise only the bundled `bd`.
struct TwoBanks;

impl SampleLookup for TwoBanks {
    fn resolve(&self, s: &str, n: f64, midi: f64) -> SampleResolution {
        match s {
            "RolandTR909_bd" | "9000_bd" => SampleResolution::Found {
                id: rustel_audio::SampleId(7),
                transpose: midi - 36.0 + n,
                duration_secs: 0.5,
                loop_secs: None,
                envelope_peak: 1.0,
                soundfont: false,
            },
            _ => BundledOnly.resolve(s, n, midi),
        }
    }
}

fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::object(entries.map(|(key, value)| (key.to_owned(), value)))
}

fn text(value: &str) -> Value {
    Value::Str(value.to_owned())
}

type Resolved = Result<OnsetEvent, VoiceError>;

/// The core value's result beside the JSON entry point's result for the
/// same control map written as JSON.
fn both(
    value: &Value,
    json: serde_json::Value,
    samples: &dyn SampleLookup,
) -> (Resolved, Resolved) {
    (
        resolve_hap_value(value, 3, 0.25, 1.5, 48_000, 0.5, samples),
        resolve_voice_with_samples_detailed(&json, 3, 0.25, 1.5, 48_000, 0.5, samples),
    )
}

#[test]
fn a_sample_map_resolves_as_its_json_does() {
    let (value, json) = both(
        &map([
            ("s", text("bd")),
            ("n", Value::F64(0.0)),
            ("gain", Value::F64(0.5)),
            ("speed", Value::F64(-1.5)),
            ("pan", Value::Bool(true)),
        ]),
        json!({"s": "bd", "n": 0, "gain": 0.5, "speed": -1.5, "pan": true}),
        &BundledOnly,
    );
    assert!(
        json.as_ref().is_ok_and(|onset| onset.sample.is_some()),
        "{json:?}"
    );
    assert_eq!(value, json);
}

#[test]
fn a_synth_map_with_nested_modulators_resolves_as_its_json_does() {
    let lfo = |rate| map([("control", text("cutoff")), ("rate", Value::F64(rate))]);
    let (value, json) = both(
        &map([
            ("s", text("supersaw")),
            ("note", Value::F64(60.5)),
            ("cutoff", Value::F64(800.0)),
            ("lfo", map([("b", lfo(2.0)), ("a", lfo(1.0))])),
            ("postgain", Value::Null),
        ]),
        json!({
            "s": "supersaw",
            "note": 60.5,
            "cutoff": 800,
            "lfo": {
                "b": {"control": "cutoff", "rate": 2},
                "a": {"control": "cutoff", "rate": 1}
            },
            "postgain": null
        }),
        &BundledOnly,
    );
    assert!(
        json.as_ref().is_ok_and(|onset| onset.synth.is_some()),
        "{json:?}"
    );
    assert_eq!(value, json);
}

#[test]
fn a_banked_map_resolves_as_its_json_does() {
    for (bank, bank_json) in [
        (text("RolandTR909"), json!("RolandTR909")),
        (Value::F64(9000.0), json!(9000)),
    ] {
        let (value, json) = both(
            &map([("s", text("bd")), ("bank", bank), ("n", Value::F64(1.0))]),
            json!({"s": "bd", "bank": bank_json, "n": 1}),
            &TwoBanks,
        );
        assert!(
            json.as_ref().is_ok_and(|onset| onset
                .sample
                .is_some_and(|sample| sample.sample == rustel_audio::SampleId(7))),
            "{json:?}"
        );
        assert_eq!(value, json);
    }
}

#[test]
fn an_invalid_value_is_refused_as_its_json_is() {
    let cases = [
        (
            map([("s", text("sine")), ("gain", text("loud"))]),
            json!({"s": "sine", "gain": "loud"}),
        ),
        (map([("s", text("nosuch"))]), json!({"s": "nosuch"})),
        (
            map([("s", text("bd")), ("bank", Value::List(Vec::new()))]),
            json!({"s": "bd", "bank": []}),
        ),
        (text("c4"), json!("c4")),
        (Value::F64(60.0), json!(60)),
        (Value::Null, json!(null)),
    ];
    for (core, literal) in cases {
        let (value, json) = both(&core, literal, &TwoBanks);
        assert!(json.is_err(), "{json:?}");
        assert_eq!(value, json);
    }
}

#[test]
fn a_value_with_no_json_form_is_refused_as_not_an_object() {
    let refused = resolve_hap_value(&Value::Undefined, 3, 0.25, 1.5, 48_000, 0.5, &BundledOnly)
        .expect_err("undefined is not a control map");
    let VoiceError::InvalidControl(message) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        message.starts_with("expected hap.value to be an object, but got \"undefined\""),
        "{message}"
    );
}
