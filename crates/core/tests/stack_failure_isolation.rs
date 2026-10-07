//! A stack contains a child's query error without discarding its siblings.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustel_core::{
    CallbackHost, CallbackId, Hap, Pattern, QueryArcOutcome, QueryLimit, State, TimeSpan, Value,
    fastcat, pure, pure_pattern, query_error_pattern, query_interrupted, signal_callback_failure,
    signal_query_error, stack, take_query_callback_failure, take_query_contained_throw,
    take_query_error, with_callback_host, with_cancellation,
};
use rustel_fraction::Fraction;

fn one_cycle() -> State {
    State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
}

fn named(name: &str) -> Pattern {
    pure(Value::Str(name.into()))
}

fn haps(pattern: &Pattern) -> Vec<Hap> {
    match pattern
        .query_arc_outcome(&one_cycle())
        .expect("a child throw is not a resource refusal")
    {
        QueryArcOutcome::Haps(haps) => haps,
        QueryArcOutcome::Thrown(message) => panic!("the stack let a child error escape: {message}"),
    }
}

fn values(haps: &[Hap]) -> Vec<Value> {
    haps.iter().map(|hap| hap.value.clone()).collect()
}

fn shape(haps: &[Hap]) -> Vec<(Option<TimeSpan>, TimeSpan, Value)> {
    haps.iter()
        .map(|hap| (hap.whole, hap.part, hap.value.clone()))
        .collect()
}

#[test]
fn a_failed_child_keeps_siblings_in_order_at_every_position() {
    let expected = haps(&stack(vec![named("a"), named("b")]));
    for position in 0..=2 {
        let mut children = vec![named("a"), named("b")];
        children.insert(position, query_error_pattern("child failed"));
        assert_eq!(shape(&haps(&stack(children))), shape(&expected));
        assert_eq!(
            take_query_callback_failure().as_deref(),
            Some("child failed"),
            "position {position} must report the isolated error"
        );
        assert_eq!(take_query_error(), None);
    }
}

#[test]
fn nested_stacks_keep_healthy_children_and_report_the_first_failure() {
    let pattern = stack(vec![
        named("a"),
        stack(vec![
            query_error_pattern("first failure"),
            named("b"),
            query_error_pattern("second failure"),
        ]),
        named("c"),
    ]);
    assert_eq!(
        values(&haps(&pattern)),
        ["a", "b", "c"].map(|name| Value::Str(name.into()))
    );
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("first failure")
    );
    assert_eq!(take_query_callback_failure(), None);
}

struct ThrowOnSecondValue {
    calls: AtomicUsize,
    logged: Mutex<Vec<String>>,
}

impl CallbackHost for ThrowOnSecondValue {
    fn call_value(&self, _id: CallbackId, value: &Value) -> Result<Value, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if value == &Value::Str("throw".into()) {
            Err("child callback failed".into())
        } else {
            Ok(value.clone())
        }
    }

    fn call_query(&self, _id: CallbackId, _state: &State) -> Result<Vec<Hap>, String> {
        unreachable!("this fixture only maps values")
    }

    fn log_query_error(&self, message: &str) {
        self.logged.lock().unwrap().push(message.into());
    }
}

#[test]
fn a_callback_throw_discards_the_failed_childs_partial_answer() {
    let host = ThrowOnSecondValue {
        calls: AtomicUsize::new(0),
        logged: Mutex::new(Vec::new()),
    };
    let partial = fastcat(vec![named("partial"), named("throw")]).fmap_js(7);
    let pattern = stack(vec![named("before"), partial, named("after")]);
    let answer = with_callback_host(&host, || haps(&pattern));
    assert_eq!(host.calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        values(&answer),
        ["before", "after"].map(|name| Value::Str(name.into()))
    );
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("callback 7 failed: child callback failed")
    );
    assert_eq!(
        host.logged.lock().unwrap().as_slice(),
        ["callback 7 failed: child callback failed"]
    );
}

