use rustel_kabel::{Graph, MAX_INPUTS, MAX_NODES, Program};
use serde_json::{Value, json};

fn program(nodes: Value) -> Program {
    Program::from_json(&json!({"nodes": nodes}).to_string()).unwrap()
}
fn graph(nodes: Value) -> Graph {
    Graph::new(&program(nodes), 48000.0).unwrap()
}
fn sample(nodes: Value) -> [f64; 2] {
    graph(nodes).process([0.0; 2], 1.0, 0.0)
}

#[test]
fn program_ids_distinguish_compilations_without_changing_content_equality() {
    let first = program(json!([{"type":"time"}]));
    let copy = first;
    let second = program(json!([{"type":"time"}]));
    assert_eq!(copy.id(), first.id());
    assert_ne!(second.id(), first.id());
    assert_eq!(first, second);
    assert_eq!(std::mem::size_of::<Option<rustel_kabel::ProgramId>>(), 8);
}

#[test]
fn stereo_outputs_add_and_audio_input_uses_left_channel() {
    let mut graph = graph(json!([
        {"type":"audioin"}, {"type":"n","value":1},
        {"type":"n","value":0.25}, {"type":"output","inputs":[0]},
        {"type":"output","inputs":[2]}, {"type":"output","inputs":[2,1]}
    ]));
    assert_eq!(graph.process([0.5, -0.75], 1.0, 0.0), [0.75, 0.25]);
    assert_eq!(graph.process([-0.5, 0.75], 1.0, 0.0), [-0.25, 0.25]);
}

#[test]
fn no_output_is_silent_and_unused_input_does_not_leak() {
    let mut graph = graph(json!([{"type":"audioin"}, {"type":"n","value":0.5}]));
    assert_eq!(graph.process([1.0; 2], 1.0, 0.0), [0.0; 2]);
}

#[test]
fn scope_string_ids_are_metadata_and_cannot_be_used_as_signals() {
    let nodes = json!([
        {"type":"n","value":0.25}, {"type":"n","value":"oscilloscope"},
        {"type":"n","value":1}, {"type":"n","value":0},
        {"type":"scope","inputs":[0,1,2,3]}, {"type":"output","inputs":[4]}
    ]);
    assert_eq!(sample(nodes.clone()), [0.25, 0.0]);
    let mut reused = nodes;
    reused
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"output","inputs":[1]}));
    let error = Program::from_json(&json!({"nodes":reused}).to_string()).unwrap_err();
    assert!(error.contains("only supported as a scope ID"));
}

#[test]
fn forward_feedback_reads_previous_sample_and_constants_are_immediate() {
    let mut graph = graph(json!([
        {"type":"add","inputs":[1,2]}, {"type":"thru","inputs":[0]},
        {"type":"n","value":0.25}, {"type":"output","inputs":[0]}
    ]));
    for expected in [0.25, 0.5, 0.75, 1.0] {
        assert_eq!(graph.process([0.0; 2], 0.0, 0.0), [expected, 0.0]);
    }
    graph.reset();
    assert_eq!(graph.process([0.0; 2], 0.0, 0.0), [0.25, 0.0]);
}

#[test]
fn source_registers_retain_output_between_samples() {
    let mut graph = graph(json!([
        {"type":"src"}, {"type":"n","value":0.2},
        {"type":"add","inputs":[0,1]}, {"type":"output","inputs":[2]}
    ]));
    for index in 1..=5 {
        let output = graph.process([0.0; 2], 0.0, 0.0);
        assert!((output[0] - f64::from(index) * 0.2).abs() < 1e-14);
    }
}

#[test]
fn gate_and_time_are_explicit_voice_controls() {
    let mut graph = graph(json!([
        {"type":"gate"}, {"type":"time"}, {"type":"n","value":1},
        {"type":"output","inputs":[0]}, {"type":"output","inputs":[1,2]}
    ]));
    assert_eq!(graph.process([99.0; 2], 0.0, 0.125), [0.0, 0.125]);
    assert_eq!(graph.process([99.0; 2], 1.0, 1.5), [1.0, 1.5]);
}

