//! Purity is enforced by representation.
//!
//! > A pattern classified PURE must never enter QuickJS.
//!
//! The decisive test is negative and needs no runtime at all: query a pure
//! pattern with **no callback host installed**. If the classification is wrong,
//! the host lookup panics instead of silently entering JS. A false-pure
//! classification would hand a pattern the tight lookahead and then block on a
//! JavaScript call in the real-time scheduling path.
//!
//! False-impure costs latency. False-pure costs correctness. Every unknown or
//! dynamic path therefore defaults to impure.

use rustel_core::{
    Pattern, Value, fastcat, js_query, pure, register::default_registry, silence, stack,
};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};

fn atom(s: &str) -> Pattern {
    pure(Value::Str(s.into()))
}

fn seq() -> Pattern {
    fastcat(vec![atom("bd"), atom("sd")])
}

/// Case 1: patterns with no JS anywhere are pure, and querying them without a
/// host installed succeeds. This is the whole point of the classification.
#[test]
fn pure_patterns_query_with_no_host_installed() {
    let cases: Vec<Pattern> = vec![
        atom("bd"),
        silence(),
        seq(),
        stack(vec![seq(), atom("hh")]),
        seq().fast(Fraction::int(2)),
        seq().slow(Fraction::int(3)),
        seq().compress(Fraction::new(1, 4), Fraction::new(3, 4)),
        seq().late(Fraction::new(1, 8)),
        {
            let reg = default_registry();
            let fast = reg.get("fast").unwrap();
            fast.call(&[atom("2")], seq())
        },
        {
            // register()'s GENERAL path: a patterned argument. Still pure,
            // because the argument contains no JS.
            let reg = default_registry();
            let fast = reg.get("fast").unwrap();
            fast.call(&[fastcat(vec![atom("2"), atom("3")])], seq())
        },
    ];

    for (i, p) in cases.iter().enumerate() {
        assert!(p.is_pure(), "case {i} should be pure");
        assert!(
            p.reachable_callbacks().is_empty(),
            "case {i}: a pure pattern reaches no callbacks"
        );
        assert!(
            p.as_pure_pattern().is_some(),
            "case {i}: must upgrade to PurePattern"
        );
        // The real assertion: no host is installed, so any entry into JS panics.
        let haps = p.query_arc(Fraction::ZERO, Fraction::int(2));
        let _ = haps.len();
    }
}

/// Case 2: anything reaching a callback is impure, and impurity is
/// **monotonic** - it survives every wrapper.
#[test]
fn impurity_is_monotonic() {
    const ID: usize = 7;
    let impure = seq().fmap_js(ID);
    assert!(!impure.is_pure());
    assert_eq!(impure.reachable_callbacks(), &[ID]);

    // Every wrapping must preserve impurity. If any constructor forgot to
    // propagate it, one of these would come back pure and the tight-lookahead
    // path would accept it.
    let wrapped: Vec<Pattern> = vec![
        impure.fast(Fraction::int(2)),
        impure.slow(Fraction::int(2)),
        impure.late(Fraction::new(1, 4)),
        impure.early(Fraction::new(1, 4)),
        impure.compress(Fraction::ZERO, Fraction::new(1, 2)),
        impure.fast_gap(Fraction::int(2)),
        impure.split_queries(),
        impure.fmap(|v| v.clone()),
        stack(vec![atom("bd"), impure.clone()]),
        rustel_core::slowcat(vec![atom("bd"), impure.clone()]),
        rustel_core::fastcat(vec![atom("bd"), impure.clone()]),
        impure.with_added_context(vec![(1, 2)]),
        {
            let reg = default_registry();
            let fast = reg.get("fast").unwrap();
            // Impure pattern as the OPERAND.
            fast.call(&[atom("2")], impure.clone())
        },
        {
            let reg = default_registry();
            let fast = reg.get("fast").unwrap();
            // Impure pattern as the ARGUMENT - the general path.
            fast.call(std::slice::from_ref(&impure), seq())
        },
    ];

    for (i, p) in wrapped.iter().enumerate() {
        assert!(
            !p.is_pure(),
            "wrapper {i} LOST impurity - this is a false-pure classification, \
             which would grant the tight lookahead to a pattern that enters JS"
        );
        assert!(
            p.reachable_callbacks().contains(&ID),
            "wrapper {i} lost the reachable callback id"
        );
        assert!(
            p.as_pure_pattern().is_none(),
            "wrapper {i} must not upgrade to PurePattern"
        );
    }
}

/// Case 3: a user-authored `new Pattern(state => …)` is always impure.
#[test]
fn user_query_function_is_always_impure() {
    let p = js_query(3);
    assert!(!p.is_pure());
    assert_eq!(p.reachable_callbacks(), &[3]);
    assert!(p.as_pure_pattern().is_none());
}

