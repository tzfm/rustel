//! Extension presets have no source position in the user's score.
//!
//! Their haps must not carry mini-notation spans. Those spans refer to preset
//! text and can highlight unrelated characters in the score.

use rustel_core::Hap;
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn haps(source: &str, cycles: i128) -> Vec<Hap> {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::int(cycles))
        .expect("query")
}

/// The byte ranges of the score's double-quoted strings, quotes included:
/// the only text a span may legitimately point into.
fn quoted_ranges(source: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut open = None;
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'"' {
            match open.take() {
                Some(start) => ranges.push((start, index + 1)),
                None => open = Some(index),
            }
        }
    }
    ranges
}

fn assert_spans_point_into_the_scores_own_strings(source: &str) {
    let ranges = quoted_ranges(source);
    let haps = haps(source, 2);
    assert!(!haps.is_empty(), "{source} produced nothing");
    for hap in &haps {
        for &(from, to) in &hap.context {
            assert!(
                ranges
                    .iter()
                    .any(|&(start, end)| start <= from && to <= end),
                "{source}: span {from}..{to} points at {:?}, outside every string of the score",
                source.get(from..to)
            );
        }
    }
}

#[test]
fn trancearp_presets_carry_no_spans_of_their_own() {
    assert_spans_point_into_the_scores_own_strings(
        r#"$: note(trancearp(['c','e','g','b'], 0, 0)).s("sawtooth")"#,
    );
    assert_spans_point_into_the_scores_own_strings(
        r#"$: note(trancearp(['c','e','g','b'], 3, 2)).s("sawtooth")"#,
    );
}

#[test]
fn a_gates_presets_carry_no_spans_of_their_own() {
    assert_spans_point_into_the_scores_own_strings(r#"$: s("bd*8").tgate(1, 1, 1)"#);
    assert_spans_point_into_the_scores_own_strings(r#"$: s("bd*8").trancegate(0.5, 42, 4)"#);
}

/// The reference example is what a player copies. It has to pitch the
/// arpeggio: a bare picked value is not a note on either engine.
#[test]
fn the_reference_example_pitches_its_notes() {
    let source = r#"$: note(trancearp(['c','e','g','b'], 0, 0)).s("sawtooth")"#;
    let notes: Vec<String> = haps(source, 1)
        .iter()
        .take(8)
        .map(|hap| {
            hap.value
                .as_object()
                .and_then(|value| value.get("note"))
                .map(|note| note.show())
                .unwrap_or_else(|| panic!("no note in {}", hap.value.show()))
        })
        .collect();
    assert_eq!(notes, ["c", "e", "g", "b", "c", "e", "g", "b"]);
}