#[test]
fn comparison_results_keep_javascript_boolean_identity() {
    // ifelse requires numeric one. True from greater is a distinct value.
    for (control, expected) in [("greater", 0.75), ("bool", 0.25)] {
        assert_eq!(
            sample(json!([
                {"type":"n","value":1}, {"type":control,"inputs":[0]},
                {"type":"n","value":0.25}, {"type":"n","value":0.75},
                {"type":"ifelse","inputs":[1,2,3]}, {"type":"output","inputs":[4]}
            ])),
            [expected, 0.0]
        );
    }
    for operation in ["not", "bool"] {
        assert_eq!(
            sample(json!([
                {"type":"n","value":0}, {"type":"greater","inputs":[0,0]},
                {"type":"thru","inputs":[1]}, {"type":operation,"inputs":[2]},
                {"type":"output","inputs":[3]}
            ])),
            [f64::from(operation == "bool"), 0.0]
        );
    }
}

#[test]
fn held_and_selected_values_keep_boolean_identity() {
    for node in [
        json!({"type":"hold","inputs":[1,0]}),
        json!({"type":"pick","inputs":[2,1]}),
        json!({"type":"seq","inputs":[0,1]}),
    ] {
        assert_eq!(
            sample(json!([
                {"type":"n","value":1}, {"type":"greater","inputs":[0]},
                {"type":"n","value":0}, node, {"type":"not","inputs":[3]},
                {"type":"output","inputs":[4]}
            ])),
            [0.0, 0.0]
        );
    }
}

#[test]
fn nonfinite_intermediates_keep_javascript_semantics_but_output_is_finite() {
    assert_eq!(
        sample(json!([
            {"type":"n","value":0}, {"type":"div","inputs":[0,0]},
            {"type":"n","value":1}, {"type":"min","inputs":[1,2]},
            {"type":"bool","inputs":[3]}, {"type":"output","inputs":[4]},
            {"type":"output","inputs":[1,2]}
        ])),
        [1.0, 0.0]
    );
}

#[test]
fn delay_reset_does_not_replay_previous_voice_history() {
    let program = program(json!([
        {"type":"audioin"}, {"type":"n","value":2.0/48000.0},
        {"type":"delay","inputs":[0,1]}, {"type":"output","inputs":[2]}
    ]));
    let mut graph = Graph::new(&program, 48000.0).unwrap();
    assert_eq!(program.delay_count(), 1);
    assert!(graph.heap_bytes() >= 48000 * 10 * 4);
    for _ in 0..16 {
        graph.process([0.75; 2], 1.0, 0.0);
    }
    graph.reset();
    for _ in 0..8 {
        assert_eq!(graph.process([0.0; 2], 1.0, 0.0), [0.0; 2]);
    }
    assert_eq!(graph.process([0.5; 2], 1.0, 0.0), [0.0; 2]);
    assert_eq!(graph.process([0.0; 2], 1.0, 0.0), [0.0; 2]);
    assert_eq!(graph.process([0.0; 2], 1.0, 0.0), [0.5, 0.0]);
}

#[test]
fn reset_delay_is_zero_until_each_ring_position_is_written_again() {
    let program = program(json!([
        {"type":"audioin"}, {"type":"raw","value":"1/0"},
        {"type":"delay","inputs":[0,1]}, {"type":"output","inputs":[2]}
    ]));
    let mut graph = Graph::new(&program, 1.0).unwrap();
    for _ in 0..25 {
        graph.process([0.75; 2], 1.0, 0.0);
    }
    graph.reset();
    for _ in 0..9 {
        assert_eq!(graph.process([0.5; 2], 1.0, 0.0), [0.0; 2]);
    }
    assert_eq!(graph.process([0.5; 2], 1.0, 0.0), [0.5, 0.0]);
}