/// Case 4: the reachable set is the union over the whole graph, deduplicated -
/// this is what L2's `gc_mark` walks, so it must be complete and O(k).
#[test]
fn reachable_set_is_deduplicated_union() {
    let a = seq().fmap_js(1);
    let b = seq().fmap_js(2);
    let c = a.fmap_js(1); // id 1 again
    let combined = stack(vec![a, b, c, atom("bd")]);
    assert_eq!(
        combined.reachable_callbacks(),
        &[1, 2],
        "union must be sorted and deduplicated"
    );
}

// A JavaScript callback under the general registration path is impure because
// its pattern is materialised only at query time.

/// The exact reproducer. Kept verbatim so it cannot be lost to refactoring.
#[test]
fn reproducer_js_callback_under_register_general_path() {
    use rustel_core::register::{
        CombinatorFn, DeclaredIn, FunctionArgument, JoinKind, Registration, Registry,
    };
    use std::sync::Arc;

    const CB: usize = 0;
    let mut reg = Registry::new();
    reg.register(Registration {
        declared_in: DeclaredIn::PatternModule,
        function_argument: FunctionArgument::None,
        names: vec![Arc::from("tagged")],
        reference: rustel_core::reference::ReferenceEntry::blank("tagged"),
        arity: 2,
        patternify: true,
        preserve_steps: false,
        join: JoinKind::Inner,
        // Opaque body that materialises a JS-backed pattern.
        func: CombinatorFn::Dynamic(Arc::new(move |_a: &[Value], p: Pattern| p.fmap_js(CB))),
    });

    // Patterned leading argument -> general path -> lazy materialisation.
    let p = reg
        .get("tagged")
        .unwrap()
        .call(&[fastcat(vec![atom("1"), atom("2")])], seq());

    assert!(
        !p.is_pure(),
        "FALSE-PURE: a JS callback materialised at query time was classified          pure. This pattern would receive the tight lookahead and then block on          QuickJS inside the audio deadline (risk R15)."
    );
    assert!(p.as_pure_pattern().is_none());
    assert!(
        p.purity().opaque,
        "an opaque closure's reachable set is incomplete, so gc_mark must mark          conservatively"
    );
}

/// Nested callbacks: impurity must survive arbitrary depth.
#[test]
fn nested_callbacks_stay_impure() {
    let deep = seq()
        .fmap_js(1)
        .fast(Fraction::int(2))
        .fmap_js(2)
        .compress(Fraction::ZERO, Fraction::new(1, 2))
        .fmap_js(3);
    assert!(!deep.is_pure());
    assert_eq!(deep.reachable_callbacks(), &[1, 2, 3]);
    assert!(deep.as_pure_pattern().is_none());
}

/// A callback that RETURNS a pattern - the `fill`/`glide` shape, materialised
/// lazily. The dynamic constructor must classify it impure regardless of what
/// the accumulator looks like.
#[test]
fn callback_returned_patterns_are_impure() {
    // Accumulator is pure; the closure materialises JS at query time.
    let acc = fastcat(vec![atom("1"), atom("2")]);
    let p = acc.fmap_to_pattern(|_v| js_query(9)).inner_join();
    assert!(
        !p.is_pure(),
        "a pattern materialised by an opaque closure must be impure even when          the accumulator is pure - this is the exact hole that was missed"
    );
    assert!(p.purity().opaque);
}

/// An opaque closure must not inherit its accumulator's purity through
/// `PatternOf`.
#[test]
fn negative_control_dynamic_must_not_inherit_accumulator_purity() {
    let pure_acc = fastcat(vec![atom("1"), atom("2")]);
    assert!(pure_acc.is_pure(), "precondition: accumulator is pure");

    let dynamic = pure_acc.fmap_to_pattern(|_v| seq().fmap_js(11));
    assert!(
        !dynamic.is_pure(),
        "PatternOf inherited the accumulator's purity through an opaque closure"
    );

    // And the sound constructor, given a provably-pure closure, MAY inherit.
    let statically_pure = pure_acc.fmap_to_pure_pattern(|_v| seq().as_pure_pattern().unwrap());
    assert!(
        statically_pure.is_pure(),
        "a closure whose return type proves purity may inherit it"
    );
}

/// A pattern that is impure must panic when
/// queried with no host. Without this, "pure patterns query fine" proves
/// nothing - it could be that nothing ever consults the host.
#[test]
#[should_panic(expected = "no host installed")]
fn impure_pattern_without_host_panics() {
    let p = seq().fmap_js(0);
    let _ = p.query_arc(Fraction::ZERO, Fraction::ONE);
}

