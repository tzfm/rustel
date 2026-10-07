//! `fill` caches query results across repeated ticks and phrases.
//!
//! Each query reads an extra cycle on both sides. Tests compare warm and cold
//! results and check invalidation for volatile children, changed settings,
//! and interrupted queries.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rustel_core::rng::{RngMode, use_rng};
use rustel_core::settings::RuntimeSettings;
use rustel_core::{
    Hap, Pattern, State, TimeSpan, Value, fastcat, pure, query_interrupted, signal_query_error,
    stack, take_query_callback_failure,
};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

/// Two events a cycle, and a count of how often the child is asked about
/// any of them.
fn counting_child(asked: Arc<AtomicUsize>) -> Pattern {
    fastcat(vec![
        pure(Value::Str("a".into())),
        pure(Value::Str("b".into())),
    ])
    .filter_haps(move |_| {
        asked.fetch_add(1, Ordering::Relaxed);
        true
    })
}

fn fill(pattern: Pattern) -> Pattern {
    rustel_ext::default_registry()
        .get("fill")
        .expect("fill registration")
        .call(&[], pattern)
}

fn span(begin: Fraction, end: Fraction) -> State {
    State::new(TimeSpan::new(begin, end))
}

/// The observable shape of one hap: whole, part, value and source context.
type Shape = (Option<TimeSpan>, TimeSpan, String, Vec<(usize, usize)>);

/// The observable shape of an answer.
fn shape(haps: &[Hap]) -> Vec<Shape> {
    haps.iter()
        .map(|hap| (hap.whole, hap.part, hap.value.show(), hap.context.clone()))
        .collect()
}

#[test]
fn a_warm_fill_answers_a_sliding_scheduler_exactly_as_a_cold_one() {
    let warm = fill(counting_child(Arc::new(AtomicUsize::new(0))));
    let step = Fraction::new(1, 20);
    for tick in 0..60i128 {
        let begin = step.mul(Fraction::int(tick));
        let end = begin.add(step);
        let from_warm = warm.query_arc(begin, end);
        let cold = fill(counting_child(Arc::new(AtomicUsize::new(0))));
        let from_cold = cold.query_arc(begin, end);
        assert_eq!(shape(&from_warm), shape(&from_cold), "tick {tick}");
    }
    // The scheduler's real question repeats under `struct`: every gate step
    // asks the same sixteenth again on every tick. Warm, it is free.
    let asked = Arc::new(AtomicUsize::new(0));
    let filled = fill(counting_child(Arc::clone(&asked)));
    let sixteenth = Fraction::new(1, 16);
    let _ = filled.query_arc(sixteenth, sixteenth.mul(Fraction::int(2)));
    let once = asked.load(Ordering::Relaxed);
    for _ in 0..40 {
        let _ = filled.query_arc(sixteenth, sixteenth.mul(Fraction::int(2)));
    }
    assert_eq!(asked.load(Ordering::Relaxed), once);
}

#[test]
fn a_volatile_child_is_asked_every_time() {
    let asked = Arc::new(AtomicUsize::new(0));
    let filled = fill(counting_child(Arc::clone(&asked)).mark_volatile());
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    let once = asked.load(Ordering::Relaxed);
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2 * once,
        "an answer that may move is never kept"
    );
}

#[test]
fn a_changed_setting_asks_the_child_again() {
    let settings = RuntimeSettings::default();
    let _scope = settings.bind();
    let asked = Arc::new(AtomicUsize::new(0));
    let filled = fill(counting_child(Arc::clone(&asked)));
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    let once = asked.load(Ordering::Relaxed);
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    assert_eq!(asked.load(Ordering::Relaxed), once, "same snapshot, kept");
    use_rng(RngMode::Precise);
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2 * once,
        "a child may read settings, so a new snapshot is a new question"
    );
    use_rng(RngMode::Legacy);
}

#[test]
fn an_interrupted_answer_is_not_kept() {
    let asked = Arc::new(AtomicUsize::new(0));
    let filled = fill(counting_child(Arc::clone(&asked)));
    // A budget of one hap refuses the child's window before it is complete.
    let refused = filled.try_query_state_with_budget(&span(Fraction::ZERO, Fraction::ONE), 1);
    assert!(refused.is_err(), "the budget refuses the query");
    let after_refusal = asked.load(Ordering::Relaxed);
    let healthy = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    assert!(
        !healthy.is_empty(),
        "the next, unbudgeted query answers in full"
    );
    assert!(
        asked.load(Ordering::Relaxed) > after_refusal,
        "and asked the child, rather than replaying the refused window"
    );
}

