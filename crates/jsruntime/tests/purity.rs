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
    use rustel_core::register::{CombinatorFn, DeclaredIn, JoinKind, Registration, Registry};
    use std::sync::Arc;

    const CB: usize = 0;
    let mut reg = Registry::new();
    reg.register(Registration {
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
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

/// Both `register()` paths, with a JS callback in each position.
#[test]
fn js_callback_impure_under_both_register_paths() {
    use rustel_core::register::{CombinatorFn, DeclaredIn, JoinKind, Registration, Registry};
    use std::sync::Arc;

    const CB: usize = 4;
    let mut reg = Registry::new();
    reg.register(Registration {
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        names: vec![Arc::from("jsfn")],
        reference: rustel_core::reference::ReferenceEntry::blank("jsfn"),
        arity: 2,
        patternify: true,
        preserve_steps: false,
        join: JoinKind::Inner,
        func: CombinatorFn::Dynamic(Arc::new(move |_a: &[Value], p: Pattern| p.fmap_js(CB))),
    });
    let entry = reg.get("jsfn").unwrap();

    // Fast path: pure leading argument.
    assert!(!entry.call(&[atom("2")], seq()).is_pure(), "fast path");
    // General path: patterned leading argument.
    assert!(
        !entry
            .call(&[fastcat(vec![atom("2"), atom("3")])], seq())
            .is_pure(),
        "general path"
    );
    // Native combinator, impure OPERAND - impurity must survive the wrapper.
    let native = default_registry();
    let fast = native.get("fast").unwrap();
    assert!(
        !fast.call(&[atom("2")], seq().fmap_js(CB)).is_pure(),
        "native combinator over an impure operand"
    );
    assert!(
        !fast.call(&[seq().fmap_js(CB)], seq()).is_pure(),
        "native combinator with an impure ARGUMENT"
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
        // Patterned first argument: the transformer is chosen per hap.
        r#"s("bd sd").every("<2 3>", x => x.fast(2))"#,
        r#"s("bd sd").sometimesBy("<0.3 0.6>", x => x.speed(2))"#,
        // Scalar probability: the combinator body's own fmap is lazy.
        r#"s("bd sd").sometimesBy(1, x => x.speed(2))"#,
        r#"s("bd sd").someCyclesBy(1, x => x.speed(2))"#,
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
#[test]
fn eagerly_applied_callbacks_leave_a_genuinely_pure_graph() {
    for source in [
        r#"s("bd sd hh cp").every(2, x => x.fast(2))"#,
        r#"s("bd sd").jux(x => x.rev())"#,
        r#"s("bd sd").superimpose(x => x.fast(2))"#,
    ] {
        let rt = JsRuntime::new().unwrap();
        rt.install_semantic_bindings().unwrap();
        rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
            .unwrap_or_else(|e| panic!("{source}: {e}"));
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

/// Stepwise combinators contain no JavaScript of their own, including the
/// `stepRegister` path selected by a patterned factor. Keep the Rust graph
/// alive after the originating QuickJS heap is gone: a false-pure graph would
/// panic here when it tried to recover a callback from the dead host.
#[test]
fn replicate_and_contract_graphs_remain_host_free_after_runtime_teardown() {
    for source in [
        "sequence(0, 1).replicate(2)",
        "replicate(2, sequence(0, 1))",
        "replicate(2)(sequence(0, 1))",
        "sequence(0, 1).replicate(sequence(1, 2))",
        "sequence(0, 1).contract(2)",
        "contract(2, sequence(0, 1))",
        "contract(2)(sequence(0, 1))",
        "sequence(0, 1).contract(sequence(1, 2))",
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active stepwise graph");
            assert!(pattern.is_pure(), "{source}: native graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free graph retained a callback"
            );
            pattern
        }; // QuickJS runtime and all of its callback cells are gone here.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert!(
            !pure.query_arc(Fraction::ZERO, Fraction::int(2)).is_empty(),
            "{source}: host-free graph produced no haps"
        );
    }
}

/// Raw extend/replicate execute only their outer wrapper eagerly; their bodies
/// deliberately compose the already-registered public methods. Stable scalar
/// native operands must nevertheless collapse to a host-free graph, including
/// the cycle-varying witness that distinguishes repeatCycles from extend.
#[test]
fn raw_extend_replicate_scalar_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expect_haps) in [
        ("sequence(0, 1)._extend(2)", true),
        ("sequence(0, 1)._replicate(2)", true),
        ("sequence(0, 1)._extend(0.5)", true),
        ("sequence(0, 1)._replicate(0.5)", true),
        ("sequence('ignored')._extend(2, sequence(0, 1))", true),
        ("sequence('ignored')._replicate(2, sequence(0, 1))", true),
        ("slowcat(sequence(0,1),sequence(2,3))._extend(2)", true),
        ("slowcat(sequence(0,1),sequence(2,3))._replicate(2)", true),
        ("gap(0)._extend(2)", false),
        ("gap(0)._replicate(2)", false),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct raw {source}: {error}"));
            let pattern = rt
                .active_pattern()
                .expect("active raw extend/replicate graph");
            assert!(pattern.is_pure(), "{source}: native graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free graph retained a callback"
            );
            pattern
        }; // The construction runtime is gone before this query.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            !pure.query_arc(Fraction::ZERO, Fraction::int(2)).is_empty(),
            expect_haps,
            "{source}: raw graph changed after runtime teardown"
        );
    }
}

/// `withSteps` and the raw expand/contract bodies run their metadata callback
/// completely during construction. Pure receivers therefore leave no
/// JavaScript callback behind, and their unchanged query graphs must remain
/// usable after the QuickJS heap that assembled them has been destroyed.
#[test]
fn with_steps_and_raw_expand_contract_scalars_remain_host_free_after_teardown() {
    for source in [
        "sequence(0, 1).withSteps(steps => steps.mul(2))",
        "Pattern.prototype.withSteps.call(sequence(0, 1), steps => steps.div(2))",
        "sequence(0, 1)._expand(2)",
        "sequence(0, 1)._contract(2)",
        "sequence(0, 1)._expand(0.5)",
        "sequence(0, 1)._contract(0.5)",
        "sequence('ignored')._expand(2, sequence(0, 1))",
        "sequence('ignored')._contract(2, sequence(0, 1))",
        "gap(0)._expand(2)",
        "gap(0)._contract(2)",
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct {source}: {error}"));
            let pattern = rt
                .active_pattern()
                .expect("active withSteps/raw expand-contract graph");
            assert!(
                pattern.is_pure(),
                "{source}: metadata-only graph became impure"
            );
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: eager metadata callback remained reachable"
            );
            pattern
        }; // The construction runtime and its eager callback are gone here.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        let _ = pure.query_arc(Fraction::ZERO, Fraction::int(2)).len();
    }
}

/// A copied custom query is the one JavaScript owner that `withSteps` must
/// retain. Its metadata callback and the Fraction object supplied as a raw
/// factor are construction-only: after collection the query-owned value must
/// still be observable while weak references to the factor and its marker are
/// empty. This covers one ordinary Fraction instance; other object and Proxy
/// coercion classes remain residuals.
#[test]
fn with_steps_and_raw_expand_contract_retain_query_but_prune_factor_after_gc() {
    for route in ["withSteps", "_expand", "_contract"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawMetadataQueryCalls = 0;
                    const owned = {{ marker: 89, route: '{route}' }};
                    globalThis.rawMetadataOwnedRef = new WeakRef(owned);
                    const makeFactor = () => {{
                      const marker = {{ marker: 41, route: '{route}' }};
                      const factor = sequence(0)._steps.mul(2);
                      factor.owner = marker;
                      globalThis.rawMetadataFactorRef = new WeakRef(factor);
                      globalThis.rawMetadataFactorOwnerRef = new WeakRef(marker);
                      return factor;
                    }};
                    const query = state => {{
                      rawMetadataQueryCalls++;
                      return pure(owned).query(state);
                    }};
                    const source = new Pattern(query, 2);
                    const result = '{route}' === 'withSteps'
                      ? source.withSteps(steps => steps.mul(makeFactor()))
                      : source['{route}'](makeFactor());
                    globalThis.rawMetadataPattern = result;
                    globalThis.rawMetadataQueryIdentity = Number(
                      result.query === query
                    );
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {route}: {error}"));

        let active = rt
            .active_pattern()
            .expect("active JS-owned metadata result");
        assert!(
            !active.is_pure(),
            "{route}: copied JS query was called pure"
        );
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "{route}: copied JS query retained no ownership root"
        );
        assert_eq!(rt.get_number("rawMetadataQueryCalls"), Some(0.0));
        assert_eq!(rt.get_number("rawMetadataQueryIdentity"), Some(1.0));

        rt.run_gc();
        rt.run_gc();
        rt.run_gc();
        rt.evaluate_score(
            r#"
              (() => {
                const state = { span: { begin: 0, end: 1 }, controls: {} };
                const values = rawMetadataPattern.query(state).map(hap => hap.value);
                globalThis.rawMetadataOwnershipProof = Number(
                  values.length === 1
                    && values[0] === rawMetadataOwnedRef.deref()
                    && values[0]?.marker === 89
                );
                globalThis.rawMetadataFactorPruned = Number(
                  rawMetadataFactorRef.deref() === undefined
                    && rawMetadataFactorOwnerRef.deref() === undefined
                );
                return rawMetadataPattern;
              })()
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("query JS-owned {route} after GC: {error}"));
        assert_eq!(rt.get_number("rawMetadataOwnershipProof"), Some(1.0));
        assert_eq!(rt.get_number("rawMetadataFactorPruned"), Some(1.0));
        assert_eq!(
            rt.get_number("rawMetadataQueryCalls"),
            Some(1.0),
            "{route}: copied query callback count changed after GC"
        );
    }
}

/// The raw range pair executes its public-composer chain completely while the
/// expression is assembled. Stable scalar native receivers therefore retain
/// no JavaScript callback and remain queryable after the constructing runtime
/// is gone. Own-query reassignment is deliberately not covered here: that
/// wider public-composer mutation surface remains residual.
#[test]
fn raw_range_pair_scalar_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expected) in [
        (
            "sequence(0,.5,1)._range(10,20)",
            ["0/1>1/3:10", "1/3>2/3:15", "2/3>1/1:20"],
        ),
        (
            "sequence(0,.5,1)._range(20,10)",
            ["0/1>1/3:20", "1/3>2/3:15", "2/3>1/1:10"],
        ),
        (
            "sequence(-1,0,1)._range2(10,20)",
            ["0/1>1/3:10", "1/3>2/3:15", "2/3>1/1:20"],
        ),
        (
            "sequence(-1,0,1)._range2(20,10)",
            ["0/1>1/3:20", "1/3>2/3:15", "2/3>1/1:10"],
        ),
        (
            "pure(99)._range(10,20,sequence(0,.5,1))",
            ["0/1>1/3:10", "1/3>2/3:15", "2/3>1/1:20"],
        ),
        (
            "pure(99)._range2(10,20,sequence(-1,0,1))",
            ["0/1>1/3:10", "1/3>2/3:15", "2/3>1/1:20"],
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct raw range {source}: {error}"));
            let pattern = rt.active_pattern().expect("active raw range graph");
            assert!(pattern.is_pure(), "{source}: scalar graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: scalar graph retained a callback"
            );
            assert_eq!(pattern.steps, Some(Fraction::int(3)));
            pattern
        }; // The stable native composer wrappers are gone before this query.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE)
                .into_iter()
                .map(|hap| format!("{}>{}:{}", hap.part.begin, hap.part.end, hap.value.show()))
                .collect::<Vec<_>>(),
            expected,
            "{source}: host-free raw range result changed"
        );
    }
}

/// A custom method chain is consumed eagerly, so its receiver, intermediate,
/// bounds, and outer raw-wrapper receiver may be collected. A JavaScript
/// Pattern returned by the final method is different: its query closure and
/// owned value must remain rooted by the active graph across collections.
#[test]
fn raw_range_pair_custom_results_retain_only_returned_js_ownership() {
    for name in ["_range", "_range2"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let chain = if name == "_range" {
            r#"
              Object.defineProperty(target, 'mul', {
                configurable: true,
                get() {
                  rawRangeOwnershipLog.push('mul.get');
                  return function (difference) {
                    rawRangeOwnershipLog.push(
                      `mul.call:${this === target}:${difference}`
                    );
                    return middle;
                  };
                },
              });
              Object.defineProperty(middle, 'add', {
                configurable: true,
                get() {
                  rawRangeOwnershipLog.push('add.get');
                  return function (value) {
                    rawRangeOwnershipLog.push(
                      `add.call:${this === middle}:${value === min}`
                    );
                    return new Pattern(state => {
                      rawRangeOwnershipQueryCalls++;
                      return pure(owned).query(state);
                    });
                  };
                },
              });
            "#
        } else {
            r#"
              Object.defineProperty(target, 'fromBipolar', {
                configurable: true,
                get() {
                  rawRangeOwnershipLog.push('fromBipolar.get');
                  return function () {
                    rawRangeOwnershipLog.push(
                      `fromBipolar.call:${this === target}`
                    );
                    return middle;
                  };
                },
              });
              Object.defineProperty(middle, '_range', {
                configurable: true,
                get() {
                  rawRangeOwnershipLog.push('_range.get');
                  return function (a, b) {
                    rawRangeOwnershipLog.push(
                      `_range.call:${this === middle}:${a === min}:${b === max}`
                    );
                    return new Pattern(state => {
                      rawRangeOwnershipQueryCalls++;
                      return pure(owned).query(state);
                    });
                  };
                },
              });
            "#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawRangeOwnershipLog = [];
                    globalThis.rawRangeOwnershipQueryCalls = 0;
                    const owned = {{ name: '{name}', marker: 89 }};
                    const outer = {{ marker: 'outer' }};
                    const min = {{
                      marker: 'min',
                      valueOf() {{ rawRangeOwnershipLog.push('min.valueOf'); return 2; }},
                    }};
                    const max = {{
                      marker: 'max',
                      valueOf() {{ rawRangeOwnershipLog.push('max.valueOf'); return 4; }},
                    }};
                    const target = {{ marker: 'target' }};
                    const middle = {{ marker: 'middle' }};
                    globalThis.rawRangeOwnedRef = new WeakRef(owned);
                    globalThis.rawRangeOuterRef = new WeakRef(outer);
                    globalThis.rawRangeMinRef = new WeakRef(min);
                    globalThis.rawRangeMaxRef = new WeakRef(max);
                    globalThis.rawRangeTargetRef = new WeakRef(target);
                    globalThis.rawRangeMiddleRef = new WeakRef(middle);
                    {chain}
                    globalThis.rawRangeOwnedPattern =
                      Pattern.prototype.{name}.call(outer, min, max, target);
                    globalThis.rawRangeOwnershipLogSnapshot =
                      JSON.stringify(rawRangeOwnershipLog);
                    return rawRangeOwnedPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {name}: {error}"));

        let expected_log = if name == "_range" {
            r#"["mul.get","max.valueOf","min.valueOf","mul.call:true:2","add.get","add.call:true:true"]"#
        } else {
            r#"["fromBipolar.get","fromBipolar.call:true","_range.get","_range.call:true:true:true"]"#
        };
        assert_eq!(
            rt.get_string("rawRangeOwnershipLogSnapshot").as_deref(),
            Some(expected_log),
            "{name}: custom construction order changed"
        );
        let active = rt.active_pattern().expect("active JS-owned raw range");
        assert!(!active.is_pure(), "{name}: custom query was called pure");
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "{name}: custom result retained no JavaScript ownership root"
        );
        assert_eq!(rt.get_number("rawRangeOwnershipQueryCalls"), Some(0.0));

        rt.run_gc();
        rt.run_gc();
        rt.run_gc();
        rt.evaluate_score(
            r#"
              (() => {
                const state = { span: { begin: 0, end: 1 }, controls: {} };
                const values = rawRangeOwnedPattern.query(state).map(hap => hap.value);
                globalThis.rawRangeReturnedOwnership = Number(
                  values.length === 1
                  && values[0] === rawRangeOwnedRef.deref()
                  && values[0]?.marker === 89
                );
                globalThis.rawRangeTransientPruning = Number(
                  rawRangeOuterRef.deref() === undefined
                  && rawRangeMinRef.deref() === undefined
                  && rawRangeMaxRef.deref() === undefined
                  && rawRangeTargetRef.deref() === undefined
                  && rawRangeMiddleRef.deref() === undefined
                );
                return rawRangeOwnedPattern;
              })()
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("query JS-owned {name} after GC: {error}"));
        assert_eq!(rt.get_number("rawRangeReturnedOwnership"), Some(1.0));
        assert_eq!(rt.get_number("rawRangeTransientPruning"), Some(1.0));
        assert_eq!(rt.get_number("rawRangeOwnershipQueryCalls"), Some(1.0));
        assert_eq!(
            rt.get_string("rawRangeOwnershipLogSnapshot").as_deref(),
            Some(expected_log),
            "{name}: query reread the construction chain"
        );
    }
}