/// Purity at the callback bridge: a pattern is impure when JavaScript can
/// run at query time, not whenever the score names a function.
#[test]
fn query_time_javascript_is_always_impure() {
    for source in [
        // Patterned first argument: each hap applies the transformer at the
        // query. A free variable keeps the callback JavaScript.
        r#"const k = 2; s("bd sd").every("<2 3>", x => x.fast(k))"#,
        r#"const k = 2; s("bd sd").sometimesBy("<0.3 0.6>", x => x.speed(k))"#,
        // Scalar probability: the combinator body's own fmap is lazy.
        r#"const k = 2; s("bd sd").sometimesBy(1, x => x.speed(k))"#,
        r#"const k = 2; s("bd sd").someCyclesBy(1, x => x.speed(k))"#,
    ] {
        let rt = JsRuntime::new().unwrap();
        rt.install_semantic_bindings().unwrap();
        rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let pattern = rt.active_pattern().expect("active pattern");
        assert!(
            !pattern.is_pure(),
            "{source} resolves its transformer at QUERY time and must be impure"
        );
        assert!(
            !pattern.reachable_callbacks().is_empty() || pattern.purity().opaque,
            "{source}: an impure graph must either enumerate its callbacks or \
             declare the set incomplete, so gc_mark knows what to keep"
        );
    }
}