#[test]
fn joins_resolve_the_healthy_siblings_of_a_failed_pattern_branch() {
    let healthy = || vec![pure_pattern(named("a")), pure_pattern(named("b"))];
    let failing = named("carrier").fmap_to_pattern(|_| {
        signal_query_error(|| "pattern callback failed".into());
        named("discarded")
    });
    let mut children = healthy();
    children.insert(1, failing);
    let outer = stack(children);
    let expected = stack(healthy());

    // innerJoin, polyJoin and stepJoin all use the pattern-resolution stack
    // arm, rather than the ordinary Vec<Hap> stack query.
    for (actual, expected) in [
        (outer.inner_join(), expected.inner_join()),
        (outer.poly_join(), expected.poly_join()),
        (outer.step_join(), expected.step_join()),
    ] {
        assert_eq!(actual.steps, expected.steps);
        let expected_haps = haps(&expected);
        assert!(!expected_haps.is_empty());
        assert_eq!(shape(&haps(&actual)), shape(&expected_haps));
        assert_eq!(
            take_query_callback_failure().as_deref(),
            Some("pattern callback failed")
        );
        assert_eq!(take_query_error(), None);
    }
}

#[test]
fn a_failed_child_does_not_change_the_stacks_declared_steps() {
    let pattern = stack(vec![
        named("a").with_steps(Some(Fraction::int(2))),
        query_error_pattern("child failed").with_steps(Some(Fraction::int(3))),
        named("b").with_steps(Some(Fraction::int(4))),
    ]);
    assert_eq!(pattern.steps, Some(Fraction::int(12)));
    assert_eq!(haps(&pattern).len(), 2);
    assert_eq!(pattern.steps, Some(Fraction::int(12)));
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("child failed")
    );
}

#[test]
fn a_stack_does_not_consume_an_already_pending_outer_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let child = named("a").fmap(move |value| {
        counted.fetch_add(1, Ordering::Relaxed);
        value.clone()
    });
    signal_query_error(|| "outer failed".into());
    let answer = stack(vec![child]).query(&one_cycle());
    // Take the thread-local before asserting so a failed assertion cannot
    // leave the fixture's deliberately pending error behind.
    let error = take_query_error();
    assert_eq!(error.as_deref(), Some("outer failed"));
    assert!(answer.is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(take_query_callback_failure(), None);
}

#[test]
fn a_recovered_answer_is_uncacheable_and_the_next_query_can_recover() {
    let fail = Arc::new(AtomicBool::new(true));
    let interrupted = Arc::new(AtomicBool::new(false));
    let fail_child = Arc::clone(&fail);
    let child = named("recovering").fmap(move |value| {
        if fail_child.load(Ordering::Relaxed) {
            signal_query_error(|| "transient child failure".into());
        }
        value.clone()
    });
    let observed = Arc::clone(&interrupted);
    let pattern = stack(vec![child, named("healthy")]).fmap(move |value| {
        observed.store(query_interrupted(), Ordering::Relaxed);
        value.clone()
    });

    assert_eq!(values(&haps(&pattern)), [Value::Str("healthy".into())]);
    assert!(
        interrupted.load(Ordering::Relaxed),
        "a cache outside stack must not keep its incomplete answer"
    );
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("transient child failure")
    );
    assert!(
        !query_interrupted(),
        "the query boundary restores its flags"
    );

    fail.store(false, Ordering::Relaxed);
    assert_eq!(
        values(&haps(&pattern)),
        ["recovering", "healthy"].map(|name| Value::Str(name.into()))
    );
    assert!(!interrupted.load(Ordering::Relaxed));
    assert_eq!(take_query_callback_failure(), None);
}

#[test]
fn a_later_outer_throw_still_discards_the_entire_answer() {
    let pattern = stack(vec![query_error_pattern("child failed"), named("a")]).fmap(|value| {
        signal_query_error(|| "outer failed".into());
        value.clone()
    });
    assert!(matches!(
        pattern.query_arc_outcome(&one_cycle()),
        Ok(QueryArcOutcome::Thrown(message)) if message == "outer failed"
    ));
    assert_eq!(take_query_callback_failure(), None);
    assert_eq!(take_query_contained_throw(), None);
}