/// Raw `_apply` runs its JavaScript callback while the expression is built.
/// When that callback selects an ordinary native Pattern, neither the callback
/// nor the JavaScript heap is part of the returned graph, so the graph must
/// remain queryable after the constructing runtime has been dropped.
#[test]
fn raw_apply_stable_native_terminals_remain_host_free_after_runtime_teardown() {
    for (source, expected_steps, expected) in [
        (
            "sequence('a','b')._apply(value => value)",
            Fraction::int(2),
            ["0/1>1/2:a", "1/2>1/1:b"],
        ),
        (
            "pure('outer')._apply(value => value, sequence('x','y'))",
            Fraction::int(2),
            ["0/1>1/2:x", "1/2>1/1:y"],
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct raw apply {source}: {error}"));
            let pattern = rt.active_pattern().expect("active raw apply graph");
            assert!(pattern.is_pure(), "{source}: native terminal became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: eager callback leaked into the returned graph"
            );
            assert_eq!(pattern.steps, Some(expected_steps));
            pattern
        };

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE)
                .into_iter()
                .map(|hap| format!("{}>{}:{}", hap.part.begin, hap.part.end, hap.value.show()))
                .collect::<Vec<_>>(),
            expected,
            "{source}: host-free raw apply terminal changed"
        );
    }
}

/// The raw wrapper returns the callback's exact terminal: it must not clone a
/// returned Pattern or merge the transient callback/receiver/target into that
/// Pattern's sidecar.  The selected Pattern still owns either its exact JS hap
/// value or its custom query closure across GC.
#[test]
fn raw_apply_retains_exact_returned_js_ownership_and_prunes_transients() {
    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawApplyOwnershipQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawApplyOwnershipQueryCalls = 0;
                    const owned = {{ route: '{route}', marker: 89 }};
                    const outer = {{ marker: 'outer' }};
                    const target = {{ marker: 'target' }};
                    const ignored = {{ marker: 'ignored' }};
                    const makeResult = {make_result};
                    let returned;
                    const callback = value => {{
                      if (value !== target) throw new Error('wrong raw apply target');
                      returned = makeResult(owned);
                      return returned;
                    }};
                    globalThis.rawApplyOwnedRef = new WeakRef(owned);
                    globalThis.rawApplyOuterRef = new WeakRef(outer);
                    globalThis.rawApplyTargetRef = new WeakRef(target);
                    globalThis.rawApplyIgnoredRef = new WeakRef(ignored);
                    globalThis.rawApplyCallbackRef = new WeakRef(callback);
                    globalThis.rawApplyOwnedPattern =
                      Pattern.prototype._apply.call(
                        outer, callback, target, ignored
                      );
                    globalThis.rawApplyExactReturned = Number(
                      rawApplyOwnedPattern === returned
                    );
                    return rawApplyOwnedPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned raw apply {route}: {error}"));
        assert_eq!(rt.get_number("rawApplyExactReturned"), Some(1.0));
        let active = rt.active_pattern().expect("active JS-owned raw apply");
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned result was granted host-free purity"
        );
        assert_eq!(rt.get_number("rawApplyOwnershipQueryCalls"), Some(0.0));

        rt.run_gc();
        rt.run_gc();
        rt.run_gc();
        rt.evaluate_score(
            r#"
              (() => {
                const state = { span: { begin: 0, end: 1 }, controls: {} };
                const values = rawApplyOwnedPattern.query(state)
                  .map(hap => hap.value);
                globalThis.rawApplyReturnedOwnership = Number(
                  values.length === 1
                  && values[0] === rawApplyOwnedRef.deref()
                  && values[0]?.marker === 89
                );
                globalThis.rawApplyTransientPruning = Number(
                  rawApplyOuterRef.deref() === undefined
                  && rawApplyTargetRef.deref() === undefined
                  && rawApplyIgnoredRef.deref() === undefined
                  && rawApplyCallbackRef.deref() === undefined
                );
                return rawApplyOwnedPattern;
              })()
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("query JS-owned raw apply {route} after GC: {error}"));
        assert_eq!(rt.get_number("rawApplyReturnedOwnership"), Some(1.0));
        assert_eq!(rt.get_number("rawApplyTransientPruning"), Some(1.0));
        assert_eq!(
            rt.get_number("rawApplyOwnershipQueryCalls"),
            Some(if route == "query" { 1.0 } else { 0.0 }),
            "{route}: returned query callback count changed"
        );
    }
}

/// Both raw `_when` branches select an already-built terminal eagerly.  A
/// native terminal therefore remains a pure, host-free graph after the
/// constructing QuickJS runtime is gone; neither a called true-branch
/// callback nor a suppressed false-branch callback may leak into that graph.
#[test]
fn raw_when_native_branches_remain_host_free_after_runtime_teardown() {
    for (branch, source, expected) in [
        (
            "true",
            r#"(() => {
                 let hits = 0;
                 const callback = value => { hits++; return value; };
                 const result = pure('outer')._when(
                   { marker: 'truthy without coercion' }, callback,
                   sequence('x', 'y')
                 );
                 if (hits !== 1) throw new Error('true callback phase changed');
                 return result;
               })()"#,
            ["0/1>1/2:x", "1/2>1/1:y"],
        ),
        (
            "false",
            r#"(() => {
                 let hits = 0;
                 const callback = () => {
                   hits++;
                   throw new Error('false callback was invoked');
                 };
                 const result = pure('outer')._when(
                   0n, callback, sequence('a', 'b')
                 );
                 if (hits !== 0) throw new Error('false callback phase changed');
                 return result;
               })()"#,
            ["0/1>1/2:a", "1/2>1/1:b"],
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct raw when {branch}: {error}"));
            let pattern = rt.active_pattern().expect("active raw when graph");
            assert!(pattern.is_pure(), "{branch}: native terminal became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{branch}: eager branch retained a JavaScript callback"
            );
            assert_eq!(pattern.steps, Some(Fraction::int(2)));
            pattern
        };

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{branch}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE)
                .into_iter()
                .map(|hap| format!("{}>{}:{}", hap.part.begin, hap.part.end, hap.value.show()))
                .collect::<Vec<_>>(),
            expected,
            "{branch}: host-free raw when terminal changed"
        );
    }
}

/// `_when` returns the exact selected JavaScript Pattern.  The selected
/// Pattern must retain its own JS value/query ownership across GC, while the
/// condition, callback, outer receiver, unselected target, captured transient,
/// and ignored extra remain only construction-time values and are pruned.
#[test]
fn raw_when_retains_selected_js_ownership_and_prunes_branch_transients() {
    for branch in ["true", "false"] {
        for route in ["value", "query"] {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            let make_result = if route == "value" {
                "owned => pure(owned)"
            } else {
                r#"owned => new Pattern(state => {
                     rawWhenOwnershipQueryCalls++;
                     return pure(owned).query(state);
                   })"#
            };
            let on = if branch == "true" {
                "{ marker: 'truthy-on' }"
            } else {
                "false"
            };
            let selected_target = if branch == "true" {
                "unselectedTarget"
            } else {
                "returned"
            };
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        globalThis.rawWhenOwnershipHits = 0;
                        globalThis.rawWhenOwnershipQueryCalls = 0;
                        globalThis.rawWhenOwnershipMismatch = 0;
                        let owned = {{
                          marker: 'raw-when-owned', branch: '{branch}', route: '{route}'
                        }};
                        let outer = {{ marker: 'outer' }};
                        let on = {on};
                        let unselectedTarget = {{ marker: 'unselected-target' }};
                        let transient = {{ marker: 'captured-transient' }};
                        let extra = {{ marker: 'ignored-extra' }};
                        const makeResult = {make_result};
                        let returned = makeResult(owned);
                        let callback = function (received) {{
                          'use strict';
                          rawWhenOwnershipHits++;
                          if (this !== undefined
                              || received !== unselectedTarget
                              || transient.marker !== 'captured-transient') {{
                            rawWhenOwnershipMismatch++;
                          }}
                          return returned;
                        }};
                        globalThis.rawWhenOwnedRef = new WeakRef(owned);
                        globalThis.rawWhenDroppedRefs = [
                          new WeakRef(outer), new WeakRef(unselectedTarget),
                          new WeakRef(transient), new WeakRef(extra),
                          new WeakRef(callback),
                          ...(on && typeof on === 'object' ? [new WeakRef(on)] : []),
                        ];
                        const result = Reflect.apply(
                          Pattern.prototype._when, outer,
                          [on, callback, {selected_target}, extra]
                        );
                        globalThis.rawWhenOwnedPattern = result;
                        globalThis.rawWhenExactReturned = Number(result === returned);
                        callback = outer = on = unselectedTarget = transient = extra
                          = returned = owned = null;
                        return result;
                      }})()
                    "#
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| {
                panic!("construct JS-owned raw when {branch}/{route}: {error}")
            });
            assert_eq!(rt.get_number("rawWhenExactReturned"), Some(1.0));
            assert_eq!(
                rt.get_number("rawWhenOwnershipHits"),
                Some(if branch == "true" { 1.0 } else { 0.0 }),
                "{branch}/{route}: callback phase changed"
            );
            assert_eq!(rt.get_number("rawWhenOwnershipMismatch"), Some(0.0));
            assert_eq!(rt.get_number("rawWhenOwnershipQueryCalls"), Some(0.0));
            let active = rt.active_pattern().expect("active JS-owned raw when");
            assert!(
                !active.is_pure() || active.purity().opaque,
                "{branch}/{route}: JS-owned result was granted host-free purity"
            );

            rt.run_gc();
            rt.run_gc();
            rt.run_gc();
            rt.evaluate_score(
                r#"(() => {
                     const state = {
                       span: { begin: 0, end: 1 }, controls: {}
                     };
                     const values = rawWhenOwnedPattern.query(state)
                       .map(hap => hap.value);
                     globalThis.rawWhenReturnedOwnership = Number(
                       values.length === 1
                       && values[0] === rawWhenOwnedRef.deref()
                       && values[0]?.marker === 'raw-when-owned'
                     );
                     globalThis.rawWhenTransientsPruned = Number(
                       rawWhenDroppedRefs.every(
                         ref => ref.deref() === undefined
                       )
                     );
                     return rawWhenOwnedPattern;
                   })()"#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query JS-owned raw when {branch}/{route}: {error}"));
            assert_eq!(rt.get_number("rawWhenReturnedOwnership"), Some(1.0));
            assert_eq!(rt.get_number("rawWhenTransientsPruned"), Some(1.0));
            assert_eq!(
                rt.get_number("rawWhenOwnershipQueryCalls"),
                Some(if route == "query" { 1.0 } else { 0.0 }),
                "{branch}/{route}: returned query callback count changed"
            );
        }
    }
}

/// Raw `_never` selects its explicit target without invoking the ignored
/// callback, while raw `_always` invokes its callback eagerly to select a
/// terminal.  When that terminal is native, neither construction-time
/// callable belongs to the returned graph, which must remain host-free after
/// the constructing QuickJS runtime is gone.
#[test]
fn raw_never_always_native_terminals_remain_host_free_after_runtime_teardown() {
    for (name, source, expected) in [
        (
            "_never",
            r#"(() => {
                 let hits = 0;
                 const ignored = () => {
                   hits++;
                   throw new Error('raw never invoked its ignored callback');
                 };
                 const result = pure('outer')._never(
                   ignored, sequence('n0', 'n1'), { marker: 'extra' }
                 );
                 if (hits !== 0) throw new Error('raw never callback phase changed');
                 return result;
               })()"#,
            ["0/1>1/2:n0", "1/2>1/1:n1"],
        ),
        (
            "_always",
            r#"(() => {
                 let hits = 0;
                 const callback = function (value) {
                   'use strict';
                   hits++;
                   if (this !== undefined) throw new Error('raw always callback receiver changed');
                   return value;
                 };
                 const result = pure('outer')._always(
                   callback, sequence('a0', 'a1'), { marker: 'extra' }
                 );
                 if (hits !== 1) throw new Error('raw always callback phase changed');
                 return result;
               })()"#,
            ["0/1>1/2:a0", "1/2>1/1:a1"],
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct native {name} terminal: {error}"));
            let pattern = rt.active_pattern().expect("active raw terminal");
            assert!(pattern.is_pure(), "{name}: native terminal became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{name}: construction-time callable leaked into the returned graph"
            );
            assert_eq!(pattern.steps, Some(Fraction::int(2)));
            pattern
        };

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{name}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE)
                .into_iter()
                .map(|hap| format!("{}>{}:{}", hap.part.begin, hap.part.end, hap.value.show()))
                .collect::<Vec<_>>(),
            expected,
            "{name}: host-free selected terminal changed"
        );
    }
}

/// Raw `_never` returns its explicit Pattern target exactly; raw `_always`
/// returns its callback's exact Pattern terminal.  The selected Pattern must
/// retain its own JS value or query callback across GC, while ignored inputs,
/// the eager callback and its target, receivers, captured transients, and
/// ignored extras remain construction-only and are pruned.
#[test]
fn raw_never_always_retain_exact_selected_js_ownership_and_prune_transients() {
    for name in ["_never", "_always"] {
        for route in ["value", "query"] {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            let make_result = if route == "value" {
                "owned => pure(owned)"
            } else {
                r#"owned => new Pattern(state => {
                     rawNeverAlwaysOwnershipQueryCalls++;
                     return pure(owned).query(state);
                   })"#
            };
            let invocation = if name == "_never" {
                r#"
                  let callback = function () {
                    rawNeverAlwaysOwnershipHits++;
                    if (transient.marker !== 'captured-transient') {
                      rawNeverAlwaysOwnershipMismatch++;
                    }
                    return returned;
                  };
                  globalThis.rawNeverAlwaysDroppedRefs = [
                    new WeakRef(outer), new WeakRef(callback),
                    new WeakRef(transient), new WeakRef(extra),
                  ];
                  const result = Reflect.apply(
                    Pattern.prototype._never, outer,
                    [callback, returned, extra]
                  );
                  callback = null;
                "#
            } else {
                r#"
                  let target = { marker: 'always-target' };
                  let callback = function (received) {
                    'use strict';
                    rawNeverAlwaysOwnershipHits++;
                    if (this !== undefined
                        || received !== target
                        || transient.marker !== 'captured-transient') {
                      rawNeverAlwaysOwnershipMismatch++;
                    }
                    return returned;
                  };
                  globalThis.rawNeverAlwaysDroppedRefs = [
                    new WeakRef(outer), new WeakRef(callback),
                    new WeakRef(target), new WeakRef(transient),
                    new WeakRef(extra),
                  ];
                  const result = Reflect.apply(
                    Pattern.prototype._always, outer,
                    [callback, target, extra]
                  );
                  callback = target = null;
                "#
            };
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        globalThis.rawNeverAlwaysOwnershipHits = 0;
                        globalThis.rawNeverAlwaysOwnershipMismatch = 0;
                        globalThis.rawNeverAlwaysOwnershipQueryCalls = 0;
                        let owned = {{
                          marker: 'raw-never-always-owned',
                          name: '{name}', route: '{route}'
                        }};
                        const transient = {{ marker: 'captured-transient' }};
                        let outer = {{ marker: 'outer' }};
                        let extra = {{ marker: 'ignored-extra' }};
                        const makeResult = {make_result};
                        let returned = makeResult(owned);
                        {invocation}
                        globalThis.rawNeverAlwaysOwnedRef = new WeakRef(owned);
                        globalThis.rawNeverAlwaysOwnedPattern = result;
                        globalThis.rawNeverAlwaysExactReturned = Number(
                          result === returned
                        );
                        owned = outer = extra = returned = null;
                        return result;
                      }})()
                    "#
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| {
                panic!("construct JS-owned raw {name}/{route} terminal: {error}")
            });
            assert_eq!(rt.get_number("rawNeverAlwaysExactReturned"), Some(1.0));
            assert_eq!(
                rt.get_number("rawNeverAlwaysOwnershipHits"),
                Some(if name == "_always" { 1.0 } else { 0.0 }),
                "{name}/{route}: callback phase changed"
            );
            assert_eq!(rt.get_number("rawNeverAlwaysOwnershipMismatch"), Some(0.0));
            assert_eq!(
                rt.get_number("rawNeverAlwaysOwnershipQueryCalls"),
                Some(0.0)
            );
            let active = rt
                .active_pattern()
                .expect("active JS-owned raw never/always terminal");
            assert!(
                !active.is_pure() || active.purity().opaque,
                "{name}/{route}: JS-owned result was granted host-free purity"
            );

            rt.run_gc();
            rt.run_gc();
            rt.run_gc();
            rt.evaluate_score(
                r#"(() => {
                     const state = {
                       span: { begin: 0, end: 1 }, controls: {}
                     };
                     const values = rawNeverAlwaysOwnedPattern.query(state)
                       .map(hap => hap.value);
                     globalThis.rawNeverAlwaysReturnedOwnership = Number(
                       values.length === 1
                       && values[0] === rawNeverAlwaysOwnedRef.deref()
                       && values[0]?.marker === 'raw-never-always-owned'
                     );
                     globalThis.rawNeverAlwaysTransientsPruned = Number(
                       rawNeverAlwaysDroppedRefs.every(
                         ref => ref.deref() === undefined
                       )
                     );
                     return rawNeverAlwaysOwnedPattern;
                   })()"#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query JS-owned raw {name}/{route} after GC: {error}"));
            assert_eq!(rt.get_number("rawNeverAlwaysReturnedOwnership"), Some(1.0));
            assert_eq!(rt.get_number("rawNeverAlwaysTransientsPruned"), Some(1.0));
            assert_eq!(
                rt.get_number("rawNeverAlwaysOwnershipQueryCalls"),
                Some(if route == "query" { 1.0 } else { 0.0 }),
                "{name}/{route}: returned query callback count changed"
            );
        }
    }
}

