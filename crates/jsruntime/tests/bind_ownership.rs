/*
rustel-jsruntime - bind ownership and purity
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Check callback ownership and purity through call_bind and try_step_join.
//! Output checks alone miss leaked cells and unclosed frames. Unlike the
//! every/jux bridge tests, stepBind invokes callbacks during construction,
//! while the evaluation frame is still open.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt
}

fn eval(rt: &JsRuntime, source: &str) {
    rt.evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
}

fn shown(rt: &JsRuntime, cycles: i128) -> String {
    rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::int(cycles))
        .expect("query")
        .iter()
        .map(|h| h.show())
        .collect::<Vec<_>>()
        .join(";")
}

/// Every scope counter back to zero. A bind that unwound badly shows up here
/// and nowhere else.
fn assert_scopes_unwound(rt: &JsRuntime, what: &str) {
    assert_eq!(rt.query_depth(), 0, "{what}: query stack not unwound");
    assert_eq!(
        rustel_jsruntime::bridge_frame_depth(),
        0,
        "{what}: bridge frame still open"
    );
    assert_eq!(
        rustel_jsruntime::bridge_scratch_len(),
        0,
        "{what}: scratch not drained"
    );
}

// -- ownership --------------------------------------------------------------

#[test]
fn pure_of_a_wrapper_imports_its_reachable_sidecars() {
    // `pure(pattern)` must import the argument's callback cells. Both `every`
    // arguments are patterned, so the callback resolves at query time. The
    // expected haps are pinned Node output, so a wrong callback also fails.
    const WANT: &str = "[ 0/1 → 1/4 | bd ];[ 1/4 → 1/2 | sd ];[ 1/2 → 3/4 | bd ];\
[ 3/4 → 1/1 | sd ];[ 1/1 → 3/2 | bd ];[ 3/2 → 2/1 | sd ]";
    let rt = runtime();
    eval(
        &rt,
        r#"pure("bd sd".every(fastcat(2, 3), x => x.fast(2))).stepJoin()"#,
    );
    assert_eq!(
        shown(&rt, 2),
        WANT,
        "the imported callback did not resolve: `pure` dropped the sidecar"
    );

    // Repeat after a GC with the graph still installed: a cell that was only
    // reachable through the temporary would be collected here.
    rt.run_gc();
    assert_eq!(
        shown(&rt, 2),
        WANT,
        "the imported cell did not survive collection"
    );
    rt.clear_active();
}

#[test]
fn pure_of_a_builder_wrapper_imports_its_sidecar_too() {
    // The builder's wrapper family: `hold` installs a `PatternWrapper`.
    // `unwrap_pattern` must accept it, or `pure` gets a constant JS object
    // and the callback never runs.
    const WANT: &str = "[ 0/1 → 1/1 | bd! ]";
    let rt = runtime();
    let mut b = rt.builder();
    // A FACTORY, as `GraphBuilder::callback` expects: it receives this graph's
    // own wrapper and returns the callback.
    let id = b.callback(&rt, "(w) => (x) => x + '!'");
    let held = rt
        .hold(
            &b,
            rustel_core::pure(rustel_core::Value::Str("bd".into())).fmap_js(id),
        )
        .unwrap();
    rt.bind_global("p", Slot::Held, held).unwrap();

    eval(&rt, "pure(p).polyJoin()");
    assert_eq!(
        shown(&rt, 1),
        WANT,
        "a held `PatternWrapper` passed to `pure` lost its pattern or its \
         callback cell"
    );
    rt.run_gc();
    assert_eq!(
        shown(&rt, 1),
        WANT,
        "the imported cell did not survive collection"
    );
    rt.clear_active();
}

#[test]
fn two_live_bind_graphs_keep_distinct_callbacks() {
    // Two bind graphs are live at once and stacked, so one query invokes both.
    // The haps are compared with pinned Node output: an id collision would
    // run one branch with the other's callback.
    let rt = runtime();
    eval(
        &rt,
        r#"stack(
             pure('bd').polyBind(x => pure(x).fast(2)),
             pure('sd').polyBind(x => pure(x).fast(3))
           )"#,
    );
    assert_eq!(
        shown(&rt, 1),
        "[ (0/1 → 1/3) ⇝ 1/1 | sd ];[ (0/1 → 1/2) ⇝ 1/1 | bd ];\
[ 0/1 ⇜ (1/3 → 2/3) ⇝ 1/1 | sd ];[ 0/1 ⇜ (1/2 → 1/1) | bd ];\
[ 0/1 ⇜ (2/3 → 1/1) | sd ]",
        "the two bind callbacks are not distinct: one branch ran the other's \
         function"
    );
    rt.clear_active();
}

#[test]
fn a_bind_callbacks_nested_graph_survives_repeated_query_and_gc() {
    // The callback returns a graph that carries its own callback. A bind
    // resolves on every query, so the inner cell must survive each one.
    let rt = runtime();
    eval(
        &rt,
        r#"pure('bd').polyBind(x => pure(x).polyBind(y => pure(y).fast(2)))"#,
    );
    let first = shown(&rt, 1);
    assert!(!first.is_empty(), "the nested bind produced nothing");
    for round in 0..4 {
        rt.run_gc();
        assert_eq!(
            shown(&rt, 1),
            first,
            "round {round}: the nested callback stopped resolving after a GC"
        );
    }
    assert_scopes_unwound(&rt, "nested bind");
    rt.clear_active();
}

#[test]
fn a_mid_query_collection_cannot_sweep_a_callbacks_returned_cell() {
    // Trigger GC on the second outer hap, while the first returned graph is
    // held only by Rust callback IDs. GC after query completion misses this
    // ownership gap. The query's BridgeFrame must keep those cells rooted.
    let rt = runtime();
    rt.install_gc_binding().unwrap();
    eval(
        &rt,
        r#""bd sd".polyBind(x => { __gc(); return pure(x).polyBind(y => pure(y).fast(2)); })"#,
    );
    let got = shown(&rt, 1);
    assert!(
        !got.is_empty(),
        "a collection between two bind callbacks swept the first one's \
         returned cell"
    );
    // Two outer haps, each producing its own nested result: a swept cell would
    // silently drop one side rather than fail outright.
    assert_eq!(
        got.matches("bd").count(),
        2,
        "the first hap's nested callback did not survive the collection: {got}"
    );
    assert_eq!(
        got.matches("sd").count(),
        2,
        "the second hap's nested callback did not resolve: {got}"
    );
    assert_scopes_unwound(&rt, "mid-query collection");
    rt.clear_active();
}

#[test]
fn a_discarded_bind_temporary_is_not_rooted_by_the_survivor() {
    // stepBind runs during construction, so its nested callback enters the
    // evaluation frame's harvest set. polyBind runs too late to test this.
    // Only the callback-free second graph survives; it must not inherit the
    // discarded graph's cell. Disabling the reachability filter fails this test.
    let rt = runtime();
    rt.run_gc();
    let baseline_live = rustel_jsruntime::cells_live();
    let created_before = rustel_jsruntime::cells_created();

    eval(
        &rt,
        r#"pure('bd').stepBind(x => pure(x).polyBind(y => pure(y).fast(2)));
           fastcat("hh", "cp").fast(2)"#,
    );
    assert!(
        rustel_jsruntime::cells_created() >= created_before + 2,
        "the discarded statement must bridge its own callback AND harvest the \
         nested one during evaluation, or the filter under test never runs; \
         created {}",
        rustel_jsruntime::cells_created() - created_before
    );

    // GC with the survivor STILL INSTALLED.
    rt.run_gc();
    assert_eq!(
        rustel_jsruntime::cells_live(),
        baseline_live,
        "a discarded bind temporary's cell is still rooted while the active graph holds \
         the callback-free survivor"
    );
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/4 | hh ];[ 1/4 → 1/2 | cp ];[ 1/2 → 3/4 | hh ];[ 3/4 → 1/1 | cp ]",
        "the surviving graph lost haps: the reachability filter pruned too much"
    );
    rt.clear_active();
}

#[test]
fn an_eager_step_bind_harvests_its_nested_callback_during_evaluation() {
    // The property the test above depends on, asserted on its own so it cannot
    // rot silently: `stepBind`'s construction-time slice really does invoke the
    // callback, and the graph that callback returns really does contribute a
    // second cell BEFORE any query runs.
    //
    // If `try_step_join` stopped querying eagerly, this would drop to one cell
    // and the reachability filter would go untested again.
    let rt = runtime();
    rt.run_gc();
    let before = rustel_jsruntime::cells_created();
    eval(
        &rt,
        r#"pure('bd').stepBind(x => pure(x).polyBind(y => pure(y).fast(2)))"#,
    );
    let during_evaluation = rustel_jsruntime::cells_created() - before;
    assert!(
        during_evaluation >= 2,
        "an eager `stepBind` must bridge its own callback and harvest the \
         nested one during EVALUATION, got {during_evaluation}"
    );

    // ...and the harvested cell is still resolvable, so the eager pass did not
    // simply leak it into a frame that was then torn down.
    assert!(
        !shown(&rt, 1).is_empty(),
        "the harvested nested callback did not survive evaluation"
    );
    assert_scopes_unwound(&rt, "eager stepBind");
    rt.clear_active();
}

#[test]
fn cell_counts_plateau_across_repeated_bind_queries() {
    // A bind bridges its callback once, at construction, but resolves it on
    // every query. If a query allocated a cell it never reclaimed, the live
    // count would climb with each round rather than settle.
    let rt = runtime();
    eval(&rt, r#""bd sd".stepBind(x => fastcat(x, x))"#);
    for _ in 0..3 {
        let _ = shown(&rt, 2);
    }
    rt.run_gc();
    let settled = rustel_jsruntime::cells_live();
    for _ in 0..8 {
        let _ = shown(&rt, 2);
    }
    rt.run_gc();
    assert!(
        rustel_jsruntime::cells_live() <= settled,
        "bind queries accumulate cells: settled at {settled}, now {}",
        rustel_jsruntime::cells_live()
    );
    assert_scopes_unwound(&rt, "repeated bind queries");
    rt.clear_active();
}

// -- scope teardown, on every exit path -------------------------------------

#[test]
fn every_bind_exit_path_restores_depth_frame_and_scratch() {
    // Four exits, each through different code:
    //
    //   success            - the ordinary query-time resolution
    //   eager construction - `stepBind` invoking the callback before any query
    //   throw              - the callback raising, caught at the query boundary
    //   non-pattern return - `Ok(BindResult::Value)`, which is NOT an error and
    //                        must not be mistaken for one by the teardown
    //
    // A construction throw is the interesting one: `try_step_join` runs a query
    // and then returns `Err`, so its stash/restore of the query-error slot and
    // the surrounding RAII frames both have to survive an early return.
    for (label, source, expect_eval_error) in [
        (
            "success",
            r#"pure('bd').polyBind(x => pure(x).fast(2))"#,
            false,
        ),
        ("eager", r#"pure('bd').stepBind(x => fastcat(x, x))"#, false),
        (
            "throw",
            r#"pure('bd').polyBind(x => { throw new Error('boom'); })"#,
            false,
        ),
        ("non-pattern", r#"pure('bd').polyBind(x => 42)"#, false),
        // `stepBind` with a non-pattern return throws during CONSTRUCTION.
        (
            "construction throw",
            r#"pure('bd').stepBind(x => 42)"#,
            true,
        ),
    ] {
        let rt = runtime();
        let result = rt.evaluate_score(source, &TranspileOptions::default());
        assert_eq!(
            result.is_err(),
            expect_eval_error,
            "{label}: wrong phase - {source} should {} during evaluation, got {result:?}",
            if expect_eval_error { "fail" } else { "succeed" }
        );
        assert_scopes_unwound(&rt, &format!("{label} (after evaluation)"));
        if !expect_eval_error {
            let _ = shown(&rt, 1);
            assert_scopes_unwound(&rt, &format!("{label} (after query)"));
            rt.clear_active();
        }
    }
}

#[test]
fn a_missing_host_is_still_a_hard_error_for_binds() {
    // The negative control: a bind graph is impure and MUST panic if it
    // is ever queried with no callback host installed. If this stopped
    // panicking, a false-pure classification would silently return wrong haps
    // instead of failing loudly.
    let rt = runtime();
    eval(&rt, r#"pure('bd').polyBind(x => pure(x).fast(2))"#);
    let pattern = rt.active_pattern().expect("active pattern");
    assert!(
        !pattern.is_pure(),
        "a bind graph must never be classified pure"
    );
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // No `with_callback_host`, deliberately.
        pattern.query_arc(Fraction::ZERO, Fraction::ONE)
    }))
    .is_err();
    assert!(
        panicked,
        "querying a bind graph with no host installed must panic, not silently \
         produce haps"
    );
    rt.clear_active();
}

// -- purity classification --------------------------------------------------

#[test]
fn bind_nodes_are_impure_opaque_and_name_their_callback() {
    // `PatternOfJs` is impure, names its callback id for `gc_mark`, and is
    // opaque: the callback's result can reach callbacks that cannot be
    // enumerated, so `derive_wrapper` must not prune by the reachable set.
    for source in [
        r#"pure('bd').polyBind(x => pure(x).fast(2))"#,
        r#"pure('bd').stepBind(x => fastcat(x, x))"#,
        r#""bd sd".polyBind(x => pure(x))"#,
    ] {
        let rt = runtime();
        eval(&rt, source);
        let pattern = rt.active_pattern().expect("active pattern");
        assert!(!pattern.is_pure(), "{source} must be impure");
        assert!(
            pattern.purity().opaque,
            "{source}: a bind's result is an unknown path and must be opaque"
        );
        assert!(
            !pattern.reachable_callbacks().is_empty(),
            "{source}: the bind must contribute its own callback id"
        );
        rt.clear_active();
    }
}

#[test]
fn a_known_inner_direct_join_stays_pure() {
    // `polyJoin`/`stepJoin` on native pieces never enters JavaScript, so it
    // stays pure. The query runs with no host installed: a wrong
    // classification panics at the host lookup.
    for (source, cycles) in [
        (r#"pure("bd sd").polyJoin()"#, 1),
        (r#"pure("bd sd").stepJoin()"#, 1),
        (r#"pure("bd sd hh").stepJoin()"#, 2),
        (r#"pure(pure('bd')).polyJoin()"#, 1),
    ] {
        let rt = runtime();
        eval(&rt, source);
        let pattern = rt.active_pattern().expect("active pattern");
        assert!(
            pattern.is_pure(),
            "{source} reaches no JavaScript and must classify pure"
        );
        // No `with_callback_host`: a false-pure graph panics here.
        let haps = pattern.query_arc(Fraction::ZERO, Fraction::int(cycles));
        assert!(
            !haps.is_empty(),
            "{source} produced nothing when queried with no host"
        );
        rt.clear_active();
    }
}

#[test]
fn a_query_error_pattern_is_pure_and_needs_no_host() {
    // `polyBind(42)` builds a `QueryError` node rather than throwing, because
    // strudel.cc's `fmap(42)` constructs fine. That node holds no callback, so it
    // must stay pure and must be queryable with no host - otherwise a
    // live-coded typo would panic instead of emptying the query.
    let rt = runtime();
    eval(&rt, r#"pure('bd').polyBind(42)"#);
    let pattern = rt.active_pattern().expect("active pattern");
    assert!(
        pattern.is_pure(),
        "a constant query failure holds no JavaScript and must classify pure"
    );
    assert!(
        pattern.query_arc(Fraction::ZERO, Fraction::ONE).is_empty(),
        "a query error must empty the result"
    );
    rt.clear_active();
}