#[test]
fn stack_isolation_does_not_contain_a_hap_budget_refusal() {
    let pattern = stack(vec![
        query_error_pattern("child failed"),
        named("a"),
        named("b"),
    ]);
    assert!(matches!(
        pattern.query_arc_outcome_with_budget(&one_cycle(), 1),
        Err(QueryLimit::HapBudget { budget: 1 })
    ));
    assert_eq!(haps(&named("next query")).len(), 1);
    assert_eq!(take_query_callback_failure(), None);
}

#[test]
fn stack_isolation_does_not_contain_cancellation() {
    let cancel = Arc::new(AtomicBool::new(false));
    let requested = Arc::clone(&cancel);
    let cancelling = named("cancel").fmap(move |value| {
        requested.store(true, Ordering::Relaxed);
        value.clone()
    });
    let pattern = stack(vec![
        query_error_pattern("child failed"),
        cancelling,
        named("later"),
    ]);
    let outcome = with_cancellation(&cancel, || pattern.query_arc_outcome(&one_cycle()));
    assert!(matches!(outcome, Err(QueryLimit::Cancelled)));
    assert_eq!(haps(&named("next query")).len(), 1);
    assert_eq!(take_query_callback_failure(), None);
}

#[test]
fn stack_isolation_does_not_catch_native_panics_or_leak_query_state() {
    let panicking = named("panic").fmap(|_| panic!("native fixture panic"));
    let pattern = stack(vec![query_error_pattern("child failed"), panicking]);
    signal_query_error(|| "outer pending error".into());
    let outcome = catch_unwind(AssertUnwindSafe(|| pattern.query_arc_outcome(&one_cycle())));
    let outer = take_query_error();
    assert!(outcome.is_err(), "native panics must keep unwinding");
    assert_eq!(outer.as_deref(), Some("outer pending error"));
    assert_eq!(haps(&named("next query")).len(), 1);
    assert_eq!(take_query_callback_failure(), None);
}

#[test]
fn a_contained_child_error_is_published_as_a_contained_throw() {
    let pattern = stack(vec![query_error_pattern("child failed"), named("a")]);
    assert_eq!(values(&haps(&pattern)), [Value::Str("a".into())]);
    assert_eq!(
        take_query_contained_throw().as_deref(),
        Some("child failed")
    );
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("child failed")
    );
}

#[test]
fn a_stack_whose_every_child_fails_stays_silent_and_reports_the_first_throw() {
    let pattern = stack(vec![
        query_error_pattern("first failure"),
        query_error_pattern("second failure"),
    ]);
    assert!(haps(&pattern).is_empty());
    assert_eq!(
        take_query_contained_throw().as_deref(),
        Some("first failure")
    );
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("first failure")
    );
    assert_eq!(take_query_error(), None);
}

#[test]
fn a_fail_open_callback_failure_is_not_a_contained_throw() {
    let failing_open = named("kept").fmap(|value| {
        signal_callback_failure(|| "predicate failed".into());
        value.clone()
    });
    let pattern = stack(vec![failing_open, named("a")]);
    assert_eq!(haps(&pattern).len(), 2);
    assert_eq!(take_query_contained_throw(), None);
    assert_eq!(
        take_query_callback_failure().as_deref(),
        Some("predicate failed")
    );
}

#[test]
fn the_next_query_does_not_report_an_earlier_contained_throw() {
    let failing = stack(vec![query_error_pattern("child failed"), named("a")]);
    assert_eq!(haps(&failing).len(), 1);
    assert_eq!(haps(&named("next query")).len(), 1);
    assert_eq!(take_query_contained_throw(), None);
    assert_eq!(take_query_callback_failure(), None);
}