/// Public `swingBy`/`swing` and raw `_swing` build a wholly native graph for
/// scalar subdivisions.  The raw shorthand must therefore shed its QuickJS
/// construction heap, while the repaired public body keeps source steps for
/// nonzero subdivisions and keeps the constructed silence metadata for zero.
#[test]
fn raw_swing_native_scalar_graphs_remain_host_free_after_runtime_teardown() {
    const FOUR: &[&str] = &[
        "0/1>1/8:a",
        "1/8>1/4:a",
        "1/4>3/8:b",
        "3/8>1/2:b",
        "1/2>5/8:c",
        "5/8>3/4:c",
        "3/4>7/8:d",
        "7/8>1/1:d",
    ];
    const ONE: &[&str] = &[
        "0/1>1/8:x",
        "1/8>1/4:x",
        "1/4>3/8:x",
        "3/8>1/2:x",
        "1/2>5/8:x",
        "5/8>3/4:x",
        "3/4>7/8:x",
        "7/8>1/1:x",
    ];
    for (name, source, expected_steps, expected) in [
        (
            "swingBy",
            "sequence('a','b','c','d').setSteps(7).swingBy(1/3,4)",
            Some(Fraction::int(7)),
            FOUR,
        ),
        (
            "swing",
            "sequence('a','b','c','d').setSteps(7).swing(4)",
            Some(Fraction::int(7)),
            FOUR,
        ),
        (
            "_swing",
            "sequence('a','b','c','d').setSteps(7)._swing(4)",
            Some(Fraction::int(7)),
            FOUR,
        ),
        (
            "_swing-zero",
            "sequence('a','b').setSteps(7)._swing(0)",
            Some(Fraction::ONE),
            &[],
        ),
        (
            "_swing-zero-source-steps",
            "pure('x').setSteps(0)._swing(4)",
            Some(Fraction::ZERO),
            ONE,
        ),
        (
            "_swing-no-source-steps",
            "pure('x').setSteps(undefined)._swing(4)",
            None,
            ONE,
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct {name}: {error}"));
            let pattern = rt.active_pattern().expect("active scalar swing graph");
            assert!(
                pattern.is_pure(),
                "{name}: native swing graph became impure"
            );
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{name}: native swing graph retained a callback"
            );
            assert_eq!(
                pattern.steps, expected_steps,
                "{name}: scalar swing step metadata changed"
            );
            pattern
        };

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{name}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE)
                .into_iter()
                .map(|hap| format!("{}>{}:{}", hap.part.begin, hap.part.end, hap.value.show()))
                .collect::<Vec<_>>(),
            expected,
            "{name}: host-free scalar swing haps changed"
        );
    }
}

/// The raw body dynamically gets and calls the target's current `swingBy` and
/// returns its exact terminal.  A JavaScript Pattern terminal must keep its
/// own value/query root across collections, while the receiver, scalar,
/// target, method, and ignored extra remain construction-only and are pruned.
#[test]
fn raw_swing_retains_exact_custom_terminal_ownership_and_prunes_dispatch_transients() {
    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawSwingOwnershipQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawSwingOwnershipQueryCalls = 0;
                    globalThis.rawSwingDispatchLog = [];
                    let owned = {{ marker: 'raw-swing-owned', route: '{route}' }};
                    let outer = {{ marker: 'outer' }};
                    let n = {{ marker: 'n' }};
                    let target = {{ marker: 'target' }};
                    let extra = {{ marker: 'ignored-extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (ratio, value) {{
                      'use strict';
                      rawSwingDispatchLog.push(
                        `call:${{this === target}}:${{arguments.length}}:${{ratio}}:${{value === n}}`
                      );
                      return returned;
                    }};
                    Object.defineProperty(target, 'swingBy', {{
                      configurable: true,
                      get() {{
                        rawSwingDispatchLog.push('get');
                        return method;
                      }},
                    }});
                    globalThis.rawSwingOwnedRef = new WeakRef(owned);
                    globalThis.rawSwingDroppedRefs = [
                      new WeakRef(outer), new WeakRef(n), new WeakRef(target),
                      new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._swing, outer, [n, target, extra]
                    );
                    globalThis.rawSwingExactReturned = Number(result === returned);
                    globalThis.rawSwingOwnedPattern = result;
                    Object.defineProperty(target, 'swingBy', {{
                      configurable: true,
                      value() {{ throw new Error('query reread swingBy'); }},
                    }});
                    globalThis.rawSwingDispatchLogSnapshot =
                      JSON.stringify(rawSwingDispatchLog);
                    owned = outer = n = target = extra = method = returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned raw swing {route}: {error}"));
        assert_eq!(rt.get_number("rawSwingExactReturned"), Some(1.0));
        assert_eq!(
            rt.get_string("rawSwingDispatchLogSnapshot").as_deref(),
            Some(r#"["get","call:true:2:0.3333333333333333:true"]"#),
            "{route}: raw swing dispatch order changed"
        );
        assert_eq!(rt.get_number("rawSwingOwnershipQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active JS-owned raw swing");
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned raw swing terminal was granted host-free purity"
        );

        for turn in 1..=2 {
            rt.run_gc();
            rt.run_gc();
            rt.run_gc();
            rt.evaluate_score(
                r#"(() => {
                     const state = {
                       span: { begin: 0, end: 1 }, controls: {}
                     };
                     const values = rawSwingOwnedPattern.query(state)
                       .map(hap => hap.value);
                     globalThis.rawSwingReturnedOwnership = Number(
                       values.length === 1
                       && values[0] === rawSwingOwnedRef.deref()
                       && values[0]?.marker === 'raw-swing-owned'
                     );
                     globalThis.rawSwingTransientsPruned = Number(
                       rawSwingDroppedRefs.every(ref => ref.deref() === undefined)
                     );
                     return rawSwingOwnedPattern;
                   })()"#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query JS-owned raw swing {route}: {error}"));
            assert_eq!(rt.get_number("rawSwingReturnedOwnership"), Some(1.0));
            assert_eq!(rt.get_number("rawSwingTransientsPruned"), Some(1.0));
            assert_eq!(
                rt.get_number("rawSwingOwnershipQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                }),
                "{route}: returned query callback count changed"
            );
            assert_eq!(
                rt.get_string("rawSwingDispatchLogSnapshot").as_deref(),
                Some(r#"["get","call:true:2:0.3333333333333333:true"]"#),
                "{route}: query reread the dynamic swingBy chain"
            );
        }
    }
}

/// Canonical tagged transformers take `sometimesBy`'s pure dispatch.  The raw
/// quartet must therefore leave a genuinely host-free graph that survives the
/// constructing QuickJS runtime.  A semantically equivalent ordinary
/// JavaScript wrapper remains a reachable query-time callback instead.
#[test]
fn raw_signal_quartet_tagged_transformers_remain_host_free_after_runtime_teardown() {
    for (raw_name, expected) in [
        ("_often", ["c", "b", "a", "c", "b", "a"]),
        ("_rarely", ["b", "c", "a", "a", "c", "b"]),
        ("_almostNever", ["b", "c", "a", "a", "c", "b"]),
        ("_almostAlways", ["c", "b", "a", "c", "b", "a"]),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(
                &format!(
                    r#"(() => {{
                         const source = sequence('a','b','c').setSteps(7);
                         source.__pure_loc = {{ start: 3, end: 5 }};
                         const result = source.{raw_name}(rev);
                         globalThis.rawSignalTaggedShape = Number(
                           Object.hasOwn(source, '__pure_loc')
                           && !Object.hasOwn(result, '__pure')
                           && !Object.hasOwn(result, '__pure_loc')
                           && result._steps === undefined
                           && result !== source
                           && result.query !== source.query
                         );
                         return result;
                       }})()"#
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("construct tagged {raw_name}: {error}"));
            assert_eq!(rt.get_number("rawSignalTaggedShape"), Some(1.0));

            let pattern = rt.active_pattern().expect("active raw signal graph");
            assert!(
                pattern.is_pure() && !pattern.purity().opaque,
                "{raw_name}: canonical tagged transformer lost pure dispatch"
            );
            assert_eq!(
                pattern.steps, None,
                "{raw_name}: outer raw signal graph retained source steps"
            );
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{raw_name}: tagged native transformer became a JS callback"
            );
            pattern
        };

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{raw_name}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.pattern()
                .query_arc_sorted(Fraction::ZERO, Fraction::int(2))
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            expected,
            "{raw_name}: host-free tagged RNG route changed"
        );

        let rt = JsRuntime::new().expect("wrapper runtime");
        rt.install_semantic_bindings().expect("wrapper bindings");
        rt.evaluate_score(
            &format!(
                r#"(() => {{
                     const wrapper = x => x.rev();
                     delete wrapper.__pure;
                     delete wrapper.__pure_loc;
                     return sequence('a','b','c').setSteps(7).{raw_name}(wrapper);
                   }})()"#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS wrapper {raw_name}: {error}"));
        let active = rt.active_pattern().expect("active JS wrapper graph");
        assert!(
            !active.is_pure(),
            "{raw_name}: ordinary JS wrapper was misclassified as host-free"
        );
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "{raw_name}: ordinary JS wrapper lost its host-owned callback lineage"
        );
        let query = || {
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::int(2))
                .unwrap_or_else(|error| panic!("query JS wrapper {raw_name}: {error}"))
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            query(),
            expected,
            "{raw_name}: JS wrapper RNG route changed"
        );
        for _ in 0..3 {
            rt.run_gc();
        }
        assert_eq!(
            query(),
            expected,
            "{raw_name}: JS wrapper callback was lost across QuickJS GC"
        );
    }
}

/// The ordinary callback is retained for query-time use together with the
/// source's exact JS-owned value, its captured owner, and its returned Pattern
/// and value.  The ignored outer receiver, explicit extra, and original source
/// wrapper are forwarding-only and must be collectible.
#[test]
fn raw_signal_quartet_retain_query_ownership_and_prune_forwarding_transients() {
    for (raw_name, expected_haps) in [
        ("_often", 19.0),
        ("_rarely", 24.0),
        ("_almostNever", 29.0),
        ("_almostAlways", 18.0),
    ] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"(() => {{
                     globalThis.rawSignalOwnedCalls = 0;
                     let sourceOwned = {{ marker: '{raw_name}-source' }};
                     let callbackOwned = {{ marker: '{raw_name}-callback' }};
                     let resultOwned = {{ marker: '{raw_name}-result' }};
                     let outer = {{ marker: '{raw_name}-outer' }};
                     let extra = {{ marker: '{raw_name}-extra' }};
                     let source = pure(sourceOwned).setSteps(7);
                     let returned = pure(resultOwned);
                     const makeCallback = (owner, returnedPattern) =>
                       function (_pat) {{
                         'use strict';
                         if (owner.marker !== '{raw_name}-callback') {{
                           throw new Error('callback owner changed');
                         }}
                         rawSignalOwnedCalls++;
                         return returnedPattern;
                       }};
                     let callback = makeCallback(callbackOwned, returned);
                     globalThis.rawSignalSourceOwnedRef = new WeakRef(sourceOwned);
                     globalThis.rawSignalCallbackOwnedRef = new WeakRef(callbackOwned);
                     globalThis.rawSignalResultOwnedRef = new WeakRef(resultOwned);
                     globalThis.rawSignalCallbackRef = new WeakRef(callback);
                     globalThis.rawSignalReturnedPatternRef = new WeakRef(returned);
                     globalThis.rawSignalDroppedRefs = [
                       new WeakRef(outer), new WeakRef(extra), new WeakRef(source),
                     ];
                     const result = Reflect.apply(
                       Pattern.prototype.{raw_name}, outer,
                       [callback, source, extra]
                     );
                     globalThis.rawSignalOwnedPattern = result;
                     sourceOwned = callbackOwned = resultOwned = null;
                     outer = extra = source = callback = returned = null;
                     return result;
                   }})()"#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct owned {raw_name}: {error}"));
        assert_eq!(rt.get_number("rawSignalOwnedCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active owned raw signal");
        assert!(
            !active.is_pure() && active.purity().opaque,
            "{raw_name}: JS callback graph was granted purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"(() => {
                     const state = {
                       span: { begin: 0, end: 16 }, controls: {}
                     };
                     const haps = rawSignalOwnedPattern.query(state);
                     const values = haps.map(hap => hap.value);
                     const sourceOwned = rawSignalSourceOwnedRef.deref();
                     const callbackOwned = rawSignalCallbackOwnedRef.deref();
                     const resultOwned = rawSignalResultOwnedRef.deref();
                     globalThis.rawSignalOwnedHapCount = haps.length;
                     globalThis.rawSignalSourceOwnership = Number(
                       sourceOwned !== undefined
                       && values.some(value => value === sourceOwned)
                       && sourceOwned.marker.endsWith('-source')
                     );
                     globalThis.rawSignalCallbackOwnership = Number(
                       callbackOwned !== undefined
                       && callbackOwned.marker.endsWith('-callback')
                     );
                     globalThis.rawSignalResultOwnership = Number(
                       resultOwned !== undefined
                       && values.some(value => value === resultOwned)
                       && resultOwned.marker.endsWith('-result')
                     );
                     globalThis.rawSignalFunctionOwnership = Number(
                       rawSignalCallbackRef.deref() !== undefined
                     );
                     globalThis.rawSignalReturnedPatternOwnership = Number(
                       rawSignalReturnedPatternRef.deref() !== undefined
                     );
                     globalThis.rawSignalForwardingTransientsPruned = Number(
                       rawSignalDroppedRefs.every(ref => ref.deref() === undefined)
                     );
                     return rawSignalOwnedPattern;
                   })()"#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query owned {raw_name} after GC: {error}"));
            assert_eq!(
                rt.get_number("rawSignalOwnedHapCount"),
                Some(expected_haps),
                "{raw_name}: returned Pattern join density changed"
            );
            let ownership = [
                rt.get_number("rawSignalSourceOwnership"),
                rt.get_number("rawSignalCallbackOwnership"),
                rt.get_number("rawSignalResultOwnership"),
                rt.get_number("rawSignalFunctionOwnership"),
                rt.get_number("rawSignalReturnedPatternOwnership"),
            ];
            assert_eq!(
                ownership,
                [Some(1.0); 5],
                "{raw_name}: split ownership flags are source/callback/result/function/pattern"
            );
            assert_eq!(
                rt.get_number("rawSignalSourceOwnership"),
                Some(1.0),
                "{raw_name}: source value identity was lost"
            );
            assert_eq!(
                rt.get_number("rawSignalCallbackOwnership"),
                Some(1.0),
                "{raw_name}: callback-captured value identity was lost"
            );
            assert_eq!(
                rt.get_number("rawSignalResultOwnership"),
                Some(1.0),
                "{raw_name}: returned value identity was lost"
            );
            assert_eq!(
                rt.get_number("rawSignalFunctionOwnership"),
                Some(1.0),
                "{raw_name}: query-time callback function was collected"
            );
            assert_eq!(
                rt.get_number("rawSignalForwardingTransientsPruned"),
                Some(1.0),
                "{raw_name}: ignored forwarding inputs remained rooted"
            );
            assert_eq!(
                rt.get_number("rawSignalOwnedCalls"),
                Some(f64::from(turn * 16)),
                "{raw_name}: callback was not exactly once per queried carrier"
            );
        }
    }
}

/// Canonical `_set` uses the receiver's `fmap` graph, so its generated lexical
/// callback remains query-time host work.  That graph must retain the exact
/// mapped JS value and callback, as well as the source's otherwise-hidden JS
/// value, while construction-only wrapper/method objects remain collectible.
#[test]
fn raw_set_canonical_fmap_retains_exact_js_owners_and_prunes_dispatch_objects() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");
    rt.evaluate_score(
        r#"
          (() => {
            globalThis.rawSetFmapCalls = 0;
            globalThis.rawSetFmapGets = 0;
            let sourceOwned = { marker: 'raw-set-source-owned' };
            let valueOwned = { marker: 'raw-set-value-owned' };
            let source = pure(sourceOwned).setSteps(7);
            const canonicalFmap = source.fmap;
            let method = function (callback) {
              'use strict';
              rawSetFmapCalls++;
              if (this !== source || arguments.length !== 1) {
                throw new Error('raw set fmap receiver/arity changed');
              }
              globalThis.rawSetCanonicalCallbackRef = new WeakRef(callback);
              return Reflect.apply(canonicalFmap, this, [callback]);
            };
            Object.defineProperty(source, 'fmap', {
              configurable: true,
              get() {
                rawSetFmapGets++;
                return method;
              },
            });
            globalThis.rawSetSourceOwnedRef = new WeakRef(sourceOwned);
            globalThis.rawSetValueOwnedRef = new WeakRef(valueOwned);
            globalThis.rawSetCanonicalDroppedRefs = [
              new WeakRef(source), new WeakRef(method),
            ];
            const result = source._set(valueOwned);
            globalThis.rawSetCanonicalShape = Number(
              result !== source
              && result.query !== source.query
              && result._steps?.show() === '7/1'
              && !Object.hasOwn(result, '__pure')
              && !Object.hasOwn(result, '__pure_loc')
            );
            globalThis.rawSetCanonicalPattern = result;
            sourceOwned = valueOwned = source = method = null;
            return result;
          })()
        "#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct canonical JS-owned raw set");
    assert_eq!(rt.get_number("rawSetFmapGets"), Some(1.0));
    assert_eq!(rt.get_number("rawSetFmapCalls"), Some(1.0));
    assert_eq!(rt.get_number("rawSetCanonicalShape"), Some(1.0));
    let active = rt.active_pattern().expect("active canonical raw set");
    assert_eq!(active.steps, Some(Fraction::int(7)));
    assert!(
        !active.is_pure() && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
        "raw set's generated JS fmap callback was granted host-free purity"
    );

    for turn in 1..=2 {
        for _ in 0..3 {
            rt.run_gc();
        }
        rt.evaluate_score(
            r#"
              (() => {
                const state = {
                  span: { begin: 0, end: 1 }, controls: {}
                };
                const haps = rawSetCanonicalPattern.query(state);
                const sourceOwned = rawSetSourceOwnedRef.deref();
                const valueOwned = rawSetValueOwnedRef.deref();
                globalThis.rawSetCanonicalHapCount = haps.length;
                globalThis.rawSetCanonicalSourceOwnership = Number(
                  sourceOwned?.marker === 'raw-set-source-owned'
                );
                globalThis.rawSetCanonicalValueOwnership = Number(
                  valueOwned?.marker === 'raw-set-value-owned'
                );
                globalThis.rawSetCanonicalExactValue = Number(
                  haps[0]?.value === valueOwned
                );
                globalThis.rawSetCanonicalCallbackOwnership = Number(
                  rawSetCanonicalCallbackRef.deref() !== undefined
                );
                globalThis.rawSetCanonicalPruning = Number(
                  rawSetCanonicalDroppedRefs.every(
                    reference => reference.deref() === undefined
                  )
                );
                return rawSetCanonicalPattern;
              })()
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("query canonical raw set after GC: {error}"));
        assert_eq!(rt.get_number("rawSetCanonicalHapCount"), Some(1.0));
        for field in [
            "rawSetCanonicalSourceOwnership",
            "rawSetCanonicalValueOwnership",
            "rawSetCanonicalExactValue",
            "rawSetCanonicalCallbackOwnership",
        ] {
            assert_eq!(
                rt.get_number(field),
                Some(1.0),
                "turn {turn}: raw set lost canonical owner {field}"
            );
        }
        assert_eq!(
            rt.get_number("rawSetCanonicalPruning"),
            Some(1.0),
            "turn {turn}: raw set retained construction-only wrapper/method"
        );
        assert_eq!(rt.get_number("rawSetFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawSetFmapCalls"), Some(1.0));
    }
}

