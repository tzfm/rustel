//! What a query-result cache is allowed to keep.
//!
//! Two classifications carry it. `Purity::volatile` names the graphs whose
//! answers may move between identical queries, so a cache never keeps them.
//! `SettingsStateId` names the settings snapshot an answer was computed
//! under, so a changed setting is a missed key rather than a stale answer.
//! The `PatternOfPure` construction memo is the first consumer of both: a
//! native body is built once per argument value and settings snapshot, and
//! the graph it built is what every later query sees.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustel_core::extension_node::ExtensionPatternNode;
use rustel_core::ops::PatOps;
use rustel_core::purity::PurePattern;
use rustel_core::rng::{RngMode, use_rng};
use rustel_core::settings::{RuntimeSettings, current_state_id};
use rustel_core::{
    Hap, Pattern, State, TimeSpan, TimelineState, Value, fastcat, js_query, pure, timeline,
};
use rustel_fraction::Fraction;

fn cycles(begin: i128, end: i128) -> (Fraction, Fraction) {
    (Fraction::int(begin), Fraction::int(end))
}

/// The observable shape of a hap, for comparing two answers.
fn shape(haps: &[Hap]) -> Vec<(Option<TimeSpan>, TimeSpan, String)> {
    haps.iter()
        .map(|hap| (hap.whole, hap.part, hap.value.show()))
        .collect()
}

struct Forwarding {
    children: Vec<Pattern>,
}

impl ExtensionPatternNode for Forwarding {
    fn kind(&self) -> &'static str {
        "test.forwarding"
    }

    fn children(&self) -> &[Pattern] {
        &self.children
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        self.children[0].query(state)
    }

    fn drain_children(&mut self, out: &mut Vec<Pattern>) {
        out.append(&mut self.children);
    }
}

struct Stateless {
    children: Vec<Pattern>,
}

impl ExtensionPatternNode for Stateless {
    fn kind(&self) -> &'static str {
        "test.stateless"
    }

    fn children(&self) -> &[Pattern] {
        &self.children
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        self.children[0].query(state)
    }

    fn volatile(&self) -> bool {
        false
    }

    fn drain_children(&mut self, out: &mut Vec<Pattern>) {
        out.append(&mut self.children);
    }
}

#[test]
fn an_ordinary_graph_is_cacheable() {
    let pattern =
        fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]).fast(Fraction::int(2));
    assert!(!pattern.purity().volatile);
    assert!(pattern.is_cacheable());
}

#[test]
fn a_timeline_remembers_and_is_volatile() {
    let pattern = timeline(
        pure(Value::F64(0.0)),
        pure(Value::Str("x".into())),
        TimelineState::default(),
    );
    assert!(pattern.purity().volatile);
    assert!(
        !pattern.purity().impure,
        "no JavaScript, still not cacheable"
    );
    assert!(!pattern.is_cacheable());
}

#[test]
fn a_callback_is_volatile_as_well_as_impure() {
    let pattern = js_query(3);
    assert!(pattern.purity().impure);
    assert!(pattern.purity().volatile);
    assert!(!pattern.is_cacheable());
}

#[test]
fn a_marked_handle_stays_volatile_however_it_is_wrapped() {
    let marked = pure(Value::F64(1.0)).mark_volatile();
    assert!(marked.purity().volatile);
    let wrapped = fastcat(vec![marked, pure(Value::F64(2.0))])
        .fast(Fraction::int(2))
        .fmap(|value| value.clone());
    assert!(wrapped.purity().volatile, "wrapping never removes the mark");
    assert!(!wrapped.is_cacheable());
    // The original handle is untouched: the mark is on the copy.
    assert!(pure(Value::F64(1.0)).is_cacheable());
}

#[test]
fn a_pure_pattern_can_be_marked_too() {
    let marked = PurePattern::assert_pure(pure(Value::F64(1.0))).mark_volatile();
    assert!(marked.pattern().purity().volatile);
    assert!(!marked.pattern().purity().impure);
}

