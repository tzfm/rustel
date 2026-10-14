use rustel_kabel::{Graph, Program};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Fixture {
    kind: String,
    inputs: Vec<Value>,
    expected: f64,
    boolean: bool,
    zero: bool,
}

#[test]
fn matches_upstream_compiler_arithmetic_defaults_and_types() {
    let fixtures: Vec<Fixture> =
        serde_json::from_str(include_str!("fixtures/primitives.json")).unwrap();
    for fixture in fixtures {
        let mut nodes = Vec::new();
        let mut inputs = Vec::new();
        for value in &fixture.inputs {
            if let Some(value) = value.as_bool() {
                nodes.push(json!({"type":"n","value": i32::from(value)}));
                nodes.push(json!({"type":"greater","inputs":[nodes.len()-1]}));
            } else {
                nodes.push(json!({"type":"n","value":value}));
            }
            inputs.push(nodes.len() - 1);
        }
        let result = nodes.len();
        nodes.push(json!({"type":fixture.kind,"inputs": inputs}));
        nodes.push(json!({"type":"output","inputs":[result]}));
        nodes.push(json!({"type":"not","inputs":[result]}));
        nodes.push(json!({"type":"n","value":1}));
        nodes.push(json!({"type":"output","inputs":[nodes.len()-2,nodes.len()-1]}));
        let program = Program::from_json(&json!({"nodes":nodes}).to_string()).unwrap();
        let mut graph = Graph::new(&program, 48000.0).unwrap();
        let output = graph.process([0.0; 2], 1.0, 0.0);
        let tolerance = 2e-12 * fixture.expected.abs().max(1.0);
        assert!(
            (output[0] - fixture.expected).abs() <= tolerance,
            "{} {:?}: {:?} != {}",
            fixture.kind,
            fixture.inputs,
            output,
            fixture.expected
        );
        assert_eq!(
            output[1],
            f64::from(fixture.zero),
            "{} {:?} zero/type differs (Boolean {})",
            fixture.kind,
            fixture.inputs,
            fixture.boolean
        );
    }
}