/// A custom receiver controls the entire `fmap` handoff. `_set` must return
/// that method's exact terminal without merging its receiver, mapped value,
/// callback input, callback, or ignored extras into the selected Pattern's
/// ownership sidecar.
#[test]
fn raw_set_custom_fmap_returns_exact_js_terminal_and_prunes_forwarding_inputs() {
    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawSetCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawSetCustomGets = 0;
                    globalThis.rawSetCustomCalls = 0;
                    globalThis.rawSetCustomQueryCalls = 0;
                    let owned = {{ marker: 'raw-set-terminal-owned', route: '{route}' }};
                    let outer = {{ marker: 'outer' }};
                    let value = {{ marker: 'mapped value' }};
                    let extra = {{ marker: 'ignored extra' }};
                    let input = {{ marker: 'callback input' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawSetCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawSetCustomCallbackRef = new WeakRef(callback);
                      if (Reflect.apply(callback, {{ ignored: true }}, [input, extra]) !== value) {{
                        throw new Error('raw set callback changed captured value');
                      }}
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{
                        rawSetCustomGets++;
                        return method;
                      }},
                    }});
                    globalThis.rawSetCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawSetCustomDroppedRefs = [
                      new WeakRef(outer), new WeakRef(value), new WeakRef(extra),
                      new WeakRef(input), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._set, outer, [value, extra]
                    );
                    globalThis.rawSetCustomExactReturned = Number(result === returned);
                    globalThis.rawSetCustomPattern = result;
                    owned = outer = value = extra = input = method = returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw set {route}: {error}"));
        assert_eq!(rt.get_number("rawSetCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawSetCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawSetCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawSetCustomQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom-fmap raw set");
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom terminal was granted host-free purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawSetCustomPattern.query(state);
                    const owned = rawSetCustomOwnedRef.deref();
                    globalThis.rawSetCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-set-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawSetCustomPruning = Number(
                      rawSetCustomCallbackRef.deref() === undefined
                      && rawSetCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawSetCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom-fmap raw set {route}: {error}"));
            assert_eq!(
                rt.get_number("rawSetCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawSetCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: custom fmap forwarding inputs stayed rooted"
            );
            assert_eq!(rt.get_number("rawSetCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawSetCustomCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawSetCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Canonical `_keep` is an identity map, not a host-free identity shortcut.
/// Its generated lexical mapper must retain both the exact source owners and
/// the semantically ignored first-argument Pattern (including that Pattern's
/// query callback), while never querying the ignored graph. Only later extras
/// and construction-only dispatch wrappers may be pruned.
#[test]
fn raw_keep_canonical_fmap_retains_source_and_ignored_pattern_without_querying_it() {
    let rt = JsRuntime::new().expect("runtime");
    rt.install_semantic_bindings().expect("bindings");
    rt.evaluate_score(
        r#"
          (() => {
            globalThis.rawKeepFmapGets = 0;
            globalThis.rawKeepFmapCalls = 0;
            globalThis.rawKeepIgnoredQueryCalls = 0;
            let sourceOwned = { marker: 'raw-keep-source-owned' };
            let ignoredOwned = { marker: 'raw-keep-ignored-owned' };
            const ignoredOwnedCapture = ignoredOwned;
            let ignoredQuery = function (state) {
              rawKeepIgnoredQueryCalls++;
              return pure(ignoredOwnedCapture).query(state);
            };
            let ignoredPattern = new Pattern(ignoredQuery);
            let extra = { marker: 'raw-keep-extra' };
            let source = pure(sourceOwned).setSteps(7);
            const canonicalFmap = source.fmap;
            let method = function (callback) {
              'use strict';
              rawKeepFmapCalls++;
              if (this !== source || arguments.length !== 1) {
                throw new Error('raw keep fmap receiver/arity changed');
              }
              globalThis.rawKeepCanonicalCallbackRef = new WeakRef(callback);
              return Reflect.apply(canonicalFmap, this, [callback]);
            };
            Object.defineProperty(source, 'fmap', {
              configurable: true,
              get() {
                rawKeepFmapGets++;
                return method;
              },
            });
            globalThis.rawKeepSourceOwnedRef = new WeakRef(sourceOwned);
            globalThis.rawKeepIgnoredOwnedRef = new WeakRef(ignoredOwned);
            globalThis.rawKeepIgnoredPatternRef = new WeakRef(ignoredPattern);
            globalThis.rawKeepIgnoredQueryRef = new WeakRef(ignoredQuery);
            globalThis.rawKeepCanonicalDroppedRefs = [
              new WeakRef(source), new WeakRef(method), new WeakRef(extra),
            ];
            const result = Reflect.apply(
              Pattern.prototype._keep, source, [ignoredPattern, extra]
            );
            globalThis.rawKeepCanonicalShape = Number(
              result !== source
              && result.query !== source.query
              && result._steps?.show() === '7/1'
              && !Object.hasOwn(result, '__pure')
              && !Object.hasOwn(result, '__pure_loc')
            );
            globalThis.rawKeepCanonicalPattern = result;
            sourceOwned = ignoredOwned = ignoredPattern = ignoredQuery =
              extra = source = method = null;
            return result;
          })()
        "#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("construct canonical JS-owned raw keep");
    assert_eq!(rt.get_number("rawKeepFmapGets"), Some(1.0));
    assert_eq!(rt.get_number("rawKeepFmapCalls"), Some(1.0));
    assert_eq!(rt.get_number("rawKeepCanonicalShape"), Some(1.0));
    assert_eq!(rt.get_number("rawKeepIgnoredQueryCalls"), Some(0.0));
    let active = rt.active_pattern().expect("active canonical raw keep");
    assert_eq!(active.steps, Some(Fraction::int(7)));
    assert!(
        !active.is_pure() && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
        "raw keep's generated JS fmap callback was granted host-free purity"
    );

    for turn in 1..=2 {
        for _ in 0..3 {
            rt.run_gc();
        }
        rt.evaluate_score(
            r#"
              (() => {
                const state = {
                  span: { begin: 0, end: 1 }, controls: {}
                };
                const haps = rawKeepCanonicalPattern.query(state);
                const sourceOwned = rawKeepSourceOwnedRef.deref();
                globalThis.rawKeepCanonicalHapCount = haps.length;
                globalThis.rawKeepCanonicalSourceOwnership = Number(
                  sourceOwned?.marker === 'raw-keep-source-owned'
                  && haps[0]?.value === sourceOwned
                );
                globalThis.rawKeepCanonicalIgnoredOwnership = Number(
                  rawKeepIgnoredOwnedRef.deref()?.marker
                    === 'raw-keep-ignored-owned'
                  && rawKeepIgnoredPatternRef.deref() !== undefined
                  && rawKeepIgnoredQueryRef.deref() !== undefined
                );
                globalThis.rawKeepCanonicalCallbackOwnership = Number(
                  rawKeepCanonicalCallbackRef.deref() !== undefined
                );
                globalThis.rawKeepCanonicalPruning = Number(
                  rawKeepCanonicalDroppedRefs.every(
                    reference => reference.deref() === undefined
                  )
                );
                return rawKeepCanonicalPattern;
              })()
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("query canonical raw keep after GC: {error}"));
        assert_eq!(rt.get_number("rawKeepCanonicalHapCount"), Some(1.0));
        for field in [
            "rawKeepCanonicalSourceOwnership",
            "rawKeepCanonicalIgnoredOwnership",
            "rawKeepCanonicalCallbackOwnership",
            "rawKeepCanonicalPruning",
        ] {
            assert_eq!(
                rt.get_number(field),
                Some(1.0),
                "turn {turn}: raw keep owner/pruning discriminator {field} failed"
            );
        }
        assert_eq!(
            rt.get_number("rawKeepIgnoredQueryCalls"),
            Some(0.0),
            "turn {turn}: raw keep queried the captured ignored Pattern"
        );
        assert_eq!(rt.get_number("rawKeepFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepFmapCalls"), Some(1.0));
    }
}

/// A custom receiver controls the complete `fmap` handoff. When it returns a
/// stable native Pattern without retaining the mapper, `_keep` must be
/// host-free after teardown. A JavaScript-owned returned Pattern must retain
/// only its own value/query owners while the ignored value, generated mapper,
/// receiver, method, and extras become collectible.
#[test]
fn raw_keep_custom_fmap_returns_exact_terminal_and_prunes_unused_mapper_capture() {
    let pattern = {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            r#"Reflect.apply(
                 Pattern.prototype._keep,
                 { fmap() { return sequence('a', 'b'); } },
                 [{ marker: 'ignored' }, { marker: 'extra' }]
               )"#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("construct host-free custom-fmap raw keep");
        let pattern = rt.active_pattern().expect("active host-free raw keep");
        assert!(pattern.is_pure(), "stable native terminal became impure");
        assert!(
            pattern.reachable_callbacks().is_empty(),
            "unused raw keep mapper leaked into stable native terminal"
        );
        pattern
    };
    let pure = pattern
        .as_pure_pattern()
        .expect("custom-fmap native terminal must upgrade after teardown");
    assert_eq!(
        pure.query_arc(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "host-free custom-fmap raw keep terminal changed"
    );

    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawKeepCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawKeepCustomGets = 0;
                    globalThis.rawKeepCustomCalls = 0;
                    globalThis.rawKeepCustomQueryCalls = 0;
                    let owned = {{ marker: 'raw-keep-terminal-owned', route: '{route}' }};
                    let outer = {{ marker: 'outer' }};
                    let ignored = {{ marker: 'captured ignored value' }};
                    let extra = {{ marker: 'ignored extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawKeepCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawKeepCustomCallbackRef = new WeakRef(callback);
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{
                        rawKeepCustomGets++;
                        return method;
                      }},
                    }});
                    globalThis.rawKeepCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawKeepCustomDroppedRefs = [
                      new WeakRef(outer), new WeakRef(ignored),
                      new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._keep, outer, [ignored, extra]
                    );
                    globalThis.rawKeepCustomExactReturned = Number(result === returned);
                    globalThis.rawKeepCustomPattern = result;
                    owned = outer = ignored = extra = method = returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw keep {route}: {error}"));
        assert_eq!(rt.get_number("rawKeepCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepCustomQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom-fmap raw keep");
        assert!(
            rt.active_needs_host(),
            "{route}: JS-owned custom terminal lost its required host"
        );
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom terminal was granted false purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawKeepCustomPattern.query(state);
                    const owned = rawKeepCustomOwnedRef.deref();
                    globalThis.rawKeepCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-keep-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawKeepCustomPruning = Number(
                      rawKeepCustomCallbackRef.deref() === undefined
                      && rawKeepCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawKeepCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom-fmap raw keep {route}: {error}"));
            assert_eq!(
                rt.get_number("rawKeepCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawKeepCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: unused mapper capture stayed rooted"
            );
            assert_eq!(rt.get_number("rawKeepCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawKeepCustomCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawKeepCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Canonical `_keepif` always installs a fresh JavaScript mapper. A Pattern
/// used as the truthy condition is retained but never queried; on the false
/// route the same ownership boundary is exercised by a Pattern nested in the
/// source value because no Pattern condition can be falsey in JavaScript.
/// Construction-only dispatch objects and the ignored extra Pattern must
/// remain collectible on both routes.
#[test]
fn raw_keepif_canonical_fmap_retains_unqueried_pattern_owners_on_both_branches() {
    for truthy in [false, true] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawKeepifFmapGets = 0;
                    globalThis.rawKeepifFmapCalls = 0;
                    globalThis.rawKeepifRetainedQueryCalls = 0;
                    globalThis.rawKeepifExtraQueryCalls = 0;
                    let retainedOwned = {{ marker: 'raw-keepif-retained-owned' }};
                    const retainedOwnedCapture = retainedOwned;
                    let retainedQuery = function (state) {{
                      rawKeepifRetainedQueryCalls++;
                      return pure(retainedOwnedCapture).query(state);
                    }};
                    let retainedPattern = new Pattern(retainedQuery);
                    let sourceOwned = {{
                      marker: 'raw-keepif-source-owned',
                      nested: {nested_pattern},
                    }};
                    let source = pure(sourceOwned).setSteps(7);
                    const canonicalFmap = source.fmap;
                    let method = function (callback) {{
                      'use strict';
                      rawKeepifFmapCalls++;
                      if (this !== source || arguments.length !== 1) {{
                        throw new Error('raw keepif fmap receiver/arity changed');
                      }}
                      globalThis.rawKeepifCanonicalCallbackRef = new WeakRef(callback);
                      return Reflect.apply(canonicalFmap, this, [callback]);
                    }};
                    Object.defineProperty(source, 'fmap', {{
                      configurable: true,
                      get() {{
                        rawKeepifFmapGets++;
                        return method;
                      }},
                    }});
                    let extraOwned = {{ marker: 'raw-keepif-extra-owned' }};
                    const extraOwnedCapture = extraOwned;
                    let extraQuery = function (state) {{
                      rawKeepifExtraQueryCalls++;
                      return pure(extraOwnedCapture).query(state);
                    }};
                    let extraPattern = new Pattern(extraQuery);
                    globalThis.rawKeepifSourceOwnedRef = new WeakRef(sourceOwned);
                    globalThis.rawKeepifRetainedOwnedRef = new WeakRef(retainedOwned);
                    globalThis.rawKeepifRetainedPatternRef = new WeakRef(retainedPattern);
                    globalThis.rawKeepifRetainedQueryRef = new WeakRef(retainedQuery);
                    globalThis.rawKeepifDroppedRefs = [
                      new WeakRef(source), new WeakRef(method),
                      new WeakRef(extraOwned), new WeakRef(extraPattern),
                      new WeakRef(extraQuery),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._keepif,
                      source,
                      [{condition}, extraPattern]
                    );
                    globalThis.rawKeepifCanonicalShape = Number(
                      result !== source
                      && result.query !== source.query
                      && result._steps?.show() === '7/1'
                      && !Object.hasOwn(result, '__pure')
                      && !Object.hasOwn(result, '__pure_loc')
                    );
                    globalThis.rawKeepifCanonicalPattern = result;
                    retainedOwned = retainedPattern = retainedQuery = sourceOwned =
                      source = method = extraOwned = extraPattern = extraQuery = null;
                    return result;
                  }})()
                "#,
                nested_pattern = if truthy { "null" } else { "retainedPattern" },
                condition = if truthy { "retainedPattern" } else { "false" },
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct canonical raw keepif {truthy}: {error}"));
        assert_eq!(rt.get_number("rawKeepifFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepifFmapCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepifCanonicalShape"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepifRetainedQueryCalls"), Some(0.0));
        assert_eq!(rt.get_number("rawKeepifExtraQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active canonical raw keepif");
        assert_eq!(active.steps, Some(Fraction::int(7)));
        assert!(
            rt.active_needs_host(),
            "{truthy}: canonical raw keepif lost its generated mapper host"
        );
        assert!(
            !active.is_pure()
                && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
            "{truthy}: canonical raw keepif mapper was granted host-free purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        const state = {{
                          span: {{ begin: 0, end: 1 }}, controls: {{}}
                        }};
                        const haps = rawKeepifCanonicalPattern.query(state);
                        const sourceOwned = rawKeepifSourceOwnedRef.deref();
                        globalThis.rawKeepifCanonicalHapCount = haps.length;
                        globalThis.rawKeepifCanonicalValue = Number(
                          {value_check}
                        );
                        globalThis.rawKeepifCanonicalOwners = Number(
                          sourceOwned?.marker === 'raw-keepif-source-owned'
                          && rawKeepifRetainedOwnedRef.deref()?.marker
                            === 'raw-keepif-retained-owned'
                          && rawKeepifRetainedPatternRef.deref() !== undefined
                          && rawKeepifRetainedQueryRef.deref() !== undefined
                          && rawKeepifCanonicalCallbackRef.deref() !== undefined
                        );
                        globalThis.rawKeepifCanonicalPruning = Number(
                          rawKeepifDroppedRefs.every(
                            reference => reference.deref() === undefined
                          )
                        );
                        return rawKeepifCanonicalPattern;
                      }})()
                    "#,
                    value_check = if truthy {
                        "haps[0]?.value === sourceOwned"
                    } else {
                        "haps[0]?.value === undefined && Object.hasOwn(haps[0] ?? {}, 'value')"
                    },
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query canonical raw keepif {truthy}: {error}"));
            assert_eq!(rt.get_number("rawKeepifCanonicalHapCount"), Some(1.0));
            for field in [
                "rawKeepifCanonicalValue",
                "rawKeepifCanonicalOwners",
                "rawKeepifCanonicalPruning",
            ] {
                assert_eq!(
                    rt.get_number(field),
                    Some(1.0),
                    "branch {truthy} turn {turn}: raw keepif discriminator {field} failed"
                );
            }
            assert_eq!(rt.get_number("rawKeepifRetainedQueryCalls"), Some(0.0));
            assert_eq!(rt.get_number("rawKeepifExtraQueryCalls"), Some(0.0));
            assert_eq!(rt.get_number("rawKeepifFmapGets"), Some(1.0));
            assert_eq!(rt.get_number("rawKeepifFmapCalls"), Some(1.0));
        }
    }
}

/// A custom receiver owns `_keepif`'s complete `fmap` handoff. A stable native
/// terminal can therefore outlive QuickJS without a host, whereas JavaScript
/// value/query terminals retain only their own owners and prune the unused
/// generated mapper, its Pattern condition, the receiver, method, and extras.
#[test]
fn raw_keepif_custom_fmap_returns_exact_terminal_and_prunes_unused_condition() {
    let pattern = {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            r#"Reflect.apply(
                 Pattern.prototype._keepif,
                 { fmap() { return sequence('a', 'b'); } },
                 [new Pattern(() => { throw new Error('unused condition'); }),
                  { marker: 'extra' }]
               )"#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("construct host-free custom-fmap raw keepif");
        let pattern = rt
            .active_pattern()
            .expect("active host-free custom raw keepif");
        assert!(pattern.is_pure(), "stable native terminal became impure");
        assert!(
            pattern.reachable_callbacks().is_empty(),
            "unused raw keepif mapper leaked into stable native terminal"
        );
        pattern
    };
    let pure = pattern
        .as_pure_pattern()
        .expect("custom-fmap raw keepif native terminal must upgrade after teardown");
    assert_eq!(
        pure.query_arc(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "host-free custom-fmap raw keepif terminal changed"
    );

    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawKeepifCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawKeepifCustomGets = 0;
                    globalThis.rawKeepifCustomCalls = 0;
                    globalThis.rawKeepifCustomQueryCalls = 0;
                    globalThis.rawKeepifConditionQueryCalls = 0;
                    let owned = {{
                      marker: 'raw-keepif-terminal-owned', route: '{route}'
                    }};
                    let conditionOwned = {{ marker: 'condition-owned' }};
                    const conditionOwnedCapture = conditionOwned;
                    let conditionQuery = function (state) {{
                      rawKeepifConditionQueryCalls++;
                      return pure(conditionOwnedCapture).query(state);
                    }};
                    let condition = new Pattern(conditionQuery);
                    let outer = {{ marker: 'outer' }};
                    let extra = {{ marker: 'ignored extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawKeepifCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawKeepifCustomCallbackRef = new WeakRef(callback);
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{
                        rawKeepifCustomGets++;
                        return method;
                      }},
                    }});
                    globalThis.rawKeepifCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawKeepifCustomDroppedRefs = [
                      new WeakRef(conditionOwned), new WeakRef(condition),
                      new WeakRef(conditionQuery), new WeakRef(outer),
                      new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._keepif, outer, [condition, extra]
                    );
                    globalThis.rawKeepifCustomExactReturned = Number(
                      result === returned
                    );
                    globalThis.rawKeepifCustomPattern = result;
                    owned = conditionOwned = condition = conditionQuery = outer =
                      extra = method = returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw keepif {route}: {error}"));
        assert_eq!(rt.get_number("rawKeepifCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepifCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepifCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawKeepifCustomQueryCalls"), Some(0.0));
        assert_eq!(rt.get_number("rawKeepifConditionQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom raw keepif");
        assert!(
            rt.active_needs_host(),
            "{route}: JS-owned custom raw keepif terminal lost its host"
        );
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom raw keepif terminal gained false purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawKeepifCustomPattern.query(state);
                    const owned = rawKeepifCustomOwnedRef.deref();
                    globalThis.rawKeepifCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-keepif-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawKeepifCustomPruning = Number(
                      rawKeepifCustomCallbackRef.deref() === undefined
                      && rawKeepifCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawKeepifCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom raw keepif {route}: {error}"));
            assert_eq!(
                rt.get_number("rawKeepifCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawKeepifCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: unused mapper/condition remained rooted"
            );
            assert_eq!(rt.get_number("rawKeepifCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawKeepifCustomCalls"), Some(1.0));
            assert_eq!(rt.get_number("rawKeepifConditionQueryCalls"), Some(0.0));
            assert_eq!(
                rt.get_number("rawKeepifCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Canonical `_eqt` is always a JavaScript `fmap` callback, even though its
/// terminal value is a boolean. The source graph and the captured comparison
/// value must therefore remain rooted for both the equal and unequal routes;
/// construction-only dispatch objects and ignored extras must not.
#[test]
fn raw_eqt_canonical_fmap_retains_source_and_captured_owners_for_both_results() {
    for equal in [false, true] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawEqtFmapGets = 0;
                    globalThis.rawEqtFmapCalls = 0;
                    globalThis.rawEqtExtraQueryCalls = 0;
                    let sourceOwner = {{ marker: 'raw-eqt-source-owner' }};
                    let capturedOwner = {{ marker: 'raw-eqt-captured-owner' }};
                    let sourceValue = {source_value};
                    let capturedValue = {captured_value};
                    let source = pure(sourceValue).setSteps(7);
                    const canonicalFmap = source.fmap;
                    let method = function (callback) {{
                      'use strict';
                      rawEqtFmapCalls++;
                      if (this !== source || arguments.length !== 1) {{
                        throw new Error('raw eqt fmap receiver/arity changed');
                      }}
                      globalThis.rawEqtCanonicalCallbackRef = new WeakRef(callback);
                      return Reflect.apply(canonicalFmap, this, [callback]);
                    }};
                    Object.defineProperty(source, 'fmap', {{
                      configurable: true,
                      get() {{
                        rawEqtFmapGets++;
                        return method;
                      }},
                    }});
                    let extraOwned = {{ marker: 'raw-eqt-extra-owned' }};
                    const extraOwnedCapture = extraOwned;
                    let extraQuery = function (state) {{
                      rawEqtExtraQueryCalls++;
                      return pure(extraOwnedCapture).query(state);
                    }};
                    let extraPattern = new Pattern(extraQuery);
                    globalThis.rawEqtSourceOwnerRef = new WeakRef(sourceOwner);
                    globalThis.rawEqtCapturedOwnerRef = new WeakRef(capturedOwner);
                    globalThis.rawEqtSourceValueRef = new WeakRef(sourceValue);
                    globalThis.rawEqtCapturedValueRef = new WeakRef(capturedValue);
                    globalThis.rawEqtDroppedRefs = [
                      new WeakRef(source), new WeakRef(method),
                      new WeakRef(extraOwned), new WeakRef(extraPattern),
                      new WeakRef(extraQuery),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._eqt,
                      source,
                      [capturedValue, extraPattern]
                    );
                    globalThis.rawEqtCanonicalShape = Number(
                      result !== source
                      && result.query !== source.query
                      && result._steps?.show() === '7/1'
                      && !Object.hasOwn(result, '__pure')
                      && !Object.hasOwn(result, '__pure_loc')
                    );
                    globalThis.rawEqtCanonicalPattern = result;
                    sourceOwner = capturedOwner = sourceValue = capturedValue =
                      source = method = extraOwned = extraPattern = extraQuery = null;
                    return result;
                  }})()
                "#,
                source_value = if equal {
                    "{ sourceOwner, capturedOwner }"
                } else {
                    "{ sourceOwner }"
                },
                captured_value = if equal {
                    "sourceValue"
                } else {
                    "{ capturedOwner }"
                },
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct canonical raw eqt {equal}: {error}"));
        assert_eq!(rt.get_number("rawEqtFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawEqtFmapCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawEqtCanonicalShape"), Some(1.0));
        assert_eq!(rt.get_number("rawEqtExtraQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active canonical raw eqt");
        assert_eq!(active.steps, Some(Fraction::int(7)));
        assert!(
            rt.active_needs_host(),
            "{equal}: canonical raw eqt lost its generated mapper host"
        );
        assert!(
            !active.is_pure()
                && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
            "{equal}: canonical raw eqt mapper was granted host-free purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        const state = {{
                          span: {{ begin: 0, end: 1 }}, controls: {{}}
                        }};
                        const haps = rawEqtCanonicalPattern.query(state);
                        const sourceValue = rawEqtSourceValueRef.deref();
                        const capturedValue = rawEqtCapturedValueRef.deref();
                        globalThis.rawEqtCanonicalValue = Number(
                          haps.length === 1 && haps[0].value === {expected}
                        );
                        globalThis.rawEqtCanonicalOwners = Number(
                          sourceValue !== undefined
                          && capturedValue !== undefined
                          && rawEqtSourceOwnerRef.deref()?.marker
                            === 'raw-eqt-source-owner'
                          && rawEqtCapturedOwnerRef.deref()?.marker
                            === 'raw-eqt-captured-owner'
                          && (sourceValue === capturedValue) === {equal}
                          && rawEqtCanonicalCallbackRef.deref() !== undefined
                        );
                        globalThis.rawEqtCanonicalPruning = Number(
                          rawEqtDroppedRefs.every(
                            reference => reference.deref() === undefined
                          )
                        );
                        return rawEqtCanonicalPattern;
                      }})()
                    "#,
                    expected = equal,
                    equal = equal,
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query canonical raw eqt {equal}: {error}"));
            for field in [
                "rawEqtCanonicalValue",
                "rawEqtCanonicalOwners",
                "rawEqtCanonicalPruning",
            ] {
                assert_eq!(
                    rt.get_number(field),
                    Some(1.0),
                    "result {equal} turn {turn}: raw eqt discriminator {field} failed"
                );
            }
            assert_eq!(rt.get_number("rawEqtExtraQueryCalls"), Some(0.0));
            assert_eq!(rt.get_number("rawEqtFmapGets"), Some(1.0));
            assert_eq!(rt.get_number("rawEqtFmapCalls"), Some(1.0));
        }
    }
}

