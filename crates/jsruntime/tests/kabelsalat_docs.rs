//! User-guide examples cross the score, graph and native processing boundaries.

use rustel_core::Value;
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn render_graph(value: &Value, source: &str) -> usize {
    let mut count = 0;
    if let Some(graph) = value.get("workletGraph").and_then(Value::as_str) {
        let program = rustel_kabel::Program::from_json(graph).expect("native graph");
        let mut graph = rustel_kabel::Graph::new(&program, 48_000.0).expect("graph state");
        let mut peak = 0.0_f64;
        for frame in 0..2048 {
            for sample in graph.process([0.25, 0.5], 1.0, f64::from(frame) / 48_000.0) {
                assert!(sample.is_finite(), "nonfinite output: {source}");
                peak = peak.max(sample.abs());
            }
        }
        assert!(peak > 0.0001, "silent example: {source}");
        count += 1;
    }
    if let Some(Value::List(effects)) = value.get("FX") {
        for effect in effects {
            count += render_graph(effect, source);
        }
    }
    count
}

#[test]
fn guide_examples_compile_and_produce_audio() {
    let guide = include_str!("../../../docs/kabelsalat.md");
    let examples = guide_examples(guide);
    let unix = guide.replace("\r\n", "\n");
    let windows = unix.replace('\n', "\r\n");
    assert_eq!(examples, guide_examples(&unix));
    assert_eq!(examples, guide_examples(&windows));
    assert!(
        !examples.is_empty(),
        "the guide must include runnable examples"
    );
    for source in &examples {
        let runtime = JsRuntime::new().expect("runtime");
        runtime.install_semantic_bindings().expect("bindings");
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        let haps = runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        let graphs: usize = haps
            .iter()
            .map(|hap| render_graph(&hap.value, source))
            .sum();
        assert!(graphs > 0, "example must contain a graph: {source}");
        // Each note plays on its own event: every event that starts in this
        // cycle has its onset here, not only the first of one event a cycle.
        let starting = haps
            .iter()
            .filter(|hap| hap.whole_or_part().begin >= Fraction::ZERO)
            .count();
        let onsets = haps.iter().filter(|hap| hap.has_onset()).count();
        assert!(
            onsets > 0 && onsets == starting,
            "example must play each note ({onsets} onsets, {starting} events start): {source}"
        );
    }
}

fn guide_examples(guide: &str) -> Vec<String> {
    guide
        .replace("\r\n", "\n")
        .split("```javascript\n")
        .skip(1)
        .map(|part| part.split_once("```").expect("closed example").0.to_owned())
        .collect()
}