#[test]
fn a_stack_answer_with_a_failed_child_is_not_kept() {
    assert_stack_failure_is_not_cached(
        |pattern, state| pattern.query_arc(state.span.begin, state.span.end),
        Some("transient stack child failure"),
    );
}

#[test]
fn a_raw_stack_query_recovers_without_leaking_failure_state() {
    assert_stack_failure_is_not_cached(Pattern::query, None);
}

fn assert_stack_failure_is_not_cached(
    query: impl Fn(&Pattern, &State) -> Vec<Hap>,
    published_failure: Option<&str>,
) {
    let fail = Arc::new(AtomicBool::new(true));
    let asked = Arc::new(AtomicUsize::new(0));
    let child_fail = Arc::clone(&fail);
    let child_asked = Arc::clone(&asked);
    let recovering = pure(Value::Str("recovering".into())).fmap(move |value| {
        child_asked.fetch_add(1, Ordering::Relaxed);
        if child_fail.load(Ordering::Relaxed) {
            signal_query_error(|| "transient stack child failure".into());
        }
        value.clone()
    });
    let healthy = pure(Value::Str("healthy".into()));
    let stacked = stack(vec![recovering, healthy.clone()]);
    assert!(
        stacked.is_cacheable(),
        "the fixture must exercise the cache"
    );
    let filled = fill(stacked);
    let state = span(Fraction::ZERO, Fraction::ONE);

    let partial = query(&filled, &state);
    assert!(!partial.is_empty(), "the healthy sibling still answers");
    assert!(
        partial
            .iter()
            .all(|hap| hap.value == Value::Str("healthy".into()))
    );
    assert_eq!(take_query_callback_failure().as_deref(), published_failure);
    assert!(
        !query_interrupted(),
        "a completed query must not leave a contained failure pending"
    );
    let after_failure = asked.load(Ordering::Relaxed);
    assert!(after_failure > 0);

    fail.store(false, Ordering::Relaxed);
    let recovered = query(&filled, &state);
    assert!(
        asked.load(Ordering::Relaxed) > after_failure,
        "the incomplete answer must not hide the recovered child"
    );
    assert_eq!(take_query_callback_failure(), None);
    assert!(!query_interrupted());
    let cold = fill(stack(vec![pure(Value::Str("recovering".into())), healthy]));
    assert_eq!(shape(&recovered), shape(&query(&cold, &state)));

    let after_recovery = asked.load(Ordering::Relaxed);
    assert_eq!(shape(&query(&filled, &state)), shape(&recovered));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        after_recovery,
        "a subsequent complete answer can still be cached"
    );
}

/// The whole recipe the cache exists for, evaluated like a score: two
/// `inspire` lanes under `tgate`, so an outer `fill` widens every tick to
/// some thirty gate steps, each re-asking an inner `fill` under `rib`.
const RECIPE: &str =
    r#"$: s("triangle*8").inspire("D:minor:pentatonic", 0.8, "0,1", 69, 2).tgate(1, 1, 1)"#;

fn evaluated(source: &str) -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    runtime
}

#[test]
fn a_warm_recipe_answers_a_sliding_scheduler_exactly_as_a_cold_one() {
    let warm = evaluated(RECIPE);
    assert!(
        warm.active_pattern()
            .expect("active pattern")
            .is_cacheable(),
        "the recipe is a function of its queries alone"
    );
    let step = Fraction::new(1, 20);
    for tick in 0..24i128 {
        let begin = step.mul(Fraction::int(tick));
        let end = begin.add(step);
        let from_warm = warm.query(Slot::Active, 0, begin, end).expect("warm query");
        let from_cold = evaluated(RECIPE)
            .query(Slot::Active, 0, begin, end)
            .expect("cold query");
        assert_eq!(shape(&from_warm), shape(&from_cold), "tick {tick}");
    }
}

#[test]
fn a_volatile_child_reached_only_at_query_time_is_asked_every_time() {
    use rustel_core::ops::PatOps;
    use rustel_core::purity::PurePattern;

    let asked = Arc::new(AtomicUsize::new(0));
    let counted = counting_child(Arc::clone(&asked));
    // A carrier that is all `fill` can classify when it is built; what the
    // bind materialises under it is volatile.
    let bound = PurePattern::assert_pure(pure(Value::F64(1.0)))
        .inner_bind(move |_| PurePattern::assert_pure(counted.mark_volatile()));
    assert!(bound.pattern().is_cacheable(), "statically cacheable");
    let filled = fill(bound.pattern().clone());
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    let once = asked.load(Ordering::Relaxed);
    assert!(once > 0);
    let _ = filled.query_arc(Fraction::ZERO, Fraction::ONE);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2 * once,
        "the answer reached something volatile, so it was not kept"
    );
}