/// A replaced `fmap` owns `_eqt`'s entire handoff. Stable native terminals
/// need no host after teardown; JavaScript terminals retain only their own
/// value/query owner and must prune the unused mapper, comparison value,
/// receiver, method, and extras.
#[test]
fn raw_eqt_custom_fmap_returns_exact_terminal_and_prunes_unused_comparison() {
    let pattern = {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            r#"Reflect.apply(
                 Pattern.prototype._eqt,
                 { fmap() { return sequence('a', 'b'); } },
                 [{ marker: 'unused comparison' }, { marker: 'extra' }]
               )"#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("construct host-free custom-fmap raw eqt");
        let pattern = rt
            .active_pattern()
            .expect("active host-free custom raw eqt");
        assert!(pattern.is_pure(), "stable native terminal became impure");
        assert!(
            pattern.reachable_callbacks().is_empty(),
            "unused raw eqt mapper leaked into stable native terminal"
        );
        pattern
    };
    let pure = pattern
        .as_pure_pattern()
        .expect("custom-fmap raw eqt native terminal must upgrade after teardown");
    assert_eq!(
        pure.query_arc(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "host-free custom-fmap raw eqt terminal changed"
    );

    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawEqtCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawEqtCustomGets = 0;
                    globalThis.rawEqtCustomCalls = 0;
                    globalThis.rawEqtCustomQueryCalls = 0;
                    let owned = {{ marker: 'raw-eqt-terminal-owned', route: '{route}' }};
                    let comparisonOwned = {{ marker: 'comparison-owned' }};
                    let comparison = {{ comparisonOwned }};
                    let outer = {{ marker: 'outer' }};
                    let extra = {{ marker: 'ignored extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawEqtCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawEqtCustomCallbackRef = new WeakRef(callback);
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{
                        rawEqtCustomGets++;
                        return method;
                      }},
                    }});
                    globalThis.rawEqtCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawEqtCustomDroppedRefs = [
                      new WeakRef(comparisonOwned), new WeakRef(comparison),
                      new WeakRef(outer), new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._eqt, outer, [comparison, extra]
                    );
                    globalThis.rawEqtCustomExactReturned = Number(result === returned);
                    globalThis.rawEqtCustomPattern = result;
                    owned = comparisonOwned = comparison = outer = extra = method =
                      returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw eqt {route}: {error}"));
        assert_eq!(rt.get_number("rawEqtCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawEqtCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawEqtCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawEqtCustomQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom raw eqt");
        assert!(
            rt.active_needs_host(),
            "{route}: JS-owned custom raw eqt terminal lost its host"
        );
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom raw eqt terminal gained false purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawEqtCustomPattern.query(state);
                    const owned = rawEqtCustomOwnedRef.deref();
                    globalThis.rawEqtCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-eqt-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawEqtCustomPruning = Number(
                      rawEqtCustomCallbackRef.deref() === undefined
                      && rawEqtCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawEqtCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom raw eqt {route}: {error}"));
            assert_eq!(
                rt.get_number("rawEqtCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawEqtCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: unused mapper/comparison remained rooted"
            );
            assert_eq!(rt.get_number("rawEqtCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawEqtCustomCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawEqtCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Canonical `_net` retains the generated mapper, exact source value, and
/// captured comparison value for both false (same identity) and true
/// (distinct identity) results. Construction-only wrappers and ignored extras
/// must still be collectible.
#[test]
fn raw_net_canonical_fmap_retains_source_and_captured_owners_for_both_results() {
    for different in [false, true] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawNetFmapGets = 0;
                    globalThis.rawNetFmapCalls = 0;
                    globalThis.rawNetExtraQueryCalls = 0;
                    let sourceOwner = {{ marker: 'raw-net-source-owner' }};
                    let capturedOwner = {{ marker: 'raw-net-captured-owner' }};
                    let sourceValue = {source_value};
                    let capturedValue = {captured_value};
                    let source = pure(sourceValue).setSteps(7);
                    const canonicalFmap = source.fmap;
                    let method = function (callback) {{
                      'use strict';
                      rawNetFmapCalls++;
                      if (this !== source || arguments.length !== 1) {{
                        throw new Error('raw net fmap receiver/arity changed');
                      }}
                      globalThis.rawNetCanonicalCallbackRef = new WeakRef(callback);
                      return Reflect.apply(canonicalFmap, this, [callback]);
                    }};
                    Object.defineProperty(source, 'fmap', {{
                      configurable: true,
                      get() {{ rawNetFmapGets++; return method; }},
                    }});
                    let extraOwned = {{ marker: 'raw-net-extra-owned' }};
                    const extraCapture = extraOwned;
                    let extraQuery = function (state) {{
                      rawNetExtraQueryCalls++;
                      return pure(extraCapture).query(state);
                    }};
                    let extraPattern = new Pattern(extraQuery);
                    globalThis.rawNetSourceOwnerRef = new WeakRef(sourceOwner);
                    globalThis.rawNetCapturedOwnerRef = new WeakRef(capturedOwner);
                    globalThis.rawNetSourceValueRef = new WeakRef(sourceValue);
                    globalThis.rawNetCapturedValueRef = new WeakRef(capturedValue);
                    globalThis.rawNetDroppedRefs = [
                      new WeakRef(source), new WeakRef(method),
                      new WeakRef(extraOwned), new WeakRef(extraPattern),
                      new WeakRef(extraQuery),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._net,
                      source,
                      [capturedValue, extraPattern]
                    );
                    globalThis.rawNetCanonicalShape = Number(
                      result !== source
                      && result.query !== source.query
                      && result._steps?.show() === '7/1'
                      && !Object.hasOwn(result, '__pure')
                      && !Object.hasOwn(result, '__pure_loc')
                    );
                    globalThis.rawNetCanonicalPattern = result;
                    sourceOwner = capturedOwner = sourceValue = capturedValue =
                      source = method = extraOwned = extraPattern = extraQuery = null;
                    return result;
                  }})()
                "#,
                source_value = if different {
                    "{ sourceOwner }"
                } else {
                    "{ sourceOwner, capturedOwner }"
                },
                captured_value = if different {
                    "{ capturedOwner }"
                } else {
                    "sourceValue"
                },
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct canonical raw net {different}: {error}"));
        assert_eq!(rt.get_number("rawNetFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawNetFmapCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawNetCanonicalShape"), Some(1.0));
        assert_eq!(rt.get_number("rawNetExtraQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active canonical raw net");
        assert_eq!(active.steps, Some(Fraction::int(7)));
        assert!(
            rt.active_needs_host(),
            "{different}: canonical raw net lost its generated mapper host"
        );
        assert!(
            !active.is_pure()
                && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
            "{different}: canonical raw net mapper gained host-free purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        const state = {{
                          span: {{ begin: 0, end: 1 }}, controls: {{}}
                        }};
                        const haps = rawNetCanonicalPattern.query(state);
                        const sourceValue = rawNetSourceValueRef.deref();
                        const capturedValue = rawNetCapturedValueRef.deref();
                        globalThis.rawNetCanonicalValue = Number(
                          haps.length === 1 && haps[0].value === {expected}
                        );
                        globalThis.rawNetCanonicalOwners = Number(
                          sourceValue !== undefined
                          && capturedValue !== undefined
                          && rawNetSourceOwnerRef.deref()?.marker
                            === 'raw-net-source-owner'
                          && rawNetCapturedOwnerRef.deref()?.marker
                            === 'raw-net-captured-owner'
                          && (sourceValue !== capturedValue) === {different}
                          && rawNetCanonicalCallbackRef.deref() !== undefined
                        );
                        globalThis.rawNetCanonicalPruning = Number(
                          rawNetDroppedRefs.every(
                            reference => reference.deref() === undefined
                          )
                        );
                        return rawNetCanonicalPattern;
                      }})()
                    "#,
                    expected = different,
                    different = different,
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query canonical raw net {different}: {error}"));
            for field in [
                "rawNetCanonicalValue",
                "rawNetCanonicalOwners",
                "rawNetCanonicalPruning",
            ] {
                assert_eq!(
                    rt.get_number(field),
                    Some(1.0),
                    "result {different} turn {turn}: raw net discriminator {field} failed"
                );
            }
            assert_eq!(rt.get_number("rawNetExtraQueryCalls"), Some(0.0));
            assert_eq!(rt.get_number("rawNetFmapGets"), Some(1.0));
            assert_eq!(rt.get_number("rawNetFmapCalls"), Some(1.0));
        }
    }
}

