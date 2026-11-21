//! A score of the native tier is in play with no wrapper, and trades places
//! with a script score in both directions.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Two notes a cycle, for the native tier.
const NATIVE: &str = "note(\"c e\")";

/// Four notes a cycle. An index into an array is JavaScript alone.
const SCRIPT: &str = "note(\"c e g a\").fast([1][0])";

/// Stage `source` and put it in play, as a session does before its probe.
fn evaluate(runtime: &JsRuntime, source: &str) {
    runtime
        .evaluate_score_candidate_with_effects_cancellable(
            source,
            None,
            &TranspileOptions::default(),
            Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .expect("a score");
}

/// How many haps the score in play answers in the first cycle.
fn haps_a_cycle(runtime: &JsRuntime) -> usize {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("haps")
        .len()
}

#[test]
fn a_native_score_plays_with_no_script_engine() {
    let runtime = JsRuntime::deferred(None);
    assert!(!runtime.has_active_pattern());
    evaluate(&runtime, NATIVE);
    assert!(runtime.evaluated_natively());
    assert_eq!(haps_a_cycle(&runtime), 2);
    runtime.snapshot_active_as_last_good();
    runtime.restore_last_good_active().expect("the last score");
    assert_eq!(haps_a_cycle(&runtime), 2);
    runtime.clear_active();
    assert!(!runtime.has_active_pattern());
    assert!(!runtime.script_engine_started());
}

#[test]
fn a_native_score_comes_back_when_a_script_score_is_refused() {
    let runtime = JsRuntime::deferred(None);
    evaluate(&runtime, NATIVE);
    runtime.snapshot_active_as_last_good();
    evaluate(&runtime, SCRIPT);
    assert!(!runtime.evaluated_natively());
    assert!(runtime.script_engine_started());
    assert_eq!(haps_a_cycle(&runtime), 4);
    runtime.restore_last_good_active().expect("the last score");
    assert_eq!(haps_a_cycle(&runtime), 2);
}

#[test]
fn a_script_score_comes_back_when_a_native_score_is_refused() {
    let runtime = JsRuntime::deferred(None);
    evaluate(&runtime, SCRIPT);
    runtime.snapshot_active_as_last_good();
    evaluate(&runtime, NATIVE);
    assert!(runtime.evaluated_natively());
    assert_eq!(haps_a_cycle(&runtime), 2);
    runtime.restore_last_good_active().expect("the last score");
    assert_eq!(haps_a_cycle(&runtime), 4);
    // The native score takes the place again, and stays after its snapshot.
    evaluate(&runtime, NATIVE);
    runtime.snapshot_active_as_last_good();
    assert_eq!(haps_a_cycle(&runtime), 2);
}

#[test]
fn a_query_of_a_native_score_is_bounded_as_a_query_of_a_script_score_is() {
    use rustel_core::QueryLimit;
    use rustel_jsruntime::QueryError;
    for (source, haps) in [(NATIVE, 2), (SCRIPT, 4)] {
        let runtime = JsRuntime::deferred(None);
        evaluate(&runtime, source);
        let ask = |budget: Duration, stop: bool| {
            let (begin, end) = (Fraction::ZERO, Fraction::ONE);
            runtime.query_cancellable(Slot::Active, 0, begin, end, budget, &AtomicBool::new(stop))
        };
        let stopped = ask(Duration::from_secs(1), true);
        assert!(
            matches!(stopped, Err(QueryError::Limit(QueryLimit::Cancelled))),
            "{source}: {stopped:?}"
        );
        let no_time = ask(Duration::ZERO, false);
        assert!(
            matches!(
                no_time,
                Err(QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 0 }))
            ),
            "{source}: {no_time:?}"
        );
        assert_eq!(
            ask(Duration::from_secs(1), false).expect("haps").len(),
            haps
        );
    }
    // A native walk past its time is refused by the deadline of the query.
    let runtime = JsRuntime::deferred(None);
    evaluate(&runtime, "s(\"bd*40000\")");
    assert!(runtime.evaluated_natively());
    let late = runtime.query_cancellable(
        Slot::Active,
        0,
        Fraction::ZERO,
        Fraction::ONE,
        Duration::from_nanos(1),
        &AtomicBool::new(false),
    );
    assert!(
        matches!(
            late,
            Err(QueryError::Limit(QueryLimit::QueryDeadline { .. }))
        ),
        "{:?}",
        late.map(|haps| haps.len())
    );
}
