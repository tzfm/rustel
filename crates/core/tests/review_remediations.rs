use std::panic::{AssertUnwindSafe, catch_unwind};

use rustel_core::{
    CallbackHost, Hap, QueryArcOutcome, QueryLimit, State, TimeSpan, Value, pure, signal,
    signal_query_error, stack, take_query_callback_failure, take_query_error, with_callback_host,
};
use rustel_fraction::Fraction;

#[test]
fn hap_duration_uses_fraction_js_number_reconstruction() {
    let hap = Hap::new(
        Some(TimeSpan::new(Fraction::ZERO, Fraction::ONE)),
        TimeSpan::new(Fraction::ZERO, Fraction::ONE),
        Value::object([
            ("duration".into(), Value::F64(1.0 / 3.0)),
            ("clip".into(), Value::F64(1.0 / 3.0)),
        ]),
    );

    assert_eq!(hap.duration(), Fraction::new(1, 9));
    assert_eq!(hap.end_clipped(), Fraction::new(1, 9));
}

/// Pins that a `duration` times `clip` product outside the native fraction
/// range keeps the un-clipped duration instead of panicking.
#[test]
fn hap_duration_ignores_a_clip_whose_product_leaves_the_fraction_range() {
    let whole = Some(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    let part = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    let extreme = Fraction::from_f64(1e29).expect("1e29 converts on its own");
    let clipped = Hap::new(
        whole,
        part,
        Value::object([
            ("duration".into(), Value::F64(1e29)),
            ("clip".into(), Value::F64(1e29)),
        ]),
    );
    assert_eq!(clipped.duration(), extreme);
    assert_eq!(clipped.end_clipped(), extreme);
    assert!(clipped.is_active(Fraction::ZERO));

    let ordinary = Hap::new(
        whole,
        part,
        Value::object([
            ("duration".into(), Value::F64(0.5)),
            ("clip".into(), Value::F64(2.0)),
        ]),
    );
    assert_eq!(ordinary.duration(), Fraction::ONE);
}

#[test]
fn span_cycle_limit_counts_fractional_edge_buckets_exactly() {
    assert_eq!(
        TimeSpan::new(Fraction::new(1, 2), Fraction::new(3, 2))
            .span_cycles()
            .len(),
        2
    );

    let pattern = pure(Value::Str("a".into()));
    let state = State::new(TimeSpan::new(
        Fraction::ZERO,
        Fraction::new(2 * rustel_core::MAX_QUERY_SPAN_CYCLES + 1, 2),
    ));
    assert!(
        matches!(
            pattern.try_query_state(&state),
            Err(QueryLimit::QuerySpan { cycles })
                if cycles == rustel_core::MAX_QUERY_SPAN_CYCLES
        ),
        "the fractional final cycle must count toward the exact span limit"
    );
}

#[test]
fn query_thread_locals_restore_after_a_caught_native_panic() {
    signal_query_error(|| "outer query error".into());
    // A caller-supplied `with_query_time` closure crosses the native Fraction
    // boundary unchecked. `2 * i128::MAX` panics there, and this test needs
    // that unwind.
    let overflowing =
        pure(Value::Str("a".into())).with_query_time(|t| t.mul(Fraction::int(i128::MAX)));
    let state = State::new(TimeSpan::new(Fraction::ZERO, Fraction::int(2)));

    assert!(
        catch_unwind(AssertUnwindSafe(|| overflowing.try_query_state(&state))).is_err(),
        "the fixture must cross the documented native Fraction boundary"
    );
    assert_eq!(
        take_query_error().as_deref(),
        Some("outer query error"),
        "the nested query error boundary leaked or discarded outer state"
    );

    let one_hap = pure(Value::Str("clean".into()));
    let one_cycle = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
    assert!(
        matches!(
            one_hap.try_query_state_with_budget(&one_cycle, 0),
            Err(QueryLimit::HapBudget { budget: 0 })
        ),
        "the unwound query left its old hap budget installed"
    );
    assert_eq!(
        one_hap
            .try_query_state_with_budget(&one_cycle, 1)
            .expect("the next independent query must be healthy")
            .len(),
        1
    );
}

#[test]
fn sorted_analog_throw_is_caught_without_poisoning_the_next_query() {
    let analogs = stack(vec![signal::sine(), signal::cosine()]);
    assert!(
        analogs
            .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .is_empty(),
        "the untyped sorted boundary must preserve throw-as-silence"
    );
    assert_eq!(
        take_query_error(),
        None,
        "the untyped sorted query leaked its comparator throw"
    );

    assert!(
        analogs
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("an analog comparator throw is not a resource refusal")
            .is_empty(),
        "sortHapsByPart's observable analog throw must become silence"
    );
    assert_eq!(
        take_query_error(),
        None,
        "the sorted query leaked its comparator throw into the next score"
    );

    // The outcome-typed form reports the comparator throw as `Thrown`. It
    // must not return empty haps and leave the error pending on this thread.
    let outcome = analogs
        .try_query_arc_sorted_outcome_with_budget(
            Fraction::ZERO,
            Fraction::ONE,
            rustel_core::DEFAULT_HAP_BUDGET,
        )
        .expect("an analog comparator throw is not a resource refusal");
    assert!(
        matches!(outcome, QueryArcOutcome::Thrown(ref message) if message.contains("sortHapsByPart")),
        "the outcome-typed sorted query swallowed the comparator throw: {outcome:?}"
    );
    assert_eq!(
        take_query_error(),
        None,
        "the outcome-typed sorted query leaked its comparator throw"
    );

    let clean = pure(Value::Str("clean".into()));
    assert_eq!(
        clean
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("the next independent query must remain healthy")
            .len(),
        1
    );
}

/// A `filterValues` predicate that always throws.
struct ThrowingPredicate;

impl CallbackHost for ThrowingPredicate {
    fn call_value(&self, _id: rustel_core::CallbackId, _value: &Value) -> Result<Value, String> {
        Err("log is not defined".into())
    }

    fn call_query(&self, _id: rustel_core::CallbackId, _state: &State) -> Result<Vec<Hap>, String> {
        Ok(Vec::new())
    }

    fn call_value_predicate(
        &self,
        _id: rustel_core::CallbackId,
        _value: &Value,
    ) -> Result<bool, String> {
        Err("log is not defined".into())
    }
}

fn one_cycle() -> State {
    State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
}

#[test]
fn a_throwing_filter_publishes_a_contained_failure_and_keeps_the_hap() {
    let pattern = pure(Value::Str("bd".into())).filter_values_js(1);
    let outcome = with_callback_host(&ThrowingPredicate, || {
        pattern.query_arc_outcome(&one_cycle())
    })
    .expect("a contained failure is not a resource refusal");
    match outcome {
        QueryArcOutcome::Haps(haps) => {
            assert_eq!(haps.len(), 1, "fail-open must keep the hap: {haps:?}");
        }
        QueryArcOutcome::Thrown(message) => {
            panic!("the contained failure aborted the query: {message}")
        }
    }
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("callback 1 failed: log is not defined"),
        "the query's caller must see the contained failure once"
    );
}