/// A custom `fmap` owns `_net`'s handoff. Native terminals can become
/// host-free, while exact JavaScript terminals retain only their own
/// value/query owner and prune the unused mapper, comparison, and transients.
#[test]
fn raw_net_custom_fmap_returns_exact_terminal_and_prunes_unused_comparison() {
    let pattern = {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            r#"Reflect.apply(
                 Pattern.prototype._net,
                 { fmap() { return sequence('a', 'b'); } },
                 [{ marker: 'unused comparison' }, { marker: 'extra' }]
               )"#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("construct host-free custom-fmap raw net");
        let pattern = rt.active_pattern().expect("active custom raw net");
        assert!(pattern.is_pure(), "stable native terminal became impure");
        assert!(
            pattern.reachable_callbacks().is_empty(),
            "unused raw net mapper leaked into stable native terminal"
        );
        pattern
    };
    let pure = pattern
        .as_pure_pattern()
        .expect("custom-fmap raw net native terminal must upgrade after teardown");
    assert_eq!(
        pure.query_arc(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "host-free custom-fmap raw net terminal changed"
    );

    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawNetCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawNetCustomGets = 0;
                    globalThis.rawNetCustomCalls = 0;
                    globalThis.rawNetCustomQueryCalls = 0;
                    let owned = {{ marker: 'raw-net-terminal-owned', route: '{route}' }};
                    let comparisonOwned = {{ marker: 'comparison-owned' }};
                    let comparison = {{ comparisonOwned }};
                    let outer = {{ marker: 'outer' }};
                    let extra = {{ marker: 'ignored extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawNetCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawNetCustomCallbackRef = new WeakRef(callback);
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{ rawNetCustomGets++; return method; }},
                    }});
                    globalThis.rawNetCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawNetCustomDroppedRefs = [
                      new WeakRef(comparisonOwned), new WeakRef(comparison),
                      new WeakRef(outer), new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._net, outer, [comparison, extra]
                    );
                    globalThis.rawNetCustomExactReturned = Number(result === returned);
                    globalThis.rawNetCustomPattern = result;
                    owned = comparisonOwned = comparison = outer = extra = method =
                      returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw net {route}: {error}"));
        assert_eq!(rt.get_number("rawNetCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawNetCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawNetCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawNetCustomQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom raw net");
        assert!(
            rt.active_needs_host(),
            "{route}: JS-owned custom raw net terminal lost its host"
        );
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom raw net terminal gained false purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawNetCustomPattern.query(state);
                    const owned = rawNetCustomOwnedRef.deref();
                    globalThis.rawNetCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-net-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawNetCustomPruning = Number(
                      rawNetCustomCallbackRef.deref() === undefined
                      && rawNetCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawNetCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom raw net {route}: {error}"));
            assert_eq!(
                rt.get_number("rawNetCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawNetCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: unused mapper/comparison remained rooted"
            );
            assert_eq!(rt.get_number("rawNetCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawNetCustomCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawNetCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Canonical `_and` retains its generated mapper, source Pattern/query owner,
/// and captured right-hand operand on both branches. The false branch returns
/// its exact left operand but still owns the semantically ignored capture.
#[test]
fn raw_and_canonical_fmap_retains_both_branch_owners_and_ignored_capture() {
    for truthy in [false, true] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawAndFmapGets = 0;
                    globalThis.rawAndFmapCalls = 0;
                    globalThis.rawAndSourceQueryCalls = 0;
                    globalThis.rawAndExtraQueryCalls = 0;
                    let sourceOwner = {{ marker: 'raw-and-source-owner' }};
                    let capturedOwner = {{ marker: 'raw-and-captured-owner' }};
                    let sourceValue = {source_value};
                    let capturedValue = {{ capturedOwner }};
                    let sourceQuery = ((owner, value) => function (state) {{
                      rawAndSourceQueryCalls++;
                      if (owner.marker !== 'raw-and-source-owner') {{
                        throw new Error('raw and source owner lost');
                      }}
                      return pure(value).query(state);
                    }})(sourceOwner, sourceValue);
                    let source = new Pattern(sourceQuery).setSteps(7);
                    const canonicalFmap = source.fmap;
                    let method = function (callback) {{
                      'use strict';
                      rawAndFmapCalls++;
                      if (this !== source || arguments.length !== 1) {{
                        throw new Error('raw and fmap receiver/arity changed');
                      }}
                      globalThis.rawAndCanonicalCallbackRef = new WeakRef(callback);
                      return Reflect.apply(canonicalFmap, this, [callback]);
                    }};
                    Object.defineProperty(source, 'fmap', {{
                      configurable: true,
                      get() {{ rawAndFmapGets++; return method; }},
                    }});
                    let extraOwned = {{ marker: 'raw-and-extra-owned' }};
                    const extraCapture = extraOwned;
                    let extraQuery = function (state) {{
                      rawAndExtraQueryCalls++;
                      return pure(extraCapture).query(state);
                    }};
                    let extraPattern = new Pattern(extraQuery);
                    globalThis.rawAndSourceOwnerRef = new WeakRef(sourceOwner);
                    globalThis.rawAndCapturedOwnerRef = new WeakRef(capturedOwner);
                    globalThis.rawAndCapturedValueRef = new WeakRef(capturedValue);
                    globalThis.rawAndSourceValueRef = {source_ref};
                    globalThis.rawAndSourcePatternRef = new WeakRef(source);
                    globalThis.rawAndSourceCallbackRef = new WeakRef(sourceQuery);
                    globalThis.rawAndDroppedRefs = [
                      new WeakRef(method),
                      new WeakRef(extraOwned), new WeakRef(extraPattern),
                      new WeakRef(extraQuery),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._and,
                      source,
                      [capturedValue, extraPattern]
                    );
                    globalThis.rawAndCanonicalShape = Number(
                      result !== source
                      && result.query !== source.query
                      && result._steps?.show() === '7/1'
                      && !Object.hasOwn(result, '__pure')
                      && !Object.hasOwn(result, '__pure_loc')
                    );
                    globalThis.rawAndCanonicalPattern = result;
                    sourceOwner = capturedOwner = sourceValue = capturedValue =
                      sourceQuery = source = method = extraOwned = extraPattern =
                      extraQuery = null;
                    return result;
                  }})()
                "#,
                source_value = if truthy { "{ sourceOwner }" } else { "0" },
                source_ref = if truthy {
                    "new WeakRef(sourceValue)"
                } else {
                    "null"
                },
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct canonical raw and {truthy}: {error}"));
        assert_eq!(rt.get_number("rawAndFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawAndFmapCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawAndCanonicalShape"), Some(1.0));
        assert_eq!(rt.get_number("rawAndSourceQueryCalls"), Some(0.0));
        assert_eq!(rt.get_number("rawAndExtraQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active canonical raw and");
        assert_eq!(active.steps, Some(Fraction::int(7)));
        assert!(
            rt.active_needs_host(),
            "{truthy}: canonical raw and lost its lexical mapper/source host"
        );
        assert!(
            !active.is_pure()
                && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
            "{truthy}: canonical raw and gained host-free purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        const state = {{
                          span: {{ begin: 0, end: 1 }}, controls: {{}}
                        }};
                        const haps = rawAndCanonicalPattern.query(state);
                        const capturedValue = rawAndCapturedValueRef.deref();
                        const sourceValue = rawAndSourceValueRef?.deref();
                        globalThis.rawAndCanonicalValue = Number(
                          haps.length === 1
                          && ({value_check})
                        );
                        globalThis.rawAndCanonicalOwners = Number(
                          rawAndSourceOwnerRef.deref()?.marker
                            === 'raw-and-source-owner'
                          && rawAndCapturedOwnerRef.deref()?.marker
                            === 'raw-and-captured-owner'
                          && capturedValue?.capturedOwner
                            === rawAndCapturedOwnerRef.deref()
                          && ({source_check})
                          && rawAndSourcePatternRef.deref() !== undefined
                          && rawAndSourceCallbackRef.deref() !== undefined
                          && rawAndCanonicalCallbackRef.deref() !== undefined
                        );
                        globalThis.rawAndCanonicalPruningMask = JSON.stringify(
                          rawAndDroppedRefs.map(
                            reference => reference.deref() === undefined
                          )
                        );
                        globalThis.rawAndCanonicalPruning = Number(
                          rawAndDroppedRefs.every(
                            reference => reference.deref() === undefined
                          )
                        );
                        return rawAndCanonicalPattern;
                      }})()
                    "#,
                    value_check = if truthy {
                        "haps[0].value === capturedValue"
                    } else {
                        "Object.is(haps[0].value, 0)"
                    },
                    source_check = if truthy {
                        "sourceValue?.sourceOwner === rawAndSourceOwnerRef.deref()"
                    } else {
                        "sourceValue === undefined"
                    },
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query canonical raw and {truthy}: {error}"));
            for field in [
                "rawAndCanonicalValue",
                "rawAndCanonicalOwners",
                "rawAndCanonicalPruning",
            ] {
                assert_eq!(
                    rt.get_number(field),
                    Some(1.0),
                    "branch {truthy} turn {turn}: raw and discriminator {field} failed; mask {:?}",
                    rt.get_string("rawAndCanonicalPruningMask")
                );
            }
            assert_eq!(
                rt.get_number("rawAndSourceQueryCalls"),
                Some(f64::from(turn))
            );
            assert_eq!(rt.get_number("rawAndExtraQueryCalls"), Some(0.0));
            assert_eq!(rt.get_number("rawAndFmapGets"), Some(1.0));
            assert_eq!(rt.get_number("rawAndFmapCalls"), Some(1.0));
        }
    }
}

/// A custom `fmap` owns `_and`'s handoff. Native terminals can become
/// host-free, while exact JavaScript terminals retain only their own
/// value/query owner and prune the unused mapper, capture, and transients.
#[test]
fn raw_and_custom_fmap_returns_exact_terminal_and_prunes_unused_capture() {
    let pattern = {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            r#"Reflect.apply(
                 Pattern.prototype._and,
                 { fmap() { return sequence('a', 'b'); } },
                 [{ marker: 'unused capture' }, { marker: 'extra' }]
               )"#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("construct host-free custom-fmap raw and");
        let pattern = rt.active_pattern().expect("active custom raw and");
        assert!(pattern.is_pure(), "stable native terminal became impure");
        assert!(
            pattern.reachable_callbacks().is_empty(),
            "unused raw and mapper leaked into stable native terminal"
        );
        pattern
    };
    let pure = pattern
        .as_pure_pattern()
        .expect("custom-fmap raw and native terminal must upgrade after teardown");
    assert_eq!(
        pure.query_arc(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "host-free custom-fmap raw and terminal changed"
    );

    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawAndCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawAndCustomGets = 0;
                    globalThis.rawAndCustomCalls = 0;
                    globalThis.rawAndCustomQueryCalls = 0;
                    let owned = {{ marker: 'raw-and-terminal-owned', route: '{route}' }};
                    let captureOwned = {{ marker: 'capture-owned' }};
                    let capture = {{ captureOwned }};
                    let outer = {{ marker: 'outer' }};
                    let extra = {{ marker: 'ignored extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawAndCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawAndCustomCallbackRef = new WeakRef(callback);
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{ rawAndCustomGets++; return method; }},
                    }});
                    globalThis.rawAndCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawAndCustomDroppedRefs = [
                      new WeakRef(captureOwned), new WeakRef(capture),
                      new WeakRef(outer), new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._and, outer, [capture, extra]
                    );
                    globalThis.rawAndCustomExactReturned = Number(result === returned);
                    globalThis.rawAndCustomPattern = result;
                    owned = captureOwned = capture = outer = extra = method =
                      returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw and {route}: {error}"));
        assert_eq!(rt.get_number("rawAndCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawAndCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawAndCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawAndCustomQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom raw and");
        assert!(
            rt.active_needs_host(),
            "{route}: JS-owned custom raw and terminal lost its host"
        );
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom raw and terminal gained false purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawAndCustomPattern.query(state);
                    const owned = rawAndCustomOwnedRef.deref();
                    globalThis.rawAndCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-and-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawAndCustomPruning = Number(
                      rawAndCustomCallbackRef.deref() === undefined
                      && rawAndCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawAndCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom raw and {route}: {error}"));
            assert_eq!(
                rt.get_number("rawAndCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawAndCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: unused mapper/capture remained rooted"
            );
            assert_eq!(rt.get_number("rawAndCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawAndCustomCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawAndCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Canonical `_or` retains its generated mapper, source Pattern/query owner,
/// and captured right-hand operand on both branches. The truthy branch returns
/// the left value after custom source-query materialization, explicitly not the
/// original source wrapper, while still owning the ignored capture. The falsey
/// branch returns the selected captured right operand with exact identity.
#[test]
fn raw_or_canonical_fmap_retains_both_branch_owners_and_ignored_capture() {
    for truthy in [false, true] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawOrFmapGets = 0;
                    globalThis.rawOrFmapCalls = 0;
                    globalThis.rawOrSourceQueryCalls = 0;
                    globalThis.rawOrExtraQueryCalls = 0;
                    let sourceOwner = {{ marker: 'raw-or-source-owner' }};
                    let capturedOwner = {{ marker: 'raw-or-captured-owner' }};
                    let sourceValue = {source_value};
                    let capturedValue = {{ capturedOwner }};
                    let sourceQuery = ((owner, value) => function (state) {{
                      rawOrSourceQueryCalls++;
                      if (owner.marker !== 'raw-or-source-owner') {{
                        throw new Error('raw or source owner lost');
                      }}
                      return pure(value).query(state);
                    }})(sourceOwner, sourceValue);
                    let source = new Pattern(sourceQuery).setSteps(7);
                    const canonicalFmap = source.fmap;
                    let method = function (callback) {{
                      'use strict';
                      rawOrFmapCalls++;
                      if (this !== source || arguments.length !== 1) {{
                        throw new Error('raw or fmap receiver/arity changed');
                      }}
                      globalThis.rawOrCanonicalCallbackRef = new WeakRef(callback);
                      return Reflect.apply(canonicalFmap, this, [callback]);
                    }};
                    Object.defineProperty(source, 'fmap', {{
                      configurable: true,
                      get() {{ rawOrFmapGets++; return method; }},
                    }});
                    let extraOwned = {{ marker: 'raw-or-extra-owned' }};
                    const extraCapture = extraOwned;
                    let extraQuery = function (state) {{
                      rawOrExtraQueryCalls++;
                      return pure(extraCapture).query(state);
                    }};
                    let extraPattern = new Pattern(extraQuery);
                    globalThis.rawOrSourceOwnerRef = new WeakRef(sourceOwner);
                    globalThis.rawOrCapturedOwnerRef = new WeakRef(capturedOwner);
                    globalThis.rawOrCapturedValueRef = new WeakRef(capturedValue);
                    globalThis.rawOrSourceValueRef = {source_ref};
                    globalThis.rawOrSourcePatternRef = new WeakRef(source);
                    globalThis.rawOrSourceCallbackRef = new WeakRef(sourceQuery);
                    globalThis.rawOrDroppedRefs = [
                      new WeakRef(method),
                      new WeakRef(extraOwned), new WeakRef(extraPattern),
                      new WeakRef(extraQuery),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._or,
                      source,
                      [capturedValue, extraPattern]
                    );
                    globalThis.rawOrCanonicalShape = Number(
                      result !== source
                      && result.query !== source.query
                      && result._steps?.show() === '7/1'
                      && !Object.hasOwn(result, '__pure')
                      && !Object.hasOwn(result, '__pure_loc')
                    );
                    globalThis.rawOrCanonicalPattern = result;
                    sourceOwner = capturedOwner = sourceValue = capturedValue =
                      sourceQuery = source = method = extraOwned = extraPattern =
                      extraQuery = null;
                    return result;
                  }})()
                "#,
                source_value = if truthy { "{ sourceOwner }" } else { "0" },
                source_ref = if truthy {
                    "new WeakRef(sourceValue)"
                } else {
                    "null"
                },
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct canonical raw or {truthy}: {error}"));
        assert_eq!(rt.get_number("rawOrFmapGets"), Some(1.0));
        assert_eq!(rt.get_number("rawOrFmapCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawOrCanonicalShape"), Some(1.0));
        assert_eq!(rt.get_number("rawOrSourceQueryCalls"), Some(0.0));
        assert_eq!(rt.get_number("rawOrExtraQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active canonical raw or");
        assert_eq!(active.steps, Some(Fraction::int(7)));
        assert!(
            rt.active_needs_host(),
            "{truthy}: canonical raw or lost its lexical mapper/source host"
        );
        assert!(
            !active.is_pure()
                && (!active.reachable_callbacks().is_empty() || active.purity().opaque),
            "{truthy}: canonical raw or gained host-free purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        const state = {{
                          span: {{ begin: 0, end: 1 }}, controls: {{}}
                        }};
                        const haps = rawOrCanonicalPattern.query(state);
                        const capturedValue = rawOrCapturedValueRef.deref();
                        const sourceValue = rawOrSourceValueRef?.deref();
                        globalThis.rawOrCanonicalValue = Number(
                          haps.length === 1
                          && ({value_check})
                        );
                        globalThis.rawOrCanonicalOwners = Number(
                          rawOrSourceOwnerRef.deref()?.marker
                            === 'raw-or-source-owner'
                          && rawOrCapturedOwnerRef.deref()?.marker
                            === 'raw-or-captured-owner'
                          && capturedValue?.capturedOwner
                            === rawOrCapturedOwnerRef.deref()
                          && ({source_check})
                          && rawOrSourcePatternRef.deref() !== undefined
                          && rawOrSourceCallbackRef.deref() !== undefined
                          && rawOrCanonicalCallbackRef.deref() !== undefined
                        );
                        globalThis.rawOrCanonicalPruningMask = JSON.stringify(
                          rawOrDroppedRefs.map(
                            reference => reference.deref() === undefined
                          )
                        );
                        globalThis.rawOrCanonicalPruning = Number(
                          rawOrDroppedRefs.every(
                            reference => reference.deref() === undefined
                          )
                        );
                        return rawOrCanonicalPattern;
                      }})()
                    "#,
                    value_check = if truthy {
                        concat!(
                            "haps[0].value !== sourceValue && ",
                            "haps[0].value?.sourceOwner?.marker === ",
                            "'raw-or-source-owner'"
                        )
                    } else {
                        "haps[0].value === capturedValue"
                    },
                    source_check = if truthy {
                        "sourceValue?.sourceOwner === rawOrSourceOwnerRef.deref()"
                    } else {
                        "sourceValue === undefined"
                    },
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query canonical raw or {truthy}: {error}"));
            for field in [
                "rawOrCanonicalValue",
                "rawOrCanonicalOwners",
                "rawOrCanonicalPruning",
            ] {
                assert_eq!(
                    rt.get_number(field),
                    Some(1.0),
                    "branch {truthy} turn {turn}: raw or discriminator {field} failed; mask {:?}",
                    rt.get_string("rawOrCanonicalPruningMask")
                );
            }
            assert_eq!(
                rt.get_number("rawOrSourceQueryCalls"),
                Some(f64::from(turn))
            );
            assert_eq!(rt.get_number("rawOrExtraQueryCalls"), Some(0.0));
            assert_eq!(rt.get_number("rawOrFmapGets"), Some(1.0));
            assert_eq!(rt.get_number("rawOrFmapCalls"), Some(1.0));
        }
    }
}

