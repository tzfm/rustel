//! The MIDI-map configuration surface is part of semantic bindings itself.
//!
//! These tests do not install the voicings compatibility surface. Direct
//! `JsRuntime` embedders have this contract, and MIDI maps use the bounded,
//! settings-scoped native registry, not a mutable JavaScript mirror.

use std::{sync::atomic::AtomicBool, time::Duration};

use rustel_core::{Value, midimap::MidiMapEntry};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn semantic_runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Result<(), String> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn entry(runtime: &JsRuntime, map: &str, control: &str) -> Option<MidiMapEntry> {
    runtime.with_runtime_settings(|| {
        rustel_core::midimap::midi_map(map).and_then(|mapping| mapping.get(control).cloned())
    })
}

#[test]
fn bare_ccs_are_widened_canonicalised_and_named_maps_stay_separate() {
    let runtime = semantic_runtime();
    evaluate(
        &runtime,
        "midimaps({ mymap: { lpf: 74 }, other: { gain: { ccn: 7, exp: 0.5 } } }); $: s('bd')",
    )
    .expect("map registration");

    let cutoff = entry(&runtime, "mymap", "cutoff").expect("canonical lpf entry");
    assert_eq!(cutoff.ccn, 74);
    assert_eq!(cutoff.min, 0.0);
    assert_eq!(cutoff.max, 1.0);
    assert_eq!(cutoff.exp, 1.0);
    assert!(entry(&runtime, "mymap", "gain").is_none());

    let gain = entry(&runtime, "other", "gain").expect("separate map entry");
    assert_eq!(gain.ccn, 7);
    assert_eq!(gain.exp, 0.5);
    assert!(entry(&runtime, "other", "cutoff").is_none());
}

#[test]
fn public_json_view_reads_the_native_registry() {
    let runtime = semantic_runtime();
    assert_eq!(runtime.midi_maps_json(), None);
    evaluate(&runtime, "midimaps({ named: { lpf: 74 } }); $: s('bd')").expect("map registration");
    let json = runtime.midi_maps_json().expect("native registry JSON");
    assert!(json.contains("\"named\""), "{json}");
    assert!(json.contains("\"cutoff\""), "canonical key missing: {json}");
    assert!(json.contains("\"ccn\":74"), "controller missing: {json}");
}

#[test]
fn default_and_awaitable_registration_return_undefined() {
    let runtime = semantic_runtime();
    evaluate(
        &runtime,
        r#"
          const a = defaultmidimap({ lpf: 74 });
          const b = await midimaps({ named: { gain: 7 } });
          $: pure(a === undefined && b === undefined)
        "#,
    )
    .expect("awaitable map registration");

    assert_eq!(entry(&runtime, "default", "cutoff").unwrap().ccn, 74);
    assert_eq!(entry(&runtime, "named", "gain").unwrap().ccn, 7);
    let haps = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .expect("query result");
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value, Value::Bool(true));
}

#[test]
fn string_loading_is_refused_without_network_access() {
    let runtime = semantic_runtime();
    let error = evaluate(
        &runtime,
        "midimaps('https://example.com/map.json'); $: s('bd')",
    )
    .expect_err("URL loading must be refused");
    assert!(error.contains("needs the network"), "{error}");
}

#[test]
fn an_invalid_mixed_batch_publishes_none_of_its_siblings() {
    let runtime = semantic_runtime();
    let error = evaluate(
        &runtime,
        "midimaps({ good: { lpf: 74 }, bad: { gain: { ccn: 7, min: 1, max: 1 } } }); $: s('bd')",
    )
    .expect_err("invalid batch must refuse");
    assert!(error.contains("scaling range"), "{error}");
    assert!(entry(&runtime, "good", "cutoff").is_none());
    assert!(entry(&runtime, "bad", "gain").is_none());
}

#[test]
fn separate_runtimes_do_not_share_registered_maps() {
    let first = semantic_runtime();
    evaluate(&first, "defaultmidimap({ lpf: 74 }); $: s('bd')").expect("first map");
    assert!(entry(&first, "default", "cutoff").is_some());

    let second = semantic_runtime();
    assert!(entry(&second, "default", "cutoff").is_none());
}
