/*
rustel-core - the pre-materialization hap budget
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! A bound on how many haps one query may PRODUCE, charged as they are made.
//!
//! `MAX_QUERY_SPAN_CYCLES` already bounds how WIDE a query is. It says nothing
//! about density: a pattern can cover a single cycle and still ask for millions
//! of haps, and nesting multiplies - two unremarkable factors make an enormous
//! product. The span guard cannot see any of it, because the explosion happens
//! inside one cycle.
//!
//! # Testing with a small budget
//!
//! * the budget is INJECTED (`try_query_state_with_budget`), so the same code
//!   paths run at a budget of a few hundred;
//! * density comes from `fast()` on a small pattern, never from N allocations.
//!   `fastcat(k).fast(m)` yields `k * m` haps per cycle from `k` nodes, so hap
//!   count and allocation are decoupled.
//!
//! Nothing in this file constructs more than a few dozen `Pattern`s or asks for
//! more than a few thousand haps.

use rustel_core::{
    DEFAULT_HAP_BUDGET, JoinMode, Pattern, PickIndexMode, PickLookup, QueryLimit, State, TimeSpan,
    Value, pure,
};
use rustel_fraction::Fraction;

/// A small budget: big enough to exercise every path, small enough that
/// exceeding it costs nothing.
const TEST_BUDGET: u64 = 500;

fn span(begin: i128, end: i128) -> State {
    State::new(TimeSpan::new(Fraction::int(begin), Fraction::int(end)))
}

/// `k * m` haps per cycle from `k` pattern nodes.
///
/// The density is produced by the TIME transform, so asking for 100,000 haps
/// costs four allocations rather than 100,000. This is the whole reason the
/// file is safe to run.
fn dense(k: usize, m: i128) -> Pattern {
    let atoms = (0..k).map(|i| pure(Value::F64(i as f64))).collect();
    rustel_core::fastcat(atoms).fast(Fraction::int(m))
}

/// Exactly `n` haps in one cycle, still from a handful of nodes.
fn exactly(n: usize) -> Pattern {
    // `n = k * m + r`: a dense block plus at most `k - 1` singles, stacked.
    let k = 4.min(n.max(1));
    let m = (n / k) as i128;
    let remainder = n - k * m as usize;
    let mut layers = Vec::with_capacity(remainder + 1);
    if m > 0 {
        layers.push(dense(k, m));
    }
    for i in 0..remainder {
        layers.push(pure(Value::F64(-(i as f64) - 1.0)));
    }
    rustel_core::stack(layers)
}

// -- the helper itself ------------------------------------------------------

#[test]
fn the_test_helpers_produce_the_hap_counts_they_claim() {
    // Asserted first: every other test in this file is meaningless if `exactly`
    // is off by one, and an off-by-one is exactly what the limit tests probe.
    for n in [0, 1, 2, 3, 4, 5, 7, 16, 63, 100, 499, 500, 501] {
        let haps = exactly(n)
            .try_query_state_with_budget(&span(0, 1), 100_000)
            .expect("helper queries must not be refused at a large budget");
        assert_eq!(haps.len(), n, "exactly({n}) produced {} haps", haps.len());
    }
}

// -- below the limit: semantics untouched -----------------------------------

#[test]
fn the_budget_spans_the_whole_query_not_each_cycle() {
    // 2 haps per cycle over 100 cycles is 200 - under the budget. Over 400
    // cycles it is 800, which is not. A per-cycle limit would allow both.
    let pattern = dense(2, 1);
    assert_eq!(
        pattern
            .try_query_state_with_budget(&span(0, 100), TEST_BUDGET)
            .expect("200 haps is under the budget")
            .len(),
        200
    );
    assert!(
        pattern
            .try_query_state_with_budget(&span(0, 400), TEST_BUDGET)
            .is_err(),
        "800 haps must exceed a {TEST_BUDGET} budget, however they are spread"
    );
}

// -- the limit itself -------------------------------------------------------

#[test]
fn the_exact_limit_is_allowed_and_one_more_is_refused() {
    // Off-by-one at a resource boundary is the difference between "refuses
    // nothing it should" and "refuses one thing it should not", so both sides
    // are pinned rather than a comfortable value in the middle.
    let at_limit = exactly(TEST_BUDGET as usize)
        .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
        .expect("exactly the budget must be allowed");
    assert_eq!(at_limit.len(), TEST_BUDGET as usize);

    let over =
        exactly(TEST_BUDGET as usize + 1).try_query_state_with_budget(&span(0, 1), TEST_BUDGET);
    assert!(
        matches!(over, Err(QueryLimit::HapBudget { budget }) if budget == TEST_BUDGET),
        "one hap past the budget must be refused, got {over:?}"
    );
}

#[test]
fn exhaustion_is_never_reported_as_an_empty_result() {
    // `query_state` returns haps, so a refused query and a silent query are
    // the same value there. The typed channel must keep them distinct, and
    // the `queryArc` boundary, which turns a strudel.cc throw into silence,
    // must not hide the refusal.
    let refused = exactly(TEST_BUDGET as usize + 1);
    assert!(
        refused
            .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
            .is_err(),
        "a refused query must report the refusal"
    );
    let silent = rustel_core::silence().try_query_state_with_budget(&span(0, 1), TEST_BUDGET);
    assert!(
        matches!(&silent, Ok(haps) if haps.is_empty()),
        "a genuinely empty pattern must NOT look like a refusal, got {silent:?}"
    );
}

#[test]
fn a_refused_query_returns_nothing_rather_than_a_truncated_prefix() {
    // Silent truncation is the failure mode that would make every downstream
    // number quietly wrong: a render missing its tail looks like a correct
    // render of a shorter pattern.
    match exactly(TEST_BUDGET as usize + 1).try_query_state_with_budget(&span(0, 1), TEST_BUDGET) {
        Err(QueryLimit::HapBudget { .. }) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn nested_density_cannot_multiply_past_the_budget() {
    // Neither factor is remarkable alone; the product is. A per-node or
    // per-combinator limit would pass each of these and still let the query
    // through.
    assert!(
        dense(4, 4)
            .fast(Fraction::int(100))
            .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
            .is_err(),
        "4 * 4 * 100 = 1600 haps must exceed a {TEST_BUDGET} budget"
    );
    // ...and the same shape just under it is produced in full.
    assert_eq!(
        dense(4, 4)
            .fast(Fraction::int(25))
            .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
            .expect("400 haps is under the budget")
            .len(),
        400
    );
}

#[test]
fn a_hostile_density_is_refused_promptly() {
    // The shape the budget exists for: one cycle, density far past the limit,
    // no width for `MAX_QUERY_SPAN_CYCLES` to object to.
    //
    // Timed, because "refused" and "refused after allocating for a minute" are
    // different outcomes: production must STOP, not merely be reported on
    // afterwards. The multiplier is large relative to the budget while staying
    // trivially small in absolute terms.
    let started = std::time::Instant::now();
    let result = dense(8, 100_000).try_query_state_with_budget(&span(0, 1), TEST_BUDGET);
    let elapsed = started.elapsed();
    assert!(result.is_err(), "a hostile density must be refused");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the refusal took {elapsed:?}; production is not being stopped early - \
         the budget is being checked after the fact rather than charged as haps \
         are made"
    );
}

#[test]
fn a_refused_query_does_not_poison_the_next_one() {
    // The budget is thread-local, so a leaked flag would make every subsequent
    // query on the thread fail - turning one hostile input into a broken
    // session.
    let refused = exactly(TEST_BUDGET as usize + 1);
    assert!(
        refused
            .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
            .is_err()
    );

    let ordinary = exactly(4);
    assert_eq!(
        ordinary
            .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
            .expect("the next query must be unaffected")
            .len(),
        4
    );
    // ...including through the untyped entry points, which is what the rest of
    // the port calls.
    assert_eq!(ordinary.query_state(&span(0, 1)).len(), 4);
    assert_eq!(ordinary.query_arc(Fraction::ZERO, Fraction::ONE).len(), 4);
}

#[test]
fn the_default_budget_is_the_documented_value() {
    // Pinned without materialising it: five million haps is far past any
    // musical query, and changing it should be a deliberate act with a reason,
    // not a silent edit.
    assert_eq!(DEFAULT_HAP_BUDGET, 5_000_000);
}

#[test]
fn a_refused_leaf_never_materialises_its_haps() {
    // The budget is charged before production, not after. `pure(x).fast(m)`
    // makes one leaf produce `m` haps in one pass. With the pre-charge the
    // refused leaf does not materialise them.
    rustel_core::reset_haps_materialised();
    let hostile = pure(Value::F64(1.0)).fast(Fraction::int(200_000));
    let result = hostile.try_query_state_with_budget(&span(0, 1), TEST_BUDGET);
    let materialised = rustel_core::haps_materialised();

    assert!(result.is_err(), "the hostile leaf must be refused");
    assert!(
        materialised <= TEST_BUDGET,
        "the refused query still materialised {materialised} haps against a \
         {TEST_BUDGET} budget: the charge happens AFTER production, so the \
         allocation it exists to prevent already occurred"
    );
}

#[test]
fn an_allowed_leaf_does_materialise_its_haps() {
    // The control, so the test above cannot pass because the counter is simply
    // never incremented.
    rustel_core::reset_haps_materialised();
    let ok = pure(Value::F64(1.0)).fast(Fraction::int(100));
    let haps = ok
        .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
        .expect("100 haps is under the budget");
    assert_eq!(haps.len(), 100);
    assert!(
        rustel_core::haps_materialised() >= 100,
        "the counter did not record a query that genuinely produced haps, so \
         the assertion above proves nothing"
    );
}

#[test]
fn an_amplifying_join_stops_before_it_multiplies_past_the_budget() {
    // The gap a per-NODE charge cannot close. A join's output is the product of
    // its operands, so two individually legal patterns multiply inside one
    // loop: checking only the completed vector allows an outer × inner join to
    // exceed the budget while it is still being built.
    //
    // `squeezeJoin`, not `innerJoin`: the latter clips the inner pattern to the
    // outer hap's part, so it produces no more haps than the outer has and is
    // not an amplifier at all. Squeeze fits a WHOLE inner cycle into each outer
    // hap, so the output really is outer x inner.
    //
    // Both operands here are comfortably legal on their own; only the product
    // is not.
    rustel_core::reset_haps_materialised();
    let outer = dense(2, 50); // 100 haps - exactly the budget below
    let joined = outer.fmap_to_pattern(|_| dense(2, 50)).squeeze_join();

    let result = joined.try_query_state_with_budget(&span(0, 1), 100);
    assert!(
        result.is_err(),
        "a join whose product is 100 x 100 must be refused under a 100 budget"
    );
    // ...and it must stop DURING the loop. The materialisation counter records
    // leaves, so an unbudgeted join shows every inner query it ran.
    let materialised = rustel_core::haps_materialised();
    assert!(
        materialised < 5_000,
        "the refused join still materialised {materialised} haps: the check \
         happens on the finished vector rather than as it grows, so the \
         allocation it exists to prevent already occurred"
    );
}

#[test]
fn a_legal_join_is_unaffected_by_the_per_push_check() {
    // The control: budgeting each push must not truncate a join that fits.
    let outer = dense(2, 5); // 10 haps
    let joined = outer.fmap_to_pattern(|_| dense(2, 5)).squeeze_join();
    let haps = joined
        .try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
        .expect("a 10 x 10 squeeze join is under a 500 budget");
    assert_eq!(haps.len(), 100, "a legal join was truncated");
}

// -- every expanding producer, exact limit and limit+1 ----------------------
//
// Each of these grows by CONCATENATION, so every branch is individually legal
// while the total is not. A charge on the finished vector sees the sum only
// after it exists: two 100-hap siblings materialised 200 under a budget of 100.
// Each test names its producer so a mutation identifies which check was lost.

/// Assert a producer is allowed at exactly `budget` haps and refused at one
/// more, and that the refusal did not materialise the whole thing first.
fn assert_bounded_at(
    producer: &str,
    at_limit: (Pattern, i128),
    over: (Pattern, i128),
    budget: u64,
    unbounded_total: u64,
) {
    let (at_pattern, at_cycles) = at_limit;
    let haps = at_pattern
        .try_query_state_with_budget(&span(0, at_cycles), budget)
        .unwrap_or_else(|e| panic!("{producer}: exactly {budget} haps was refused: {e}"));
    assert_eq!(
        haps.len() as u64,
        budget,
        "{producer}: the at-limit case produced the wrong number of haps"
    );

    let (over_pattern, over_cycles) = over;
    rustel_core::reset_haps_materialised();
    let result = over_pattern.try_query_state_with_budget(&span(0, over_cycles), budget);
    assert!(
        result.is_err(),
        "{producer}: one hap past the budget was NOT refused - this producer \
         accumulates without a per-push check"
    );
    // A check on the finished vector also refuses, so the refusal alone does
    // not show whether the concatenation happened first. The peak charge
    // does. When bounded, no single accumulation exceeds the budget by more
    // than one branch. When unbounded, it reaches the full total.
    let peak = rustel_core::peak_hap_vector();
    assert!(
        peak <= budget,
        "{producer}: a vector of {peak} haps existed under a {budget} budget \
         (unbounded this producer reaches {unbounded_total}). The check runs \
         after the vector is built, not as it grows."
    );
}

#[test]
fn stack_is_bounded_before_it_concatenates() {
    // Two individually legal siblings can exceed the combined budget.
    assert_bounded_at(
        "stack",
        (rustel_core::stack(vec![dense(2, 25), dense(2, 25)]), 1), // 50 + 50
        (
            rustel_core::stack(vec![dense(2, 25), dense(2, 25), exactly(1)]),
            1,
        ),
        100,
        101, // 50 + 50 + 1
    );
}

#[test]
fn split_queries_is_bounded_before_it_concatenates() {
    // One branch per cycle, so the span drives the total.
    // One branch per cycle, so the SPAN is what crosses the limit: 10 cycles of
    // a 10-hap pattern is exactly 100, 11 cycles is 110.
    let per_cycle = dense(2, 5);
    assert_bounded_at(
        "splitQueries",
        (per_cycle.clone().split_queries(), 10),
        (per_cycle.split_queries(), 11),
        100,
        110, // 11 cycles x 10
    );
}

#[test]
fn choose_cycles_is_bounded_before_it_concatenates() {
    // One branch per cycle, chosen at random; every branch here is the same
    // size so the count is deterministic regardless of the choice.
    let chooser = || rustel_core::choose_cycles(vec![dense(2, 5), dense(2, 5)], 0);
    assert_bounded_at(
        "chooseCycles",
        (chooser(), 10),
        (chooser(), 11),
        100,
        110, // 11 cycles x 10
    );
}

#[test]
fn weighted_choice_is_bounded_before_it_concatenates_cycles() {
    // Each selected value query is individually legal (10 haps), but the
    // cycle chooser accumulates one such branch per cycle. The WChoose node's
    // own push must refuse before the eleventh branch forms a 110-hap vector.
    let chooser = || rustel_core::wchoose(vec![(dense(2, 5), pure(Value::F64(1.0)))], true);
    assert_bounded_at("wchooseCycles", (chooser(), 10), (chooser(), 11), 100, 110);
}

#[test]
fn pick_is_bounded_before_it_concatenates_selected_cycles() {
    let picked = || {
        rustel_core::pick(
            pure(Value::F64(0.0)),
            PickLookup::Array {
                enumerable_len: 1,
                length: 1,
                entries: vec![(0, dense(2, 5))],
            },
            PickIndexMode::Clamp,
            JoinMode::Inner,
        )
    };
    assert_bounded_at("pick", (picked(), 10), (picked(), 11), 100, 110);
}

#[test]
fn patternified_pick_lookup_source_charges_before_materialising_carriers() {
    // The source is a PurePickLookup, which can emit one Arc-backed carrier per
    // cycle. Its own pre-charge must reject 501 before that vector exists; the
    // generic finished-node charge is too late. `assert_bounded_at`'s peak
    // assertion kills removing that source pre-charge (peak becomes 501).
    let picked = || {
        let lookup = PickLookup::Array {
            enumerable_len: 1,
            length: 1,
            entries: vec![(0, pure(Value::Str("x".into())))],
        };
        let carrier =
            rustel_core::pure_pick_lookup(Value::List(vec![Value::Str("x".into())]), lookup);
        rustel_core::pick_patternified(
            pure(Value::F64(0.0)),
            carrier,
            PickIndexMode::Clamp,
            JoinMode::Inner,
        )
    };
    assert_bounded_at(
        "patternified pick lookup source",
        (picked(), 500),
        (picked(), 501),
        500,
        501,
    );
}

#[test]
fn a_squeeze_join_is_bounded_before_it_multiplies() {
    // The amplifier, at the exact boundary rather than an order of magnitude
    // past it: 10 outer x 10 inner is exactly 100.
    let outer = dense(2, 5);
    assert_bounded_at(
        "squeezeJoin",
        (
            outer
                .clone()
                .fmap_to_pattern(|_| dense(2, 5))
                .squeeze_join(),
            1,
        ),
        (
            rustel_core::stack(vec![
                outer.fmap_to_pattern(|_| dense(2, 5)).squeeze_join(),
                exactly(1),
            ]),
            1,
        ),
        100,
        101, // 10 x 10 + 1
    );
}

// -- cancellation -----------------------------------------------------------

#[test]
fn a_cancelled_query_stops_between_nodes_rather_than_finishing() {
    // Targeted at the per-node check, deterministically. Driving this from the
    // CLI timing cannot isolate the query phase from the later sort.
    //
    // Setting the flag BEFORE the query removes the race entirely. With the
    // check, essentially no haps are materialised; without it, the whole
    // pattern is produced and only the boundary notices.
    let flag = std::sync::atomic::AtomicBool::new(true);
    let pattern = dense(8, 5_000); // 40,000 haps if it runs to completion

    rustel_core::reset_haps_materialised();
    let result = rustel_core::with_cancellation(&flag, || {
        pattern.try_query_state_with_budget(&span(0, 1), 1_000_000)
    });
    let materialised = rustel_core::haps_materialised();

    assert!(
        matches!(result, Err(QueryLimit::Cancelled)),
        "a cancelled query must report cancellation, not silence and not a \
         budget refusal, got {result:?}"
    );
    assert!(
        materialised < 1_000,
        "the cancelled query still materialised {materialised} haps: the check \
         runs only at the boundary, so the work happened anyway"
    );
}

#[test]
fn cancellation_is_reported_as_itself_not_as_a_budget_refusal() {
    // A query stopped part way usually also looks under-budget, so the order of
    // these checks decides which error the caller sees. Cancellation is the
    // caller's own request and must not be reported as a resource problem.
    let flag = std::sync::atomic::AtomicBool::new(true);
    let result = rustel_core::with_cancellation(&flag, || {
        exactly(TEST_BUDGET as usize + 1).try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
    });
    assert!(
        matches!(result, Err(QueryLimit::Cancelled)),
        "a cancelled query that would ALSO have exceeded its budget must \
         report the cancellation, got {result:?}"
    );
}

#[test]
fn an_uncancelled_query_is_unaffected_by_the_check() {
    let flag = std::sync::atomic::AtomicBool::new(false);
    let haps = rustel_core::with_cancellation(&flag, || {
        exactly(100).try_query_state_with_budget(&span(0, 1), TEST_BUDGET)
    })
    .expect("an uncancelled query must run normally");
    assert_eq!(haps.len(), 100);
}

#[test]
fn the_query_span_limit_is_exact_and_typed() {
    // The other resource guard is exact at its boundary, and its refusal is
    // typed. `fast(n)` scales the query span, so the inner pattern sees
    // `n * cycles` cycles: this crosses the limit by width, with no dense
    // pattern. The at-limit half materialises 1,000,000 haps to detect an
    // off-by-one. The over-limit half is refused before allocation.
    let at_limit = pure(Value::F64(1.0)).fast(Fraction::int(1_000));
    let haps = at_limit
        .try_query_state_with_budget(&span(0, 1_000), 10_000_000)
        .expect("exactly MAX_QUERY_SPAN_CYCLES must be allowed");
    assert_eq!(haps.len(), 1_000_000);

    let over = pure(Value::F64(1.0)).fast(Fraction::int(1_001));
    let refused = over.try_query_state_with_budget(&span(0, 1_000), 10_000_000);
    assert!(
        matches!(
            refused,
            Err(QueryLimit::QuerySpan {
                cycles: rustel_core::MAX_QUERY_SPAN_CYCLES
            })
        ),
        "one cycle past the span limit must be refused as QuerySpan, got {refused:?}"
    );
}