#[test]
fn a_fatal_throw_discards_the_contained_filter_failure() {
    // Filter throws first (fail-open would keep the hap), then `.add("x")`
    // hits parseNumeral and the queryArc aborts. The window is silent; a
    // leftover "kept playing" publication would make the session report both.
    let filtered = pure(Value::Str("bd".into())).filter_values_js(1);
    let pattern = rustel_core::compose::compose(
        &filtered,
        &pure(Value::Str("x".into())),
        rustel_core::compose::ComposeOp::Add,
        rustel_core::compose::Alignment::In,
    );
    let outcome = with_callback_host(&ThrowingPredicate, || {
        pattern.query_arc_outcome(&one_cycle())
    })
    .expect("a user throw is not a resource refusal");
    assert!(
        matches!(outcome, QueryArcOutcome::Thrown(_)),
        "parseNumeral must still abort the query: {outcome:?}"
    );
    assert_eq!(
        take_query_callback_failure(),
        None,
        "a silent window must not also report that a filter kept playing"
    );
}

/// A tiny negative arp index wraps to the first voice, as JavaScript's
/// `((i % n) + n) % n` does, and never selects past the chord.
#[test]
fn arp_with_a_tiny_negative_index_selects_the_first_voice() {
    let chord = stack(
        ["c", "e", "g"]
            .map(|note| pure(Value::Str(note.into())))
            .to_vec(),
    );
    let haps = rustel_core::arp(chord, pure(Value::F64(-1e-20)))
        .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
        .expect("arp queries");
    let values: Vec<_> = haps.into_iter().map(|hap| hap.value).collect();
    assert_eq!(values, vec![Value::Str("c".into())]);
}

/// A tiny negative anchored scale step wraps to degree 0 one octave down, as
/// JavaScript's `_mod` does, and never indexes past the scale.
#[test]
fn anchored_scale_with_a_tiny_negative_step_selects_the_first_degree() {
    let step = pure(Value::object([
        ("n".into(), Value::F64(-1e-20)),
        ("anchor".into(), Value::F64(60.0)),
    ]));
    let haps = rustel_core::combinators::scale(&step, Value::Str("C major".into()))
        .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
        .expect("scale queries");
    assert_eq!(haps.len(), 1, "{haps:?}");
    let Value::Object(map) = &haps[0].value else {
        panic!("an object step stays an object: {haps:?}");
    };
    assert_eq!(map.get("note"), Some(&Value::F64(48.0)));
}

/// A `beat` window outside the native fraction range refuses as
/// `NativeFraction { operation: "beat" }` instead of panicking.
#[test]
fn beat_with_a_division_past_the_fraction_range_refuses() {
    let division = Fraction::from_f64(1e38).expect("1e38 converts on its own");
    let pattern = rustel_core::combinators::beat(
        &pure(Value::Str("bd".into())),
        Fraction::new(1, 2),
        division,
    );
    assert_eq!(
        pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .err(),
        Some(QueryLimit::NativeFraction { operation: "beat" }),
    );
}

/// A `bite` window outside the native fraction range refuses as
/// `NativeFraction { operation: "bite" }` instead of panicking.
#[test]
fn bite_with_an_index_past_the_fraction_range_refuses() {
    let pattern = rustel_core::combinators::bite(
        &pure(Value::Str("bd".into())),
        &pure(Value::F64(0.5)),
        &pure(Value::F64(1e38)),
    );
    assert_eq!(
        pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .err(),
        Some(QueryLimit::NativeFraction { operation: "bite" }),
    );
}