/// A custom `fmap` owns `_or`'s handoff. Native terminals can become
/// host-free, while exact JavaScript terminals retain only their own
/// value/query owner and prune the unused mapper, capture, and transients.
#[test]
fn raw_or_custom_fmap_returns_exact_terminal_and_prunes_unused_capture() {
    let pattern = {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            r#"Reflect.apply(
                 Pattern.prototype._or,
                 { fmap() { return sequence('a', 'b'); } },
                 [{ marker: 'unused capture' }, { marker: 'extra' }]
               )"#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("construct host-free custom-fmap raw or");
        let pattern = rt.active_pattern().expect("active custom raw or");
        assert!(pattern.is_pure(), "stable native terminal became impure");
        assert!(
            pattern.reachable_callbacks().is_empty(),
            "unused raw or mapper leaked into stable native terminal"
        );
        pattern
    };
    let pure = pattern
        .as_pure_pattern()
        .expect("custom-fmap raw or native terminal must upgrade after teardown");
    assert_eq!(
        pure.query_arc(Fraction::ZERO, Fraction::ONE)
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "host-free custom-fmap raw or terminal changed"
    );

    for route in ["value", "query"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let make_result = if route == "value" {
            "owned => pure(owned)"
        } else {
            r#"owned => new Pattern(state => {
                 rawOrCustomQueryCalls++;
                 return pure(owned).query(state);
               })"#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawOrCustomGets = 0;
                    globalThis.rawOrCustomCalls = 0;
                    globalThis.rawOrCustomQueryCalls = 0;
                    let owned = {{ marker: 'raw-or-terminal-owned', route: '{route}' }};
                    let captureOwned = {{ marker: 'capture-owned' }};
                    let capture = {{ captureOwned }};
                    let outer = {{ marker: 'outer' }};
                    let extra = {{ marker: 'ignored extra' }};
                    const makeResult = {make_result};
                    let returned = makeResult(owned);
                    let method = function (callback) {{
                      'use strict';
                      rawOrCustomCalls++;
                      if (this !== outer || arguments.length !== 1) {{
                        throw new Error('custom fmap receiver/arity changed');
                      }}
                      globalThis.rawOrCustomCallbackRef = new WeakRef(callback);
                      return returned;
                    }};
                    Object.defineProperty(outer, 'fmap', {{
                      configurable: true,
                      get() {{ rawOrCustomGets++; return method; }},
                    }});
                    globalThis.rawOrCustomOwnedRef = new WeakRef(owned);
                    globalThis.rawOrCustomDroppedRefs = [
                      new WeakRef(captureOwned), new WeakRef(capture),
                      new WeakRef(outer), new WeakRef(extra), new WeakRef(method),
                    ];
                    const result = Reflect.apply(
                      Pattern.prototype._or, outer, [capture, extra]
                    );
                    globalThis.rawOrCustomExactReturned = Number(result === returned);
                    globalThis.rawOrCustomPattern = result;
                    owned = captureOwned = capture = outer = extra = method =
                      returned = null;
                    return result;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct custom-fmap raw or {route}: {error}"));
        assert_eq!(rt.get_number("rawOrCustomExactReturned"), Some(1.0));
        assert_eq!(rt.get_number("rawOrCustomGets"), Some(1.0));
        assert_eq!(rt.get_number("rawOrCustomCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawOrCustomQueryCalls"), Some(0.0));
        let active = rt.active_pattern().expect("active custom raw or");
        assert!(
            rt.active_needs_host(),
            "{route}: JS-owned custom raw or terminal lost its host"
        );
        assert!(
            !active.is_pure() || active.purity().opaque,
            "{route}: JS-owned custom raw or terminal gained false purity"
        );

        for turn in 1..=2 {
            for _ in 0..3 {
                rt.run_gc();
            }
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = {
                      span: { begin: 0, end: 1 }, controls: {}
                    };
                    const haps = rawOrCustomPattern.query(state);
                    const owned = rawOrCustomOwnedRef.deref();
                    globalThis.rawOrCustomOwnership = Number(
                      haps.length === 1
                      && owned?.marker === 'raw-or-terminal-owned'
                      && haps[0].value === owned
                    );
                    globalThis.rawOrCustomPruning = Number(
                      rawOrCustomCallbackRef.deref() === undefined
                      && rawOrCustomDroppedRefs.every(
                        reference => reference.deref() === undefined
                      )
                    );
                    return rawOrCustomPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query custom raw or {route}: {error}"));
            assert_eq!(
                rt.get_number("rawOrCustomOwnership"),
                Some(1.0),
                "{route} turn {turn}: custom terminal lost exact JS ownership"
            );
            assert_eq!(
                rt.get_number("rawOrCustomPruning"),
                Some(1.0),
                "{route} turn {turn}: unused mapper/capture remained rooted"
            );
            assert_eq!(rt.get_number("rawOrCustomGets"), Some(1.0));
            assert_eq!(rt.get_number("rawOrCustomCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawOrCustomQueryCalls"),
                Some(if route == "query" {
                    f64::from(turn)
                } else {
                    0.0
                })
            );
        }
    }
}

/// Every hop in the raw bodies is a dynamic public-method lookup. A custom
/// chain is called only while constructing, but the JavaScript Pattern returned
/// by its final expand method must retain both its query callback and the
/// factor-owned value after the intermediate objects have become unreachable.
#[test]
fn raw_extend_replicate_dynamic_results_retain_js_ownership_across_gc() {
    for name in ["_extend", "_replicate"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        let install_chain = if name == "_extend" {
            r#"
              Object.defineProperty(receiver, 'fast', {
                configurable: true,
                get() {
                  rawStepwiseChainLog.push('fast.get');
                  return function (value) {
                    rawStepwiseChainLog.push(
                      `fast.call:${this === receiver}:${value === factor}`
                    );
                    return middle;
                  };
                },
              });
              Object.defineProperty(middle, 'expand', {
                configurable: true,
                get() {
                  rawStepwiseChainLog.push('expand.get');
                  return finish;
                },
              });
            "#
        } else {
            r#"
              Object.defineProperty(receiver, 'repeatCycles', {
                configurable: true,
                get() {
                  rawStepwiseChainLog.push('repeatCycles.get');
                  return function (value) {
                    rawStepwiseChainLog.push(
                      `repeatCycles.call:${this === receiver}:${value === factor}`
                    );
                    return first;
                  };
                },
              });
              Object.defineProperty(first, 'fast', {
                configurable: true,
                get() {
                  rawStepwiseChainLog.push('fast.get');
                  return function (value) {
                    rawStepwiseChainLog.push(
                      `fast.call:${this === first}:${value === factor}`
                    );
                    return middle;
                  };
                },
              });
              Object.defineProperty(middle, 'expand', {
                configurable: true,
                get() {
                  rawStepwiseChainLog.push('expand.get');
                  return finish;
                },
              });
            "#
        };
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawStepwiseChainLog = [];
                    globalThis.rawStepwiseChainQueryCalls = 0;
                    const owned = {{ marker: 89, name: '{name}' }};
                    const factor = {{ marker: 41 }};
                    const receiver = sequence('receiver');
                    const first = {{}};
                    const middle = {{}};
                    const finish = function (value) {{
                      rawStepwiseChainLog.push(
                        `expand.call:${{this === middle}}:${{value === factor}}`
                      );
                      return new Pattern(state => {{
                        rawStepwiseChainQueryCalls++;
                        return pure({{ owned, factor }}).query(state);
                      }});
                    }};
                    {install_chain}
                    globalThis.rawStepwiseChainPattern = receiver.{name}(factor);
                    Object.defineProperty(receiver, 'fast', {{
                      configurable: true,
                      value() {{ throw new Error('late fast reread'); }},
                    }});
                    Object.defineProperty(receiver, 'repeatCycles', {{
                      configurable: true,
                      value() {{ throw new Error('late repeatCycles reread'); }},
                    }});
                    Object.defineProperty(middle, 'expand', {{
                      configurable: true,
                      value() {{ throw new Error('late expand reread'); }},
                    }});
                    globalThis.rawStepwiseChainLogSnapshot =
                      JSON.stringify(rawStepwiseChainLog);
                    return rawStepwiseChainPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {name}: {error}"));

        let expected_log = if name == "_extend" {
            r#"["fast.get","fast.call:true:true","expand.get","expand.call:true:true"]"#
        } else {
            r#"["repeatCycles.get","repeatCycles.call:true:true","fast.get","fast.call:true:true","expand.get","expand.call:true:true"]"#
        };
        assert_eq!(
            rt.get_string("rawStepwiseChainLogSnapshot").as_deref(),
            Some(expected_log),
            "{name}: dynamic chain order changed"
        );
        let active = rt
            .active_pattern()
            .expect("active JS-owned raw extend/replicate result");
        assert!(!active.is_pure(), "{name}: JS result was called pure");
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "{name}: JS result retained no ownership root"
        );
        assert_eq!(rt.get_number("rawStepwiseChainQueryCalls"), Some(0.0));

        for turn in 1..=3 {
            rt.run_gc();
            rt.run_gc();
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = { span: { begin: 0, end: 1 }, controls: {} };
                    const values = rawStepwiseChainPattern.query(state)
                      .map(hap => hap.value);
                    globalThis.rawStepwiseChainOwnershipProof = Number(
                      values.length === 1
                        && values[0].owned.marker === 89
                        && values[0].factor.marker === 41
                    );
                    return rawStepwiseChainPattern;
                  })()
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("query JS-owned {name} after GC: {error}"));
            assert_eq!(rt.get_number("rawStepwiseChainOwnershipProof"), Some(1.0));
            assert_eq!(
                rt.get_number("rawStepwiseChainQueryCalls"),
                Some(f64::from(turn)),
                "{name}: callback count changed after GC"
            );
            assert_eq!(
                rt.get_string("rawStepwiseChainLogSnapshot").as_deref(),
                Some(expected_log),
                "{name}: query reread the construction chain"
            );
        }
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
    assert_eq!(replicate.steps, Some(Fraction::int(2)));

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
    assert_eq!(contract.steps, Some(Fraction::int(3)));
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

/// Canonical `take`/`drop` are native stepwise transforms. Their scalar and
/// pure-patterned routes must remain host-free even after the QuickJS heap
/// that constructed the graph has been destroyed. A no-step receiver is the
/// strongest negative witness: its user query callback must be pruned when the
/// combinator returns `nothing`.
#[test]
fn take_and_drop_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expect_haps) in [
        ("sequence(0, 1, 2).take(1)", true),
        ("take(1, sequence(0, 1, 2))", true),
        ("take(1)(sequence(0, 1, 2))", true),
        ("sequence(0, 1, 2).take(0)", false),
        ("sequence(0, 1, 2).take(-1)", true),
        ("sequence(0, 1, 2).take(1.5)", true),
        ("sequence(0, 1, 2).take(4)", true),
        ("sequence(0, 1, 2).take(sequence(1, 2))", true),
        ("sequence(0, 1, 2).drop(1)", true),
        ("drop(1, sequence(0, 1, 2))", true),
        ("drop(1)(sequence(0, 1, 2))", true),
        ("sequence(0, 1, 2).drop(0)", true),
        ("sequence(0, 1, 2).drop(-1)", true),
        ("sequence(0, 1, 2).drop(1.5)", true),
        ("sequence(0, 1, 2).drop(4)", true),
        ("sequence(0, 1, 2).drop(sequence(1, 2))", true),
        (
            "new Pattern(state => pure('x').query(state)).take(1)",
            false,
        ),
        (
            "new Pattern(state => pure('x').query(state)).drop(1)",
            false,
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active take/drop graph");
            assert!(pattern.is_pure(), "{source}: native graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free graph retained a callback"
            );
            pattern
        }; // The QuickJS runtime and its callback sidecar are gone here.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            !pure.query_arc(Fraction::ZERO, Fraction::ONE).is_empty(),
            expect_haps,
            "{source}: host-free result changed"
        );
    }
}

