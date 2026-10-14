//! Native construction retains the upstream graph's order and shared edges.

use rustel_core::Value;
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

#[test]
fn score_graphs_match_upstream_construction() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/kabelsalat_graphs.json")).expect("fixtures");
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    for fixture in fixtures.as_array().expect("cases") {
        if fixture["builder_only"].as_bool() == Some(true) {
            continue;
        }
        let name = fixture["name"].as_str().expect("name");
        let source = fixture["source"]
            .as_str()
            .expect("source")
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        runtime
            .evaluate_score(
                &format!("freq(220).worklet('{source}', 3)"),
                &TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let haps = runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(haps.len(), 1, "{name}");
        let graph = haps[0]
            .value
            .get("workletGraph")
            .and_then(Value::as_str)
            .expect("compiled graph");
        let graph: serde_json::Value = serde_json::from_str(graph).expect("graph data");
        let actual = graph["nodes"].as_array().expect("actual nodes");
        let expected = fixture["graph"]["nodes"]
            .as_array()
            .expect("expected nodes");
        assert_eq!(actual.len(), expected.len(), "{name}: node count");
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(actual["type"], expected["type"], "{name}: node {index}");
            assert_eq!(actual["inputs"], expected["inputs"], "{name}: node {index}");
            match (actual["value"].as_f64(), expected["value"].as_f64()) {
                (Some(actual), Some(expected)) => assert!(
                    (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
                    "{name}: node {index}: {actual} != {expected}"
                ),
                _ => assert_eq!(actual["value"], expected["value"], "{name}: node {index}"),
            }
        }
        runtime.run_gc();
    }
}