#[test]
fn an_extension_node_is_volatile_unless_it_says_otherwise() {
    let forwarding = Pattern::extension_node(Forwarding {
        children: vec![pure(Value::F64(1.0))],
    });
    assert!(
        forwarding.purity().volatile,
        "the default is the conservative one"
    );
    assert!(!forwarding.is_cacheable());

    let stateless = Pattern::extension_node(Stateless {
        children: vec![pure(Value::F64(1.0))],
    });
    assert!(!stateless.purity().volatile);
    assert!(stateless.is_cacheable());

    // A stateless node over a volatile child is volatile through the child.
    let over_volatile = Pattern::extension_node(Stateless {
        children: vec![pure(Value::F64(1.0)).mark_volatile()],
    });
    assert!(over_volatile.purity().volatile);
}

#[test]
fn the_settings_identity_moves_when_a_setting_is_published() {
    let settings = RuntimeSettings::default();
    let _scope = settings.bind();
    let before = current_state_id();
    assert_eq!(
        before,
        current_state_id(),
        "the same snapshot, the same identity"
    );
    use_rng(RngMode::Precise);
    let after = current_state_id();
    assert_ne!(before, after, "a setter publishes a fresh snapshot");
    use_rng(RngMode::Legacy);
    assert_ne!(after, current_state_id(), "and so does restoring the value");
}

/// A carrier of two values bound to a counting constructor.
fn counted_bind(built: Arc<AtomicUsize>) -> PurePattern {
    let carrier =
        PurePattern::assert_pure(fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]));
    carrier.inner_bind(move |value| {
        built.fetch_add(1, Ordering::Relaxed);
        PurePattern::assert_pure(pure(value.clone()).fast(Fraction::int(2)))
    })
}

#[test]
fn a_native_body_is_built_once_per_argument_value() {
    let built = Arc::new(AtomicUsize::new(0));
    let bound = counted_bind(Arc::clone(&built));
    let (begin, end) = cycles(0, 1);

    let first = bound.query_arc(begin, end);
    assert!(!first.is_empty());
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "one construction per value"
    );

    let again = bound.query_arc(begin, end);
    assert_eq!(shape(&first), shape(&again));
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "the second query builds nothing"
    );

    let (begin, end) = cycles(1, 5);
    let later = bound.query_arc(begin, end);
    assert!(!later.is_empty());
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "later cycles ask about the same two values"
    );
}

#[test]
fn a_kept_construction_answers_exactly_what_a_fresh_one_does() {
    let (begin, end) = cycles(0, 3);
    let warm = counted_bind(Arc::new(AtomicUsize::new(0)));
    let _ = warm.query_arc(begin, end);
    let from_memo = warm.query_arc(begin, end);
    let fresh = counted_bind(Arc::new(AtomicUsize::new(0))).query_arc(begin, end);
    assert_eq!(shape(&from_memo), shape(&fresh));
}

#[test]
fn a_changed_setting_builds_again() {
    let settings = RuntimeSettings::default();
    let _scope = settings.bind();
    let built = Arc::new(AtomicUsize::new(0));
    let bound = counted_bind(Arc::clone(&built));
    let (begin, end) = cycles(0, 1);

    let _ = bound.query_arc(begin, end);
    assert_eq!(built.load(Ordering::Relaxed), 2);
    use_rng(RngMode::Precise);
    let _ = bound.query_arc(begin, end);
    assert_eq!(
        built.load(Ordering::Relaxed),
        4,
        "a body may read settings while it builds, so a new snapshot is a new build"
    );
    use_rng(RngMode::Legacy);
}

#[test]
fn nan_is_never_kept() {
    let built = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&built);
    let bound = PurePattern::assert_pure(pure(Value::F64(f64::NAN))).inner_bind(move |value| {
        counter.fetch_add(1, Ordering::Relaxed);
        PurePattern::assert_pure(pure(value.clone()))
    });
    let (begin, end) = cycles(0, 1);
    let _ = bound.query_arc(begin, end);
    let _ = bound.query_arc(begin, end);
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "NaN would never be found again"
    );
}