/// The executable form of the purity claim: anything classified PURE must
/// query with **no callback host installed**. If the classification were
/// wrong, the host lookup panics instead of silently entering JS.
///
/// A callback that eagerly applied JavaScript built leaves a pure graph, and
/// so does a callback compiled to a native program, even where the
/// combinator applies it lazily. A block body keeps a callback JavaScript.
#[test]
fn eager_and_compiled_callbacks_leave_a_genuinely_pure_graph() {
    for (source, compiled) in [
        (
            r#"s("bd sd hh cp").every(2, x => { return x.fast(2) })"#,
            false,
        ),
        (r#"s("bd sd").jux(x => { return x.rev() })"#, false),
        (
            r#"s("bd sd").superimpose(x => { return x.fast(2) })"#,
            false,
        ),
        (r#"s("bd sd hh cp").every(2, x => x.fast(2))"#, true),
        (r#"s("bd sd").jux(x => x.rev())"#, true),
        (r#"s("bd sd").superimpose(x => x.fast(2))"#, true),
        (r#"s("bd sd").sometimesBy(1, x => x.speed(2))"#, true),
        (r#"s("bd sd").someCyclesBy(1, x => x.speed(2))"#, true),
        // A patterned first argument applies the compiled callback at the
        // query, once for each value.
        (r#"s("bd sd").every("<2 3>", x => x.fast(2))"#, true),
        (
            r#"s("bd sd").sometimesBy("<0.3 0.6>", x => x.speed(2))"#,
            true,
        ),
    ] {
        let rt = JsRuntime::new().unwrap();
        rt.install_semantic_bindings().unwrap();
        rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let stats = rt.pattern_transform_ir_stats();
        assert_eq!(stats.lowered > 0, compiled, "{source}: {stats:?}");
        let pattern = rt.active_pattern().expect("active pattern");
        let pure_view = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: eager application leaves no JS behind"));
        // No host: a wrong classification panics here rather than entering JS.
        let haps = pure_view.query_arc(Fraction::ZERO, Fraction::ONE);
        assert!(!haps.is_empty(), "{source} produced nothing");
    }
}

/// ...and a graph built from the SAME combinators with native references stays
/// pure, so the classification is not simply "everything is impure now".
#[test]
fn native_combinator_references_stay_pure() {
    for source in [
        r#"s("bd sd").every(2, rev)"#,
        r#"s("bd sd").jux(rev)"#,
        r#"s("bd sd").off(0.25, fast(2))"#,
    ] {
        let rt = JsRuntime::new().unwrap();
        rt.install_semantic_bindings().unwrap();
        rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let pattern = rt.active_pattern().expect("active pattern");
        assert!(
            pattern.is_pure(),
            "{source} contains no JavaScript and must stay on the pure path"
        );
    }
}

/// A factor pattern may itself call JavaScript at query time. `stepRegister`
/// must retain that callback through the StepJoin graph, mark the result
/// impure, and keep the cell alive across a real QuickJS collection.
#[test]
fn js_owned_stepwise_factors_survive_gc_and_keep_step_join_semantics() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");

    rt.evaluate_score(
        "sequence(0, 1).replicate(slowcat(1, 2).fmap(x => x))",
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct callback-owned replicate factor");
    let replicate = rt.active_pattern().expect("active replicate graph");
    assert!(!replicate.is_pure(), "query-time factor was called pure");
    assert!(
        !replicate.reachable_callbacks().is_empty(),
        "query-time factor is not rooted by the active graph"
    );
    assert_eq!(replicate.steps(), Some(Fraction::int(2)));

    let snapshot = |begin, end| {
        rt.query(Slot::Active, 0, begin, end)
            .expect("query callback-owned replicate")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>()
    };
    let joined = snapshot(Fraction::ZERO, Fraction::int(2));
    assert_eq!(
        joined,
        [
            "[ 0/1 → 1/2 | 0 ]",
            "[ 1/2 → 1/1 | 1 ]",
            "[ 1/1 → 3/2 | 0 ]",
            "[ 3/2 → 2/1 | 1 ]",
        ],
        "StepJoin stopped anchoring its carrier to the joined query window"
    );
    let second_cycle = snapshot(Fraction::ONE, Fraction::int(2));
    assert_eq!(
        second_cycle,
        [
            "[ 1/1 → 5/4 | 0 ]",
            "[ 5/4 → 3/2 | 1 ]",
            "[ 3/2 → 7/4 | 0 ]",
            "[ 7/4 → 2/1 | 1 ]",
        ],
        "a separately queried cycle lost its factor-2 branch"
    );
    rt.run_gc();
    rt.run_gc();
    assert_eq!(
        snapshot(Fraction::ONE, Fraction::int(2)),
        second_cycle,
        "replicate's patterned-factor callback was collected"
    );

    rt.evaluate_score(
        "sequence(0, 1).contract(sequence(1, 2).fmap(x => x))",
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct callback-owned contract factor");
    let contract = rt.active_pattern().expect("active contract graph");
    assert!(!contract.is_pure(), "query-time factor was called pure");
    assert!(
        !contract.reachable_callbacks().is_empty(),
        "contract factor is not rooted by the active graph"
    );
    assert_eq!(contract.steps(), Some(Fraction::int(3)));
    let before = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query callback-owned contract")
        .into_iter()
        .map(|hap| hap.show())
        .collect::<Vec<_>>();
    assert_eq!(
        before,
        [
            "[ 0/1 → 1/3 | 0 ]",
            "[ 1/3 → 2/3 | 1 ]",
            "[ 2/3 → 5/6 | 0 ]",
            "[ 5/6 → 1/1 | 1 ]",
        ]
    );
    rt.run_gc();
    rt.run_gc();
    let after = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query contract after GC")
        .into_iter()
        .map(|hap| hap.show())
        .collect::<Vec<_>>();
    assert_eq!(
        after, before,
        "contract's patterned-factor callback was collected"
    );
}

/// A patterned amount can own JavaScript and `stepRegister` evaluates its
/// cycle-zero slice while constructing StepJoin metadata. The callback must
/// remain reachable for later windows and survive collection. Joined and
/// separately queried windows intentionally differ: StepJoin anchors the
/// carrier to the query's begin cycle.
#[test]
fn js_owned_take_drop_amounts_survive_gc_and_keep_step_join_windows() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");

    rt.evaluate_score(
        "sequence(0, 1, 2).take(slowcat(1, 2).fmap(x => x))",
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct callback-owned take amount");
    let take = rt.active_pattern().expect("active take graph");
    assert!(!take.is_pure(), "query-time take amount was called pure");
    assert!(
        !take.reachable_callbacks().is_empty(),
        "query-time take amount is not rooted by the graph"
    );
    assert_eq!(take.steps(), Some(Fraction::ONE));

    let snapshot = |begin, end| {
        rt.query(Slot::Active, 0, begin, end)
            .expect("query callback-owned take")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>()
    };
    let take_joined = snapshot(Fraction::ZERO, Fraction::int(2));
    assert_eq!(
        take_joined,
        ["[ 0/1 → 1/1 | 0 ]", "[ 1/1 → 2/1 | 0 ]"],
        "joined take query stopped using its begin-cycle carrier"
    );
    let take_second = snapshot(Fraction::ONE, Fraction::int(2));
    assert_eq!(
        take_second,
        ["[ 1/1 → 3/2 | 0 ]", "[ 3/2 → 2/1 | 1 ]"],
        "separate take query lost cycle one's amount"
    );
    rt.run_gc();
    rt.run_gc();
    assert_eq!(
        snapshot(Fraction::ONE, Fraction::int(2)),
        take_second,
        "take's patterned-amount callback was collected"
    );

    rt.evaluate_score(
        "sequence(0, 1, 2).drop(slowcat(1, 2).fmap(x => x))",
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct callback-owned drop amount");
    let drop = rt.active_pattern().expect("active drop graph");
    assert!(!drop.is_pure(), "query-time drop amount was called pure");
    assert!(
        !drop.reachable_callbacks().is_empty(),
        "query-time drop amount is not rooted by the graph"
    );
    assert_eq!(drop.steps(), Some(Fraction::int(2)));
    let drop_joined = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::int(2))
        .expect("query callback-owned drop")
        .into_iter()
        .map(|hap| hap.show())
        .collect::<Vec<_>>();
    assert_eq!(
        drop_joined,
        [
            "[ 0/1 → 1/2 | 1 ]",
            "[ 1/2 → 1/1 | 2 ]",
            "[ 1/1 → 3/2 | 1 ]",
            "[ 3/2 → 2/1 | 2 ]",
        ],
        "joined drop query stopped using its begin-cycle carrier"
    );
    let drop_second = rt
        .query(Slot::Active, 0, Fraction::ONE, Fraction::int(2))
        .expect("query cycle-one drop")
        .into_iter()
        .map(|hap| hap.show())
        .collect::<Vec<_>>();
    assert_eq!(drop_second, ["[ 1/1 → 2/1 | 2 ]"]);
    rt.run_gc();
    rt.run_gc();
    let drop_after_gc = rt
        .query(Slot::Active, 0, Fraction::ONE, Fraction::int(2))
        .expect("query drop after GC")
        .into_iter()
        .map(|hap| hap.show())
        .collect::<Vec<_>>();
    assert_eq!(
        drop_after_gc, drop_second,
        "drop's patterned-amount callback was collected"
    );
}

/// A JavaScript-owned numeric factor is queried once by StepJoin while the
/// result's cycle-zero `_steps` are constructed, then once per later query.
/// Keep that callback rooted through two collections and pin the intentional
/// joined-versus-separate window difference at the same time.
#[test]
fn js_owned_shrink_grow_amounts_survive_gc_and_keep_step_join_windows() {
    const SHRINK_JOINED: &[&str] = &[
        "[ 0/1 → 1/10 | 0 ]",
        "[ 1/10 → 1/5 | 1 ]",
        "[ 1/5 → 3/10 | 2 ]",
        "[ 3/10 → 2/5 | 3 ]",
        "[ 2/5 → 1/2 | 1 ]",
        "[ 1/2 → 3/5 | 2 ]",
        "[ 3/5 → 7/10 | 3 ]",
        "[ 7/10 → 4/5 | 2 ]",
        "[ 4/5 → 9/10 | 3 ]",
        "[ 9/10 → 1/1 | 3 ]",
        "[ 1/1 → 11/10 | 0 ]",
        "[ 11/10 → 6/5 | 1 ]",
        "[ 6/5 → 13/10 | 2 ]",
        "[ 13/10 → 7/5 | 3 ]",
        "[ 7/5 → 3/2 | 1 ]",
        "[ 3/2 → 8/5 | 2 ]",
        "[ 8/5 → 17/10 | 3 ]",
        "[ 17/10 → 9/5 | 2 ]",
        "[ 9/5 → 19/10 | 3 ]",
        "[ 19/10 → 2/1 | 3 ]",
    ];
    const SHRINK_SECOND: &[&str] = &[
        "[ 1/1 → 7/6 | 0 ]",
        "[ 7/6 → 4/3 | 1 ]",
        "[ 4/3 → 3/2 | 2 ]",
        "[ 3/2 → 5/3 | 3 ]",
        "[ 5/3 → 11/6 | 2 ]",
        "[ 11/6 → 2/1 | 3 ]",
    ];
    const GROW_JOINED: &[&str] = &[
        "[ 0/1 → 1/10 | 0 ]",
        "[ 1/10 → 1/5 | 0 ]",
        "[ 1/5 → 3/10 | 1 ]",
        "[ 3/10 → 2/5 | 0 ]",
        "[ 2/5 → 1/2 | 1 ]",
        "[ 1/2 → 3/5 | 2 ]",
        "[ 3/5 → 7/10 | 0 ]",
        "[ 7/10 → 4/5 | 1 ]",
        "[ 4/5 → 9/10 | 2 ]",
        "[ 9/10 → 1/1 | 3 ]",
        "[ 1/1 → 11/10 | 0 ]",
        "[ 11/10 → 6/5 | 0 ]",
        "[ 6/5 → 13/10 | 1 ]",
        "[ 13/10 → 7/5 | 0 ]",
        "[ 7/5 → 3/2 | 1 ]",
        "[ 3/2 → 8/5 | 2 ]",
        "[ 8/5 → 17/10 | 0 ]",
        "[ 17/10 → 9/5 | 1 ]",
        "[ 9/5 → 19/10 | 2 ]",
        "[ 19/10 → 2/1 | 3 ]",
    ];
    const GROW_SECOND: &[&str] = &[
        "[ 1/1 → 7/6 | 0 ]",
        "[ 7/6 → 4/3 | 1 ]",
        "[ 4/3 → 3/2 | 0 ]",
        "[ 3/2 → 5/3 | 1 ]",
        "[ 5/3 → 11/6 | 2 ]",
        "[ 11/6 → 2/1 | 3 ]",
    ];

    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");
    let snapshot = |begin, end| {
        rt.query(Slot::Active, 0, begin, end)
            .expect("query callback-owned shrink/grow")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>()
    };

    rt.eval("globalThis.shrinkFactorCalls = 0;")
        .expect("install shrink counter");
    rt.evaluate_score(
        "sequence(0, 1, 2, 3).shrink(\
           slowcat(1, 2).fmap(x => (shrinkFactorCalls++, x)))",
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct callback-owned shrink amount");
    let shrink = rt.active_pattern().expect("active shrink graph");
    assert!(
        !shrink.is_pure(),
        "query-time shrink amount was called pure"
    );
    assert!(!shrink.reachable_callbacks().is_empty());
    assert_eq!(shrink.steps(), Some(Fraction::int(10)));
    assert_eq!(rt.get_number("shrinkFactorCalls"), Some(1.0));
    assert_eq!(snapshot(Fraction::ZERO, Fraction::int(2)), SHRINK_JOINED);
    assert_eq!(rt.get_number("shrinkFactorCalls"), Some(2.0));
    let shrink_second = snapshot(Fraction::ONE, Fraction::int(2));
    assert_eq!(shrink_second, SHRINK_SECOND);
    assert_eq!(rt.get_number("shrinkFactorCalls"), Some(3.0));
    rt.run_gc();
    rt.run_gc();
    assert_eq!(
        snapshot(Fraction::ONE, Fraction::int(2)),
        shrink_second,
        "shrink's patterned amount was collected"
    );
    assert_eq!(rt.get_number("shrinkFactorCalls"), Some(4.0));

    rt.eval("globalThis.growFactorCalls = 0;")
        .expect("install grow counter");
    rt.evaluate_score(
        "sequence(0, 1, 2, 3).grow(\
           slowcat(1, 2).fmap(x => (growFactorCalls++, x)))",
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct callback-owned grow amount");
    let grow = rt.active_pattern().expect("active grow graph");
    assert!(!grow.is_pure(), "query-time grow amount was called pure");
    assert!(!grow.reachable_callbacks().is_empty());
    assert_eq!(grow.steps(), Some(Fraction::int(10)));
    assert_eq!(rt.get_number("growFactorCalls"), Some(1.0));
    assert_eq!(snapshot(Fraction::ZERO, Fraction::int(2)), GROW_JOINED);
    assert_eq!(rt.get_number("growFactorCalls"), Some(2.0));
    let grow_second = snapshot(Fraction::ONE, Fraction::int(2));
    assert_eq!(grow_second, GROW_SECOND);
    assert_eq!(rt.get_number("growFactorCalls"), Some(3.0));
    rt.run_gc();
    rt.run_gc();
    assert_eq!(
        snapshot(Fraction::ONE, Fraction::int(2)),
        grow_second,
        "grow's patterned amount was collected"
    );
    assert_eq!(rt.get_number("growFactorCalls"), Some(4.0));
}

/// JavaScript functions and objects used as tour values are expanded into
/// several branches. Every copy must share the rooted JS identity and remain
/// usable after collection; retaining only the first occurrence would leave
/// later branches dangling.
#[test]
fn tour_keeps_js_owned_values_alive_across_gc() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");
    rt.evaluate_score(
        r#"
          (() => {
            globalThis.tourOwnedFunction = value => `${value}!`;
            globalThis.tourOwnedObject = { marker: 17 };
            globalThis.tourOwnedPattern = pure('x').tour(
              pure(tourOwnedFunction), pure(tourOwnedObject)
            );
            return tourOwnedPattern;
          })()
        "#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct JS-owned tour");
    let active = rt.active_pattern().expect("active JS-owned tour");
    assert!(!active.is_pure(), "JS-owned tour was classified pure");
    assert!(
        !active.reachable_callbacks().is_empty() || active.purity().opaque,
        "JS-owned tour did not retain an ownership root"
    );

    for _ in 0..3 {
        rt.run_gc();
        rt.evaluate_score(
            r#"
              (() => {
                const state = { span: { begin: 0, end: 1 }, controls: {} };
                const values = tourOwnedPattern.query(state).map(hap => hap.value);
                if (values.length !== 9
                    || values[0] !== tourOwnedFunction
                    || values[1] !== tourOwnedObject
                    || values[2] !== 'x'
                    || values[3] !== tourOwnedFunction
                    || values[4] !== 'x'
                    || values[5] !== tourOwnedObject
                    || values[6] !== 'x'
                    || values[7] !== tourOwnedFunction
                    || values[8] !== tourOwnedObject
                    || values[7]('kept') !== 'kept!'
                    || values[8].marker !== 17) {
                  throw new Error('tour JS ownership or order changed');
                }
                return tourOwnedPattern;
              })();
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("inspect JS-owned tour after GC");
        assert_eq!(
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("query JS-owned tour after GC")
                .len(),
            9
        );
    }
}

/// Repeated occurrences share their original JavaScript-owned values. The
/// final wrapper must retain each unique sidecar across collection without
/// cloning function/object identity or dropping a later expanded occurrence.
#[test]
fn stepalt_keeps_js_owned_values_alive_across_gc() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");
    rt.evaluate_score(
        r#"
          (() => {
            globalThis.stepaltOwnedFunction = value => `${value}!`;
            globalThis.stepaltOwnedObject = { marker: 47 };
            globalThis.stepaltOwnedPattern = stepalt(
              [pure(stepaltOwnedFunction), pure('a')],
              [pure(stepaltOwnedObject), pure('b'), pure('c')]
            );
            return stepaltOwnedPattern;
          })()
        "#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct JS-owned stepalt");
    let active = rt.active_pattern().expect("active JS-owned stepalt");
    assert!(!active.is_pure(), "JS-owned stepalt was classified pure");
    assert!(
        !active.reachable_callbacks().is_empty() || active.purity().opaque,
        "JS-owned stepalt did not retain an ownership root"
    );

    for _ in 0..3 {
        rt.run_gc();
        rt.evaluate_score(
            r#"
              (() => {
                const state = { span: { begin: 0, end: 1 }, controls: {} };
                const values = stepaltOwnedPattern.query(state).map(hap => hap.value);
                if (values.length !== 12
                    || values[0] !== stepaltOwnedFunction
                    || values[1] !== stepaltOwnedObject
                    || values[2] !== 'a'
                    || values[3] !== 'b'
                    || values[4] !== stepaltOwnedFunction
                    || values[5] !== 'c'
                    || values[6] !== 'a'
                    || values[7] !== stepaltOwnedObject
                    || values[8] !== stepaltOwnedFunction
                    || values[9] !== 'b'
                    || values[10] !== 'a'
                    || values[11] !== 'c'
                    || values[8]('kept') !== 'kept!'
                    || values[7].marker !== 47) {
                  throw new Error('stepalt JS ownership or order changed');
                }
                return stepaltOwnedPattern;
              })();
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("inspect JS-owned stepalt after GC");
        assert_eq!(
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("query JS-owned stepalt after GC")
                .len(),
            12
        );
    }
}

/// A polymeter graph that carries JavaScript values or a lane-local callback
/// must stay impure/rooted. Repeated modern pacing and legacy first-array
/// reification both preserve the exact function/object identities across GC.
#[test]
fn polymeter_keeps_js_owned_values_and_callbacks_alive_across_gc() {
    for (label, expression, expected_values, callback_calls) in [
        (
            "modern",
            r#"polymeter(
              sequence(pure(polymeterOwnedFunction), pure('a'))
                .fmap(polymeterOwnedMap),
              sequence(pure(polymeterOwnedObject), pure('b'), pure('c'))
                .fmap(polymeterOwnedMap)
            )"#,
            "F,F,F,O,O,a,a,a,b,b,c,c",
            12,
        ),
        (
            "legacy",
            r#"s_polymeter(
              [
                pure(polymeterOwnedFunction).fmap(polymeterOwnedMap),
                pure('a').fmap(polymeterOwnedMap)
              ],
              [
                pure(polymeterOwnedObject).fmap(polymeterOwnedMap),
                pure('b').fmap(polymeterOwnedMap),
                pure('c').fmap(polymeterOwnedMap)
              ]
            )"#,
            "F,O,a,b",
            4,
        ),
    ] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.polymeterOwnedFunction = value => `${{value}}!`;
                    globalThis.polymeterOwnedObject = {{ marker: 53 }};
                    globalThis.polymeterOwnedCallbackCalls = 0;
                    globalThis.polymeterOwnedMap = value => {{
                      polymeterOwnedCallbackCalls++;
                      if (value === polymeterOwnedFunction) return 'F';
                      if (value === polymeterOwnedObject) return 'O';
                      return value;
                    }};
                    globalThis.polymeterOwnedPattern = {expression};
                    return polymeterOwnedPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {label} polymeter: {error}"));
        let active = rt.active_pattern().expect("active JS-owned polymeter");
        assert!(!active.is_pure(), "JS-owned {label} polymeter was pure");
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "JS-owned {label} polymeter retained no ownership root"
        );

        for _ in 0..3 {
            rt.run_gc();
            rt.eval(
                "polymeterOwnedCallbackCalls = 0; \
                 globalThis.polymeterOwnedIdentityProof = Number(\
                   polymeterOwnedFunction('kept') === 'kept!' \
                   && polymeterOwnedObject.marker === 53\
                 );",
            )
            .unwrap_or_else(|error| panic!("inspect JS-owned {label} values: {error}"));
            assert_eq!(rt.get_number("polymeterOwnedIdentityProof"), Some(1.0));
            let mut values = rt
                .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .unwrap_or_else(|error| panic!("query JS-owned {label} polymeter: {error}"))
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>();
            values.sort();
            assert_eq!(
                values.join(","),
                expected_values,
                "{label} polymeter changed JS-owned values after GC"
            );
            assert_eq!(
                rt.get_number("polymeterOwnedCallbackCalls"),
                Some(f64::from(callback_calls)),
                "{label} polymeter lost or duplicated its retained callback"
            );
        }
    }
}

/// Function and object values carried by zip's constituent patterns keep the
/// exact JavaScript identities of every repeated occurrence. The graph must
/// remain impure/rooted while those values are reachable and survive repeated
/// collection without introducing a new query callback family.
#[test]
fn zip_keeps_js_owned_values_alive_across_gc() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");
    rt.evaluate_score(
        r#"
          (() => {
            globalThis.zipOwnedFunction = value => `${value}!`;
            globalThis.zipOwnedObject = { marker: 31 };
            globalThis.zipOwnedPattern = zip(
              sequence(pure(zipOwnedFunction), pure('a')),
              sequence(pure(zipOwnedObject), pure('b'), pure('c'))
            );
            return zipOwnedPattern;
          })()
        "#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct JS-owned zip");
    let active = rt.active_pattern().expect("active JS-owned zip");
    assert!(!active.is_pure(), "JS-owned zip was classified pure");
    assert!(
        !active.reachable_callbacks().is_empty() || active.purity().opaque,
        "JS-owned zip did not retain an ownership root"
    );

    for _ in 0..3 {
        rt.run_gc();
        rt.evaluate_score(
            r#"
              (() => {
                const state = { span: { begin: 0, end: 1 }, controls: {} };
                const values = zipOwnedPattern.query(state).map(hap => hap.value);
                if (values.length !== 6
                    || values[0] !== zipOwnedFunction
                    || values[1] !== zipOwnedObject
                    || values[2] !== 'a'
                    || values[3] !== 'b'
                    || values[4] !== zipOwnedFunction
                    || values[5] !== 'c'
                    || values[4]('kept') !== 'kept!'
                    || values[1].marker !== 31) {
                  throw new Error('zip JS ownership or order changed');
                }
                return zipOwnedPattern;
              })();
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("inspect JS-owned zip after GC");
        assert_eq!(
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("query JS-owned zip after GC")
                .len(),
            6
        );
    }
}

/// Distinct repeated zoom wrappers must all preserve the same JavaScript-owned
/// values after the temporary source/list variables have left scope. This
/// covers both a list element retained alone and a final stepcat composition.
#[test]
fn shrinklist_entries_keep_js_owned_values_alive_across_gc() {
    for (label, expression, expected) in [
        (
            "retained-element",
            r#"(() => {
              const ownedFunction = value => `${value}!`;
              const ownedObject = { marker: 71 };
              const source = sequence(
                pure(ownedFunction),
                pure(ownedObject),
                pure('tail')
              );
              const list = source.shrinklist([-1, 2]);
              return list[1];
            })()"#,
            "retained-element",
        ),
        (
            "composed-zero",
            r#"(() => {
              const ownedFunction = value => `${value}!`;
              const ownedObject = { marker: 71 };
              const source = sequence(
                pure(ownedFunction),
                pure(ownedObject)
              );
              const list = source.shrinklist([0, 2]);
              if (list[0] === list[1]) {
                throw new Error('zero-amount list wrappers were reused');
              }
              return stepcat(...list);
            })()"#,
            "composed-zero",
        ),
    ] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.shrinklistOwnedPattern = {expression};
                    return shrinklistOwnedPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {label} shrinklist: {error}"));
        let active = rt.active_pattern().expect("active JS-owned list graph");
        assert!(!active.is_pure(), "JS-owned {label} list graph was pure");
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "JS-owned {label} list graph retained no ownership root"
        );

        for _ in 0..3 {
            rt.run_gc();
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = { span: { begin: 0, end: 1 }, controls: {} };
                    const values = shrinklistOwnedPattern.query(state)
                      .map(hap => hap.value);
                    if (values[0]('kept') !== 'kept!'
                        || values[1].marker !== 71) {
                      throw new Error('shrinklist JS values were not retained');
                    }
                    if (values.length === 2) {
                      if (typeof values[0] !== 'function'
                          || typeof values[1] !== 'object') {
                        throw new Error('retained list element changed shape');
                      }
                    } else if (values.length === 4) {
                      if (values[0] !== values[2] || values[1] !== values[3]
                          || typeof values[0] !== 'function'
                          || typeof values[1] !== 'object') {
                        throw new Error('composed list lost repeated identity');
                      }
                    } else {
                      throw new Error(`unexpected shrinklist value count: ${values.length}`);
                    }
                    globalThis.shrinklistOwnedView = values.length === 2
                      ? 'retained-element' : 'composed-zero';
                    return shrinklistOwnedPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("inspect JS-owned {label} shrinklist: {error}"));
            assert_eq!(
                rt.get_string("shrinklistOwnedView").as_deref(),
                Some(expected),
                "{label}: JS-owned list values changed after GC"
            );
        }
    }
}