/// Raw take/drop bypass StepJoin and execute their scalar body eagerly.  A
/// native receiver therefore sheds the construction runtime, including the
/// callback of a no-step receiver.  Mutable zoom/take overrides are different:
/// the override itself is called only while constructing, but a JavaScript
/// Pattern it returns must retain its query callback and exact JS value.
#[test]
fn raw_take_drop_are_eager_host_free_and_retain_override_results() {
    for (source, expected_haps) in [
        ("sequence(0, 1, 2)._take(1)", 1),
        ("sequence(0, 1, 2)._take([1, 2])", 1),
        ("sequence(0, 1, 2)._take(-1)", 1),
        ("sequence(0, 1, 2)._take(4)", 3),
        ("sequence(0, 1, 2)._drop(1)", 2),
        ("sequence(0, 1, 2)._drop([1, 2])", 3),
        ("sequence(0, 1, 2)._drop(-1)", 2),
        ("sequence(0, 1, 2)._drop(4)", 1),
        ("sequence('ignored')._take(2, sequence(0, 1, 2))", 2),
        ("sequence('ignored')._drop(2, sequence(0, 1, 2))", 1),
        ("new Pattern(state => pure('x').query(state))._take(1)", 0),
        ("new Pattern(state => pure('x').query(state))._drop(1)", 0),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct raw {source}: {error}"));
            let pattern = rt.active_pattern().expect("active raw take/drop graph");
            assert!(pattern.is_pure(), "{source}: eager raw graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: eager raw graph retained a callback"
            );
            pattern
        }; // The constructor heap and any pruned receiver callback are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            expected_haps,
            "{source}: raw graph changed after runtime teardown"
        );
    }

    for (name, property) in [("_take", "zoom"), ("_drop", "take")] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawTakeDropOwned = {{ name: '{name}', marker: 89 }};
                    globalThis.rawTakeDropGetterCalls = 0;
                    globalThis.rawTakeDropMethodCalls = 0;
                    globalThis.rawTakeDropQueryCalls = 0;
                    globalThis.rawTakeDropReceiver = sequence('receiver');
                    Object.defineProperty(rawTakeDropReceiver, '{property}', {{
                      configurable: true,
                      get() {{
                        rawTakeDropGetterCalls++;
                        return function () {{
                          if (this !== rawTakeDropReceiver) {{
                            throw new Error('raw dynamic receiver changed');
                          }}
                          rawTakeDropMethodCalls++;
                          return new Pattern(state => {{
                            rawTakeDropQueryCalls++;
                            return pure(rawTakeDropOwned).query(state);
                          }});
                        }};
                      }},
                    }});
                    globalThis.rawTakeDropPattern =
                      rawTakeDropReceiver.{name}([1, 2]);
                    Object.defineProperty(rawTakeDropReceiver, '{property}', {{
                      configurable: true,
                      value() {{ throw new Error('late raw method reread'); }},
                    }});
                    return rawTakeDropPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {name}: {error}"));
        let active = rt.active_pattern().expect("active JS-owned raw take/drop");
        assert!(
            !active.is_pure(),
            "{name}: JS-owned override result was pure"
        );
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "{name}: override result retained no JavaScript ownership root"
        );
        assert_eq!(rt.get_number("rawTakeDropGetterCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawTakeDropMethodCalls"), Some(1.0));
        assert_eq!(rt.get_number("rawTakeDropQueryCalls"), Some(0.0));

        for turn in 1..=3 {
            rt.run_gc();
            rt.run_gc();
            rt.evaluate_score(
                r#"
                  (() => {
                    const state = { span: { begin: 0, end: 1 }, controls: {} };
                    const values = rawTakeDropPattern.query(state).map(hap => hap.value);
                    globalThis.rawTakeDropIdentityProof = Number(
                      values.length === 1
                        && values[0] === rawTakeDropOwned
                        && rawTakeDropOwned.marker === 89
                    );
                    return rawTakeDropPattern;
                  })();
                "#,
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("inspect JS-owned {name}: {error}"));
            assert_eq!(rt.get_number("rawTakeDropIdentityProof"), Some(1.0));
            assert_eq!(rt.get_number("rawTakeDropGetterCalls"), Some(1.0));
            assert_eq!(rt.get_number("rawTakeDropMethodCalls"), Some(1.0));
            assert_eq!(
                rt.get_number("rawTakeDropQueryCalls"),
                Some(f64::from(turn)),
                "{name}: query callback count changed after GC"
            );
        }
    }
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
    assert_eq!(take.steps, Some(Fraction::ONE));

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
    assert_eq!(drop.steps, Some(Fraction::int(2)));
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

/// Canonical scalar and pair `shrink`/`grow`/`s_taper` bodies finish eagerly.
/// Their native zoom/stepcat graphs must therefore remain usable after the
/// QuickJS heap that assembled them is gone. The no-step cases are the
/// negative ownership witness: their JavaScript receiver callback is
/// unreachable after the scalar body returns `nothing`.
#[test]
fn scalar_and_pair_shrink_grow_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expect_haps) in [
        ("sequence(0, 1, 2, 3).shrink(1)", true),
        ("shrink(1, sequence(0, 1, 2, 3))", true),
        ("shrink(1)(sequence(0, 1, 2, 3))", true),
        ("sequence(0, 1, 2, 3).shrink([1, 2])", true),
        ("shrink([1, 2], sequence(0, 1, 2, 3))", true),
        ("shrink([1, 2])(sequence(0, 1, 2, 3))", true),
        ("sequence(0, 1, 2, 3).shrink(0)", true),
        ("sequence(0, 1, 2, 3).shrink(-1)", true),
        ("sequence(0, 1, 2, 3).shrink(0.5)", true),
        ("sequence(0, 1, 2, 3).shrink(5)", true),
        ("sequence(0, 1, 2, 3).grow(1)", true),
        ("grow(1, sequence(0, 1, 2, 3))", true),
        ("grow(1)(sequence(0, 1, 2, 3))", true),
        ("sequence(0, 1, 2, 3).grow([1, 2])", true),
        ("grow([1, 2], sequence(0, 1, 2, 3))", true),
        ("grow([1, 2])(sequence(0, 1, 2, 3))", true),
        ("sequence(0, 1, 2, 3).grow(0)", true),
        ("sequence(0, 1, 2, 3).grow(-1)", true),
        ("sequence(0, 1, 2, 3).grow(-0.5)", true),
        ("sequence(0, 1, 2, 3).grow(5)", true),
        ("sequence(0, 1, 2, 3).s_taper([1, 2])", true),
        ("s_taper([1, 2], sequence(0, 1, 2, 3))", true),
        ("s_taper([1, 2])(sequence(0, 1, 2, 3))", true),
        (
            "new Pattern(state => pure('x').query(state)).shrink(1)",
            false,
        ),
        (
            "new Pattern(state => pure('x').query(state)).grow(1)",
            false,
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active shrink/grow graph");
            assert!(pattern.is_pure(), "{source}: native graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free graph retained a callback"
            );
            pattern
        }; // The originating QuickJS runtime and callback cells are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            !pure.query_arc(Fraction::ZERO, Fraction::ONE).is_empty(),
            expect_haps,
            "{source}: host-free result changed after runtime teardown"
        );
    }
}

/// Raw shrink/grow invoke their registered bodies exactly once during
/// construction. Native-only results must shed the assembling QuickJS heap;
/// an eager custom helper is likewise not retained, while JavaScript-owned
/// Pattern values returned by that helper remain reachable through the graph.
#[test]
fn raw_shrink_grow_are_eager_host_free_and_retain_returned_js_values() {
    for (source, expected_haps) in [
        ("sequence(0, 1, 2, 3)._shrink(1)", 10),
        ("sequence(0, 1, 2, 3)._shrink([1, 2])", 7),
        ("sequence(0, 1, 2, 3)._grow(1)", 10),
        ("sequence(0, 1, 2, 3)._grow([1, 2])", 14),
        ("sequence('ignored')._shrink(2, sequence(0, 1, 2))", 4),
        ("new Pattern(state => pure('x').query(state))._shrink(1)", 0),
        ("new Pattern(state => pure('x').query(state))._grow(1)", 0),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct raw {source}: {error}"));
            let pattern = rt.active_pattern().expect("active raw graph");
            assert!(pattern.is_pure(), "{source}: eager raw graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: eager raw graph retained a callback"
            );
            pattern
        }; // The constructor heap and any discarded no-step callback are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            expected_haps,
            "{source}: raw graph changed after runtime teardown"
        );
    }

    for name in ["_shrink", "_grow"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.rawOwnedFunction = value => `${{value}}!`;
                    globalThis.rawOwnedObject = {{ marker: 97 }};
                    globalThis.rawOwnedHelperCalls = 0;
                    const receiver = sequence('receiver');
                    receiver.shrinklist = function () {{
                      rawOwnedHelperCalls++;
                      return [pure(rawOwnedFunction), pure(rawOwnedObject)];
                    }};
                    globalThis.rawOwnedPattern = receiver.{name}(1);
                    return rawOwnedPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct JS-owned {name}: {error}"));
        let active = rt.active_pattern().expect("active JS-owned raw graph");
        assert!(!active.is_pure(), "{name}: JS-owned result was called pure");
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "{name}: returned JS values retained no ownership root"
        );
        assert_eq!(
            rt.get_number("rawOwnedHelperCalls"),
            Some(1.0),
            "{name}: eager helper call count changed"
        );

        for _ in 0..3 {
            rt.run_gc();
            rt.evaluate_score(
                &format!(
                    r#"
                      (() => {{
                        const state = {{
                          span: {{ begin: 0, end: 1 }}, controls: {{}}
                        }};
                        const values = rawOwnedPattern.query(state)
                          .map(hap => hap.value);
                        const fn = {fn_index};
                        const object = {object_index};
                        if (values.length !== 2
                            || values[fn] !== rawOwnedFunction
                            || values[object] !== rawOwnedObject
                            || values[fn]('kept') !== 'kept!'
                            || values[object].marker !== 97) {{
                          throw new Error('raw returned-list ownership changed');
                        }}
                        if (rawOwnedHelperCalls !== 1) {{
                          throw new Error('raw helper was reread during query');
                        }}
                        return rawOwnedPattern;
                      }})()
                    "#,
                    fn_index = if name == "_grow" { 1 } else { 0 },
                    object_index = if name == "_grow" { 0 } else { 1 },
                ),
                &rustel_transpiler::TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("inspect JS-owned {name} after GC: {error}"));
            assert_eq!(
                rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query JS-owned {name}: {error}"))
                    .len(),
                2
            );
            assert_eq!(rt.get_number("rawOwnedHelperCalls"), Some(1.0));
        }
    }
}

/// Patterned amounts rerun the ordinary `receiver.shrinklist` lookup while
/// querying. The StepJoin graph must retain that receiver, the replacement
/// function, and the JavaScript values returned from it across collections.
#[test]
fn patterned_mutable_shrinklist_dispatch_is_host_dependent_and_gc_owned() {
    for name in ["shrink", "grow", "s_taper"] {
        let rt = JsRuntime::new().expect("runtime");
        rt.install_semantic_bindings().expect("bindings");
        rt.evaluate_score(
            &format!(
                r#"
                  (() => {{
                    globalThis.mutableCanonicalOwned = {{ name: '{name}', marker: 83 }};
                    globalThis.mutableCanonicalFirstCalls = 0;
                    globalThis.mutableCanonicalSecondCalls = 0;
                    globalThis.mutableCanonicalReceiver = sequence('a', 'b', 'c', 'd');
                    mutableCanonicalReceiver.shrinklist = function () {{
                      mutableCanonicalFirstCalls++;
                      return [pure(mutableCanonicalOwned)];
                    }};
                    globalThis.mutableCanonicalPattern =
                      mutableCanonicalReceiver.{name}(sequence(1, 2));
                    return mutableCanonicalPattern;
                  }})()
                "#
            ),
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap_or_else(|error| panic!("construct patterned {name}: {error}"));
        let active = rt.active_pattern().expect("active patterned canonical");
        assert!(
            !active.is_pure(),
            "patterned {name} lost its query-time host dependency"
        );
        assert!(
            !active.reachable_callbacks().is_empty() || active.purity().opaque,
            "patterned {name} retained no JavaScript ownership root"
        );
        assert_eq!(
            rt.get_number("mutableCanonicalFirstCalls"),
            Some(2.0),
            "patterned {name} changed its eager cycle-zero dispatch count"
        );

        rt.evaluate_score(
            r#"
              mutableCanonicalPattern.fmap(value => {
                globalThis.mutableCanonicalIdentityCalls =
                  (globalThis.mutableCanonicalIdentityCalls ?? 0) + 1;
                if (value !== mutableCanonicalOwned) {
                  throw new Error('mutable canonical identity changed');
                }
                return value;
              })
            "#,
            &rustel_transpiler::TranspileOptions::default(),
        )
        .expect("install exact-identity query witness");

        rt.eval(
            r#"
              mutableCanonicalReceiver.shrinklist = function () {
                mutableCanonicalSecondCalls++;
                return [pure(mutableCanonicalOwned)];
              };
            "#,
        )
        .expect("replace instance shrinklist after construction");
        rt.run_gc();
        rt.run_gc();
        assert_eq!(
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .unwrap_or_else(|error| panic!("query patterned {name} after GC: {error}"))
                .len(),
            2
        );
        assert_eq!(rt.get_number("mutableCanonicalIdentityCalls"), Some(2.0));
        assert_eq!(rt.get_number("mutableCanonicalFirstCalls"), Some(2.0));
        assert_eq!(
            rt.get_number("mutableCanonicalSecondCalls"),
            Some(2.0),
            "patterned {name} did not rerun the replacement helper per factor hap"
        );

        rt.run_gc();
        rt.run_gc();
        assert_eq!(
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .unwrap_or_else(|error| panic!("query retained patterned {name}: {error}"))
                .len(),
            2,
            "patterned {name} lost its JS-owned output after GC"
        );
        assert_eq!(rt.get_number("mutableCanonicalSecondCalls"), Some(4.0));
        assert_eq!(rt.get_number("mutableCanonicalIdentityCalls"), Some(4.0));
    }
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
    assert_eq!(shrink.steps, Some(Fraction::int(10)));
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
    assert_eq!(grow.steps, Some(Fraction::int(10)));
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

/// `tour` is a finite native expansion over `stepcat`. Pure receivers and
/// inserted patterns must therefore stay queryable after the QuickJS heap that
/// assembled them is gone; the free/alias wrappers must not accidentally make
/// the resulting graph host-dependent.
#[test]
fn tour_graphs_remain_host_free_after_runtime_teardown() {
    for source in [
        "pure('x').tour()",
        "tour(pure('x'))",
        "s_tour(pure('x'))",
        "pure('x').s_tour()",
        "pure('x').tour(pure('a'))",
        "tour(pure('x'), pure('a'))",
        "s_tour(pure('x'), pure('a'), pure('b'))",
        "sequence(pure('x0'), pure('x1')).tour(\
           pure('a'), sequence(pure('b0'), pure('b1'), pure('b2')))",
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active tour graph");
            assert!(
                pattern.is_pure(),
                "{source}: native tour graph became impure"
            );
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free tour retained a callback"
            );
            pattern
        }; // The originating QuickJS heap and wrapper objects are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert!(
            !pure.query_arc(Fraction::ZERO, Fraction::ONE).is_empty(),
            "{source}: host-free tour produced no haps"
        );
    }
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

/// `stepalt` expands references to already-reified patterns and finishes with
/// the existing native stepcat graph. Pure dense groups must therefore remain
/// host-free after the QuickJS heap that assembled them is gone.
#[test]
fn stepalt_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expected_len) in [
        (
            "stepalt([pure('a'), pure('b')], \
                     [pure('c'), pure('d'), pure('e')])",
            12,
        ),
        (
            "s_alt([pure('a'), pure('b')], \
                   [pure('c'), pure('d'), pure('e')])",
            12,
        ),
        ("new stepalt([pure('a'), pure('b')], [pure('c')])", 4),
        (
            "stepalt([pure('a'), gap(0), pure('b').setSteps(2)], \
                     [pure('x')])",
            5,
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active stepalt graph");
            assert!(pattern.is_pure(), "{source}: stepalt graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free stepalt retained a callback"
            );
            pattern
        }; // The originating QuickJS heap and group arrays are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            expected_len,
            "{source}: host-free stepalt expansion changed"
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

/// Both polymeter algorithms finish as native graphs. Dense Pattern operands
/// use checked-LCM pacing, while a first Array selects the compatibility
/// reifier; neither path may retain the QuickJS heap that assembled pure
/// inputs.
#[test]
fn polymeter_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expected_len) in [
        ("polymeter(sequence('a', 'b'), sequence('x', 'y', 'z'))", 12),
        ("pm(sequence('a', 'b'), sequence('w', 'x', 'y', 'z'))", 8),
        ("s_polymeter(['a', 'b'], ['x', 'y', 'z'])", 4),
        ("new polymeter([['a', 'b'], 'c'], [['x'], ['y', 'z']])", 6),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active polymeter graph");
            assert!(pattern.is_pure(), "{source}: polymeter graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free polymeter retained a callback"
            );
            pattern
        }; // The originating QuickJS heap and source arrays are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            expected_len,
            "{source}: host-free polymeter timing changed"
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

/// `zip` is assembled entirely from the existing native slowcat/fast graph.
/// Its ordinary free wrapper and copied free alias must not make otherwise
/// pure inputs depend on the QuickJS heap that performed score construction.
#[test]
fn zip_graphs_remain_host_free_after_runtime_teardown() {
    for (source, expected_len) in [
        ("zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))", 6),
        (
            "s_zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2', 'b3'))",
            4,
        ),
        (
            "new zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))",
            6,
        ),
        (
            "new s_zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))",
            6,
        ),
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let pattern = rt.active_pattern().expect("active zip graph");
            assert!(pattern.is_pure(), "{source}: zip graph became impure");
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free zip retained a callback"
            );
            pattern
        }; // The originating QuickJS heap and wrapper objects are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            expected_len,
            "{source}: host-free zip timing changed"
        );
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

/// The list helpers return JavaScript Arrays, but composing those temporary
/// arrays with stepcat must leave an entirely native graph when every source
/// value is native. The originating wrapper heap may disappear immediately.
#[test]
fn shrinklist_and_growlist_compositions_remain_host_free_after_runtime_teardown() {
    for source in [
        "stepcat(...sequence('a', 'b', 'c', 'd').shrinklist([1, 3]))",
        "stepcat(...growlist([1, 3], sequence('a', 'b', 'c', 'd')))",
        "stepcat(...s_taperlist([1, 3], sequence('a', 'b', 'c', 'd')))",
    ] {
        let pattern = {
            let rt = JsRuntime::new().expect("runtime");
            rt.install_semantic_bindings().expect("bindings");
            rt.evaluate_score(source, &rustel_transpiler::TranspileOptions::default())
                .unwrap_or_else(|error| panic!("construct {source}: {error}"));
            let pattern = rt.active_pattern().expect("active list composition");
            assert!(
                pattern.is_pure(),
                "{source}: list composition became impure"
            );
            assert!(
                pattern.reachable_callbacks().is_empty(),
                "{source}: host-free list composition retained a callback"
            );
            pattern
        }; // The QuickJS Array, wrappers and originating runtime are gone.

        let pure = pattern
            .as_pure_pattern()
            .unwrap_or_else(|| panic!("{source}: failed to upgrade to PurePattern"));
        assert_eq!(
            pure.query_arc(Fraction::ZERO, Fraction::ONE).len(),
            9,
            "{source}: temporary list composition changed timing"
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