#[test]
fn trigger_keeps_high_state_across_nan_input() {
    let mut graph = graph(json!([
        {"type":"audioin"}, {"type":"trig","inputs":[0]},
        {"type":"output","inputs":[1]}
    ]));
    for (input, expected) in [
        (1.0, 1.0),
        (f64::NAN, 0.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (1.0, 1.0),
    ] {
        assert_eq!(graph.process([input; 2], 1.0, 0.0), [expected, 0.0]);
    }
}

#[test]
fn empty_sequence_emits_numeric_zero_only_on_rising_clock() {
    let mut graph = graph(json!([
        {"type":"audioin"}, {"type":"seq","inputs":[0]},
        {"type":"not","inputs":[1]}, {"type":"output","inputs":[2]}
    ]));
    for (clock, expected) in [(1.0, 0.0), (-1.0, 0.0), (1.0, 1.0), (1.0, 0.0)] {
        assert_eq!(graph.process([clock; 2], 1.0, 0.0), [expected, 0.0]);
    }
}

#[test]
fn biquad_boolean_filter_type_retains_initial_bypass_coefficients() {
    assert_eq!(
        sample(json!([
            {"type":"n","value":0.25}, {"type":"raw","value":"false"},
            {"type":"qf","inputs":[0,1]}, {"type":"output","inputs":[2]}
        ])),
        [0.25, 0.0]
    );
}

#[test]
fn prepared_storage_can_restart_different_programs_without_growing() {
    let delayed = program(json!([{"type":"delay"}]));
    let constant = program(json!([{"type":"n","value":0.2}, {"type":"output","inputs":[0]}]));
    let mut graph = Graph::new(&constant, 48000.0).unwrap();
    assert!(!graph.can_reuse(&delayed));
    assert!(!graph.restart(&delayed));
    assert_eq!(graph.program(), &constant);
    let mut graph = Graph::new(&delayed, 48000.0).unwrap();
    let bytes = graph.heap_bytes();
    assert!(graph.restart(&constant));
    assert_eq!(graph.process([0.0; 2], 0.0, 0.0), [0.2, 0.0]);
    assert_eq!(graph.heap_bytes(), bytes);
    assert!(graph.restart(&delayed));
    assert_eq!(graph.process([0.0; 2], 0.0, 0.0), [0.0; 2]);
}

#[test]
fn malformed_graphs_fail_before_audio_processing() {
    for source in [
        "{}",
        r#"{"nodes":[]}"#,
        r#"{"nodes":[{"type":"unknown"}]}"#,
        r#"{"nodes":[{"type":"n","value":"text"}]}"#,
        r#"{"nodes":[{"type":"n","value":1e999}]}"#,
        r#"{"nodes":[{"type":"thru","inputs":[1]}]}"#,
        r#"{"nodes":[{"type":"thru","inputs":[-1]}]}"#,
        r#"{"nodes":[{"type":"n","value":16},{"type":"output","inputs":[0,0]}]}"#,
        r#"{"nodes":[{"type":"n","value":-1},{"type":"src","inputs":[0]}]}"#,
        r#"{"nodes":[{"type":"delay","inputs":[0,0,0]}]}"#,
    ] {
        assert!(Program::from_json(source).is_err(), "accepted {source}");
    }
    for kind in ["cc", "midicc", "midifreq", "midigate", "midivel"] {
        let error = Program::from_json(&json!({"nodes":[{"type":kind}]}).to_string()).unwrap_err();
        assert!(error.contains("live control input"));
    }
    let nodes = vec![json!({"type":"time"}); MAX_NODES + 1];
    assert!(Program::from_json(&json!({"nodes":nodes}).to_string()).is_err());
    let edges = vec![0; MAX_INPUTS + 1];
    assert!(
        Program::from_json(&json!({"nodes":[{"type":"add","inputs":edges}]}).to_string()).is_err()
    );
}

#[test]
fn sample_rate_and_memory_are_bounded_before_allocation() {
    let program = program(json!([{"type":"delay"}]));
    for rate in [0.0, -1.0, f64::NAN, f64::INFINITY, 384001.0] {
        assert!(Graph::new(&program, rate).is_err());
    }
    let program =
        Program::from_json(&json!({"nodes":vec![json!({"type":"delay"}); MAX_NODES]}).to_string())
            .unwrap();
    assert!(
        Graph::new(&program, 48000.0)
            .unwrap_err()
            .contains("256 MiB")
    );
}

fn expression(source: &str, input: f64, kind: &str) -> [f64; 2] {
    sample(json!([
        {"type":"n","value":input}, {"type":kind,"value":source,"inputs":[0]},
        {"type":"output","inputs":[1]}
    ]))
}

#[test]
fn expressions_execute_arithmetic_bitwise_and_math_operations() {
    for (source, input, expected) in [
        ("$input * 2 + Math.sin(Math.PI / 2)", 0.25, 1.5),
        ("2 ** 3 ** 2", 0.0, 512.0),
        ("t>>1|t<<2", 9.0, 36.0),
        ("-1 >>> 0", 0.0, 4294967295.0),
        ("~4294967296", 0.0, -1.0),
        ("t > 3 ? Math.sqrt(t) : 99", 9.0, 3.0),
        ("0 || t && 0.5", 9.0, 0.5),
        ("Math.max(Math.abs(-2),Math.min(3,4))", 0.0, 3.0),
        ("Math.floor(t) /* sample clock */ + 1 // done", 2.75, 3.0),
    ] {
        assert_eq!(
            expression(source, input, "raw"),
            [expected, 0.0],
            "{source}"
        );
    }
    assert_eq!(expression("t", 255.0, "bytebeat"), [1.0, 0.0]);
    assert_eq!(expression("t", 256.0, "bytebeat"), [-1.0, 0.0]);
    assert_eq!(expression("t", -1.0, "bytebeat"), [1.0, 0.0]);
}

#[test]
fn expressions_preserve_boolean_identity_in_equality_and_selection() {
    for (source, expected) in [
        ("(1 < 2) === 1", 0.0),
        ("(1 < 2) !== 1", 1.0),
        ("(1 < 2) == 1", 1.0),
        ("(1 < 2) != 1", 0.0),
        ("true === true", 1.0),
        ("false === 0", 0.0),
        ("false == 0", 1.0),
        ("(true && false) === false", 1.0),
        ("(false || true) === true", 1.0),
        ("(true ? false : 0) === false", 1.0),
        ("(+true) === 1", 1.0),
        ("(0 / 0) === (0 / 0)", 0.0),
        ("(0 / 0) !== (0 / 0)", 1.0),
    ] {
        assert_eq!(expression(source, 0.0, "raw"), [expected, 0.0], "{source}");
    }
}

#[test]
fn expressions_preserve_javascript_signed_zero_rounding_and_nonfinite_math() {
    for (source, expected) in [
        ("1 / Math.round(-0.5) < 0", 1.0),
        ("Math.round(0.49999999999999994)", 0.0),
        ("1 / Math.min(0,-0) < 0", 1.0),
        ("1 / Math.max(0,-0) > 0", 1.0),
        ("Math.sign(-0) === 0", 1.0),
        ("(1 ** (1 / 0)) === (1 ** (1 / 0))", 0.0),
        ("Math.min(0 / 0, 1) === 1", 0.0),
    ] {
        assert_eq!(expression(source, 0.0, "raw"), [expected, 0.0], "{source}");
    }
}

#[test]
fn expression_booleans_are_distinct_from_numbers() {
    assert_eq!(
        sample(json!([
            {"type":"raw","value":"1 < 2"}, {"type":"n","value":0.25},
            {"type":"n","value":0.75}, {"type":"ifelse","inputs":[0,1,2]},
            {"type":"output","inputs":[3]}
        ])),
        [0.75, 0.0]
    );
}

#[test]
fn unsafe_and_unbounded_expressions_are_rejected() {
    for source in [
        "globalThis",
        "Math.random()",
        "t=1",
        "(()=>1)()",
        "[1,2]",
        "while(true){}",
        "t.foo",
        "",
    ] {
        assert!(
            Program::from_json(&json!({"nodes":[{"type":"raw","value":source}]}).to_string())
                .is_err(),
            "accepted {source}"
        );
    }
    let source = std::iter::repeat_n("1", 256).collect::<Vec<_>>().join("+");
    assert!(
        Program::from_json(&json!({"nodes":[{"type":"raw","value":source}]}).to_string()).is_err()
    );
}
