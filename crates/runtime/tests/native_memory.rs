//! A score that the native tier evaluates keeps the memory of one graph.
//!
//! The test reads the count of graph nodes of the process, so it is alone
//! in its file.

use rustel_runtime::Session;

#[test]
fn a_native_score_that_replaces_a_score_frees_the_graph_it_replaces() {
    let mut session = Session::new().expect("a session");
    // The same lanes each time, with one number that moves.
    let score = |round: usize| {
        format!(
            "stack(note(\"c e g a\").s(\"sawtooth\").lpf(sine.range(200, {})), s(\"bd*4, hh*8\").gain(0.8)).fast(2)",
            800 + round
        )
    };
    session.evaluate(&score(0)).expect("a score");
    assert!(session.evaluated_natively());
    session.evaluate(&score(1)).expect("a score");
    let held = rustel_core::live_node_count();
    for round in 2..40 {
        session.evaluate(&score(round)).expect("a score");
        assert!(session.evaluated_natively());
    }
    assert_eq!(
        rustel_core::live_node_count(),
        held,
        "each evaluation kept the graph that it replaced"
    );
}