#[test]
fn a_volatile_construction_is_not_kept() {
    let built = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&built);
    let bound = PurePattern::assert_pure(pure(Value::F64(1.0))).inner_bind(move |value| {
        counter.fetch_add(1, Ordering::Relaxed);
        PurePattern::assert_pure(pure(value.clone()).mark_volatile())
    });
    let (begin, end) = cycles(0, 1);
    let _ = bound.query_arc(begin, end);
    let _ = bound.query_arc(begin, end);
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "sharing a construction that keeps memory would change what it answers"
    );
}

#[test]
fn kept_constructions_are_released_with_their_node() {
    // A token every built graph holds a copy of: its count says how many of
    // them are alive.
    let token = Arc::new(());
    {
        let held = Arc::clone(&token);
        let bound = PurePattern::assert_pure(pure(Value::F64(1.0))).inner_bind(move |value| {
            let held = Arc::clone(&held);
            PurePattern::assert_pure(pure(value.clone()).fmap(move |x| {
                let _alive = &held;
                x.clone()
            }))
        });
        let (begin, end) = cycles(0, 1);
        let _ = bound.query_arc(begin, end);
        // The test's own copy, the closure's, and the kept graph's.
        assert_eq!(
            Arc::strong_count(&token),
            3,
            "the memo keeps the graph it built"
        );
    }
    assert_eq!(
        Arc::strong_count(&token),
        1,
        "and releases it with the node, through the destruction worklist"
    );
}

#[test]
fn negative_zero_is_a_different_key_from_zero() {
    let built = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&built);
    let carrier =
        PurePattern::assert_pure(fastcat(vec![pure(Value::F64(0.0)), pure(Value::F64(-0.0))]));
    let bound = carrier.inner_bind(move |value| {
        counter.fetch_add(1, Ordering::Relaxed);
        PurePattern::assert_pure(pure(value.clone()))
    });
    let (begin, end) = cycles(0, 1);
    let _ = bound.query_arc(begin, end);
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "the bits differ, so a body may tell them apart"
    );
    let _ = bound.query_arc(begin, end);
    assert_eq!(
        built.load(Ordering::Relaxed),
        2,
        "and each is kept on its own"
    );
}

#[test]
fn reaching_something_volatile_is_counted() {
    let (begin, end) = cycles(0, 1);
    let before = rustel_core::uncacheable_touches();
    let _ = pure(Value::F64(1.0)).query_arc(begin, end);
    assert_eq!(
        rustel_core::uncacheable_touches(),
        before,
        "an ordinary query touches nothing"
    );
    let _ = pure(Value::F64(1.0)).mark_volatile().query_arc(begin, end);
    assert!(rustel_core::uncacheable_touches() > before);
}

#[test]
fn a_volatile_graph_materialised_at_query_time_is_counted_too() {
    let bound = PurePattern::assert_pure(pure(Value::F64(1.0)))
        .inner_bind(|value| PurePattern::assert_pure(pure(value.clone()).mark_volatile()));
    assert!(
        bound.pattern().is_cacheable(),
        "statically, the carrier is all that can be seen"
    );
    let (begin, end) = cycles(0, 1);
    let before = rustel_core::uncacheable_touches();
    let _ = bound.query_arc(begin, end);
    assert!(
        rustel_core::uncacheable_touches() > before,
        "the inner graph is volatile, and a query reaches it"
    );
}

#[test]
fn a_key_holding_a_pattern_is_never_kept() {
    // A value that carries a graph could carry the memo's own node, and
    // keeping it would tie a cycle no worklist takes apart.
    // A pattern-valued carrier is impure (the value is a JS-owned identity),
    // which is exactly how `register()`'s general path wraps a native body
    // around it: the closure stays pure, the carrier does not.
    let built = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&built);
    let carrier = pure(Value::Pattern(Box::new(
        rustel_core::value::PatternValue::new(7, pure(Value::F64(1.0))),
    )));
    let bound = carrier
        .fmap_to_pure_pattern(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            PurePattern::assert_pure(pure(Value::F64(1.0)))
        })
        .inner_join();
    let (begin, end) = cycles(0, 1);
    let _ = bound.query_arc(begin, end);
    let _ = bound.query_arc(begin, end);
    assert_eq!(built.load(Ordering::Relaxed), 2);
}
