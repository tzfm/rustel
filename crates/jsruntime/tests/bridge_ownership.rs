//! Check callback-cell ownership, identity and lifetime across the JS bridge:
//! returned wrappers retain nested callbacks, user globals cannot reset IDs,
//! and query-created cells do not accumulate between queries.

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

/// Defect 1, verbatim. Pinned Node returns four haps; this returned none,
/// because the cell for the INNER transformer died with the outer call.
#[test]
fn a_callback_returned_graph_keeps_its_own_callbacks_alive() {
    let rt = runtime();
    eval(
        &rt,
        r#"fastcat("bd", "sd").every(2, x => x.every(fastcat(2, 3), y => y.fast(2)))"#,
    );
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/4 | bd ];[ 1/4 → 1/2 | sd ];[ 1/2 → 3/4 | bd ];[ 3/4 → 1/1 | sd ]",
        "pinned Node's answer; an empty result means the nested cell lost its owner"
    );
    rt.clear_active();
}

/// The same shape with BOTH arguments patterned, so both transformers are
/// chosen at query time and neither can be applied eagerly.
#[test]
fn query_time_nested_callbacks_resolve() {
    let rt = runtime();
    eval(
        &rt,
        r#"fastcat("bd", "sd").every(fastcat(2, 2), x => x.every(fastcat(2, 3), y => y.fast(2)))"#,
    );
    let haps = shown(&rt, 2);
    assert!(
        !haps.is_empty(),
        "a query-time nested callback must still resolve its cell"
    );
    // Stable across repeats: the frame is rebuilt per query, not carried over.
    assert_eq!(haps, shown(&rt, 2), "repeated query changed its answer");
    rt.clear_active();
}

/// `sometimesBy` has an INTERNAL `fmap(...).innerJoin()` even when its
/// probability is scalar. The callback is therefore not an eager register
/// argument: it must survive score construction and GC, then run once for each
/// queried carrier cycle. This also kills a tempting false fix that prebuilds
/// the stack and merely splits its finished graph.
#[test]
fn scalar_sometimes_callback_is_lazy_and_survives_gc() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__sometimesCalls = 0;
           pure('c3').sometimesBy(1, x => {
             globalThis.__sometimesCalls++;
             return x.late(1/4);
           })"#,
    );
    assert_eq!(
        rt.get_number("__sometimesCalls"),
        Some(0.0),
        "pinned scalar fmap is lazy during construction"
    );

    rt.run_gc();
    rt.run_gc();
    let expected = "[ -3/4 ⇜ (0/1 → 1/4) | c3 ];[ (1/4 → 1/1) ⇝ 5/4 | c3 ];[ 1/4 ⇜ (1/1 → 5/4) | c3 ];[ (5/4 → 2/1) ⇝ 9/4 | c3 ]";
    assert_eq!(shown(&rt, 2), expected);
    assert_eq!(rt.get_number("__sometimesCalls"), Some(2.0));

    rt.run_gc();
    rt.run_gc();
    assert_eq!(shown(&rt, 2), expected);
    assert_eq!(rt.get_number("__sometimesCalls"), Some(4.0));
    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
}

#[test]
fn scalar_sometimes_stops_after_the_first_throwing_carrier() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__sometimesThrowCalls = 0;
           pure('c3').sometimesBy(1, () => {
             globalThis.__sometimesThrowCalls++;
             throw new Error('phase sentinel');
           })"#,
    );
    assert_eq!(rt.get_number("__sometimesThrowCalls"), Some(0.0));
    assert!(
        shown(&rt, 2).is_empty(),
        "queryArc catches the callback throw"
    );
    assert_eq!(
        rt.get_number("__sometimesThrowCalls"),
        Some(1.0),
        "pinned fmap unwinds instead of visiting the later carrier cycle"
    );
    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
}

#[test]
fn echo_with_collects_every_raw_result_before_stack_reifies_any() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__echoBatchLog = [];
           setStringParser(value => {
             globalThis.__echoBatchLog.push(`A:${value}`);
             return pure(`A:${value}`);
           });
           const result = pure('seed').echoWith(2, 1/8, (pattern, index) => {
             globalThis.__echoBatchLog.push(`cb${index}`);
             if (index === 1) {
               setStringParser(value => {
                 globalThis.__echoBatchLog.push(`B:${value}`);
                 return pure(`B:${value}`);
               });
             }
             return index === 0 ? 'zero' : 'one';
           });
           globalThis.__echoBatchLogText = globalThis.__echoBatchLog.join(',');
           result"#,
    );
    assert_eq!(
        rt.get_string("__echoBatchLogText").as_deref(),
        Some("cb0,cb1,B:zero,B:one"),
        "strudel.cc finishes map(callback) before stack reifies the first result"
    );
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/1 | B:zero ];[ 0/1 → 1/1 | B:one ]"
    );
    rt.clear_active();
}

#[test]
fn echo_with_passes_number_indices_and_reifies_non_pattern_results() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__echoIndexTypes = [];
           pure(10).echoWith(3, 1/8, (pattern, index) => {
             globalThis.__echoIndexTypes.push(typeof index);
             return index;
           })"#,
    );
    rt.eval("globalThis.__echoIndexTypeText = globalThis.__echoIndexTypes.join(',');")
        .unwrap();
    assert_eq!(
        rt.get_string("__echoIndexTypeText").as_deref(),
        Some("number,number,number")
    );
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/1 | 0 ];[ 0/1 → 1/1 | 1 ];[ 0/1 → 1/1 | 2 ]"
    );
    rt.clear_active();
}

#[test]
fn echo_with_patterned_arguments_are_lazy_repeatable_and_owned() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__echoLazyCalls = 0;
           pure('x').echoWith(fastcat(2, 2), 1/8, (pattern, index) => {
             globalThis.__echoLazyCalls++;
             return pattern;
           })"#,
    );
    assert_eq!(rt.get_number("__echoLazyCalls"), Some(0.0));
    rt.run_gc();
    rt.run_gc();
    let expected = "[ -7/8 ⇜ (0/1 → 1/8) | x ];[ (0/1 → 1/2) ⇝ 1/1 | x ];[ (1/8 → 1/2) ⇝ 9/8 | x ];[ 0/1 ⇜ (1/2 → 1/1) | x ];[ 1/8 ⇜ (1/2 → 1/1) ⇝ 9/8 | x ]";
    assert_eq!(shown(&rt, 1), expected);
    assert_eq!(rt.get_number("__echoLazyCalls"), Some(4.0));
    assert_eq!(shown(&rt, 1), expected);
    assert_eq!(rt.get_number("__echoLazyCalls"), Some(8.0));
    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
}

#[test]
fn echo_with_patterned_throw_discards_the_whole_query_and_restarts_next_time() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__echoLazyThrowCalls = [];
           pure('x').echoWith(fastcat(2, 2), 1/8, (pattern, index) => {
             globalThis.__echoLazyThrowCalls.push(index);
             if (index === 1) throw new Error('lazy echo sentinel');
             return pattern;
           })"#,
    );
    rt.eval("globalThis.__echoLazyThrowText = globalThis.__echoLazyThrowCalls.join(',');")
        .unwrap();
    assert_eq!(rt.get_string("__echoLazyThrowText").as_deref(), Some(""));

    assert!(shown(&rt, 1).is_empty());
    rt.eval("globalThis.__echoLazyThrowText = globalThis.__echoLazyThrowCalls.join(',');")
        .unwrap();
    assert_eq!(
        rt.get_string("__echoLazyThrowText").as_deref(),
        Some("0,1"),
        "the first throw stops the current indexed batch and every later carrier"
    );

    assert!(shown(&rt, 1).is_empty());
    rt.eval("globalThis.__echoLazyThrowText = globalThis.__echoLazyThrowCalls.join(',');")
        .unwrap();
    assert_eq!(
        rt.get_string("__echoLazyThrowText").as_deref(),
        Some("0,1,0,1"),
        "a new query clears the prior ordinary callback error and restarts the batch"
    );
    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
}

#[test]
fn echo_with_tagged_callbacks_keep_their_variadic_strudel_shape() {
    let rt = runtime();

    eval(&rt, "pure(10).echoWith(3, 1/8, revv)");
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/1 | 0 ];[ 0/1 → 1/1 | 1 ];[ 0/1 → 1/1 | 2 ]",
        "patternified arity-one revv consumes the index as its operand"
    );

    eval(&rt, "pure(10).echoWith(3, 1/8, fast(2))");
    assert!(
        shown(&rt, 1).is_empty(),
        "the extra indexed argument must reach fast(2)'s pfunc/appLeft failure"
    );

    eval(&rt, "pure(10).echoWith(3, 1/8, rev)");
    assert!(
        shown(&rt, 1).contains("| 10 ]"),
        "unpatternified rev ignores the extra index and transforms the delayed input"
    );
    rt.clear_active();
}

#[test]
fn eager_echo_throw_rethrows_the_exact_javascript_value_and_stops_the_batch() {
    let rt = runtime();
    eval(
        &rt,
        r#"globalThis.__echoThrowLog = [];
           globalThis.__echoSentinel = new Error('echo sentinel');
           globalThis.__echoCaughtSame = false;
           try {
             pure('x').echoWith(4, 1/8, (pattern, index) => {
               globalThis.__echoThrowLog.push(index);
               if (index === 1) throw globalThis.__echoSentinel;
               return pattern;
             });
           } catch (error) {
             globalThis.__echoCaughtSame = error === globalThis.__echoSentinel;
           }
           globalThis.__echoThrowState = `${globalThis.__echoCaughtSame}:${globalThis.__echoThrowLog.join(',')}`;
           pure('recovered')"#,
    );
    assert_eq!(
        rt.get_string("__echoThrowState").as_deref(),
        Some("true:0,1"),
        "Array.map stops at the first throw and the enclosing JS catch sees the same Error"
    );
    assert_eq!(shown(&rt, 1), "[ 0/1 → 1/1 | recovered ]");
    rt.clear_active();
}

/// Both arguments are patterned, so the inner transformer is resolved per hap
/// at query time and creates a fresh cell on every query. The test verifies
/// creation, plateau, and scratch teardown.
#[test]
fn query_created_cells_are_allocated_and_reclaimed() {
    let rt = runtime();
    eval(
        &rt,
        r#"fastcat("bd", "sd").every(fastcat(2, 3), x => x.every(fastcat(2, 3), y => y.fast(2)))"#,
    );

    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);

    let created_before = rustel_jsruntime::cells_created();
    let first = shown(&rt, 2);
    assert_eq!(first, NODE_NESTED, "pinned Node's exact haps");
    assert!(
        rustel_jsruntime::cells_created() > created_before,
        "this shape must allocate callback cells DURING query; it allocated none, \
         so the lifetime assertions below would be vacuous"
    );

    for _ in 0..60 {
        assert_eq!(shown(&rt, 2), NODE_NESTED, "answer drifted across queries");
    }
    rt.run_gc();
    let settled_live = rustel_jsruntime::cells_live();
    let settled_objects = rt.js_object_count();

    for _ in 0..60 {
        let _ = shown(&rt, 2);
    }
    rt.run_gc();

    assert_eq!(
        rustel_jsruntime::bridge_scratch_len(),
        0,
        "query-created cells leaked into the scratch"
    );
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    assert!(
        rustel_jsruntime::cells_live() <= settled_live,
        "live cell count grew across identical queries: {} -> {}",
        settled_live,
        rustel_jsruntime::cells_live()
    );
    assert!(
        rt.js_object_count() <= settled_objects,
        "JS object count grew across identical queries: {} -> {}",
        settled_objects,
        rt.js_object_count()
    );
    rt.clear_active();
}

/// The same lifetime guarantee when the callback THROWS on every query.
#[test]
fn throwing_queries_reclaim_their_cells_too() {
    let rt = runtime();
    eval(
        &rt,
        r#"fastcat("bd", "sd").every(fastcat(2, 3), x => { throw new Error("boom"); })"#,
    );
    for _ in 0..40 {
        assert!(
            shown(&rt, 2).is_empty(),
            "a throwing callback yields no haps on strudel.cc"
        );
    }
    rt.run_gc();
    let settled_live = rustel_jsruntime::cells_live();
    for _ in 0..40 {
        let _ = shown(&rt, 2);
    }
    rt.run_gc();
    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
    assert_eq!(rustel_jsruntime::bridge_frame_depth(), 0);
    assert!(
        rustel_jsruntime::cells_live() <= settled_live,
        "cells leak when every query throws: {} -> {}",
        settled_live,
        rustel_jsruntime::cells_live()
    );
    rt.clear_active();
}

/// A callback-bearing TEMPORARY built and discarded inside one evaluation must
/// not stay rooted merely because the frame harvested it.
///
/// GC must run while the active graph still owns the surviving wrapper;
/// clearing it first would release both correct and incorrectly retained
/// callbacks before the count is taken.
#[test]
fn a_discarded_callback_temporary_is_not_rooted_by_the_survivor() {
    let rt = runtime();
    rt.run_gc();
    let baseline_live = rustel_jsruntime::cells_live();
    let created_before = rustel_jsruntime::cells_created();

    // The discarded statement puts a cell into the frame's harvest set: the
    // inner `every(fastcat(2, 3), ...)` resolves at query time. The result is
    // never bound, so the callback-free second graph must not get that cell.
    eval(
        &rt,
        r#"fastcat("bd", "sd").every(2, x => x.every(fastcat(2, 3), y => y.fast(2)));
           fastcat("hh", "cp").fast(2)"#,
    );
    assert!(
        rustel_jsruntime::cells_created() > created_before,
        "the discarded statement must actually create a callback cell, or this \
         test asserts nothing"
    );

    // GC with the survivor STILL INSTALLED: this is what distinguishes a
    // correctly-filtered transfer from one that harvested the whole frame.
    rt.run_gc();
    assert_eq!(
        rustel_jsruntime::cells_live(),
        baseline_live,
        "the discarded temporary's cell is still rooted while the active graph holds the \
         survivor; `derive_wrapper` is retaining every frame cell instead of \
         only those the surviving graph can reach"
    );

    // ...and the survivor is intact, so the filter did not over-prune.
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/4 | hh ];[ 1/4 → 1/2 | cp ];[ 1/2 → 3/4 | hh ];[ 3/4 → 1/1 | cp ]",
        "the surviving graph lost haps: the reachability filter pruned too much"
    );

    rt.clear_active();
}

/// Two query-time callback graphs live in one composed result, and the score
/// resets the old allocator name between them. The ids must not collide: the
/// haps are compared with pinned Node output, so a swapped transformer fails.
#[test]
fn resetting_the_old_allocator_name_cannot_collide_two_live_graphs() {
    let rt = runtime();
    rt.eval(
        "globalThis.__rustel_next_id = 0; \
         globalThis.__rustel_pending = [];",
    )
    .unwrap();
    eval(
        &rt,
        r#"const a = fastcat("bd", "sd").every(fastcat(2, 3), x => x.fast(2));
           globalThis.__rustel_next_id = 0;
           const b = fastcat("hh", "cp").every(fastcat(2, 3), x => x.slow(2));
           stack(a, b)"#,
    );
    assert_eq!(
        shown(&rt, 2),
        NODE_COLLISION,
        "pinned Node's exact haps; a mismatch means one branch ran the other's \
         transformer"
    );
    rt.clear_active();
}

/// Pinned Node, `stack(a, b)` above, cycles 0..2.
const NODE_COLLISION: &str = "[ 0/1 → 1/4 | bd ];[ (0/1 → 1/2) ⇝ 1/1 | hh ];[ 1/4 → 1/2 | sd ];[ 1/2 → 3/4 | bd ];[ 0/1 ⇜ (1/2 → 1/1) | hh ];[ 3/4 → 1/1 | sd ];[ 1/1 → 3/2 | bd ];[ 1/1 → 3/2 | hh ];[ 3/2 → 2/1 | sd ];[ 3/2 → 2/1 | cp ]";

/// Pinned Node, the nested query-time form, cycles 0..2.
const NODE_NESTED: &str = "[ 0/1 → 1/4 | bd ];[ 1/4 → 1/2 | sd ];[ 1/2 → 3/4 | bd ];[ 3/4 → 1/1 | sd ];[ 1/1 → 3/2 | bd ];[ 3/2 → 2/1 | sd ]";

/// Ids must be unique across BOTH wrapper families in one runtime - the
/// builder's `PatternWrapper` and the host surface's `NativePatternWrapper`
/// share one allocator.
#[test]
fn ids_are_unique_across_both_wrapper_families() {
    let rt = runtime();
    let before = rt.alloc_id();

    // Host-surface path: creates bridge cells.
    eval(&rt, r#"fastcat("bd", "sd").every(2, x => x.fast(2))"#);
    let after_host = rt.alloc_id();
    assert!(
        after_host > before,
        "the host surface did not draw from the shared allocator"
    );

    // Builder path: must not restart from zero.
    let mut b = rt.builder();
    let builder_id = b.callback(&rt, "(_w) => (x) => x");
    assert!(
        builder_id > after_host,
        "builder id {builder_id} collides with host-surface ids (<= {after_host})"
    );
    rt.clear_active();
}

/// An eager transformer callback that throws fails the evaluation: numeric
/// `every` invokes its callback during construction. The failed evaluation
/// unwinds the query scope and the bridge frame.
#[test]
fn a_throwing_callback_unwinds_every_scope() {
    let rt = runtime();
    let error = rt
        .evaluate_score(
            r#"fastcat("bd", "sd").every(1, x => { throw new Error("boom"); })"#,
            &TranspileOptions::default(),
        )
        .expect_err("an eager callback that throws must fail the evaluation");
    assert_eq!(
        error.to_string(),
        "Error: boom - line 1",
        "the evaluation error must be the callback's own throw"
    );
    assert_eq!(rt.query_depth(), 0, "query stack not unwound");
    assert_eq!(
        rustel_jsruntime::bridge_frame_depth(),
        0,
        "bridge frame not unwound"
    );
    assert_eq!(rustel_jsruntime::bridge_scratch_len(), 0);
}

/// An eager transformer throw is a JavaScript exception at the call that
/// invoked the callback, as with upstream's synchronous `func(pat)`. The
/// score's `try`/`catch` sees the exact value; an uncaught throw stops it.
#[test]
fn an_eager_callback_throw_is_an_exception_at_the_calling_method() {
    for call in [
        "s('bd sd').every(2, T)",
        "every(2, T, s('bd sd'))",
        "s('bd sd').jux(T)",
        "s('bd sd').off(0.25, T)",
        "s('bd sd').superimpose(T)",
        "s('bd sd').layer(T)",
    ] {
        let rt = runtime();
        eval(
            &rt,
            &format!(
                "globalThis.__sentinel = new Error('eager sentinel');
                 const T = x => {{ throw globalThis.__sentinel; }};
                 globalThis.__caught = 'nothing';
                 try {{ {call}; }} catch (error) {{
                   globalThis.__caught = error === globalThis.__sentinel ? 'same' : String(error);
                 }}
                 pure('recovered')"
            ),
        );
        assert_eq!(
            rt.get_string("__caught").as_deref(),
            Some("same"),
            "{call}: the score's catch must see the callback's exact throw"
        );
        assert_eq!(
            shown(&rt, 1),
            "[ 0/1 → 1/1 | recovered ]",
            "{call}: a caught throw leaves the score to go on"
        );
        rt.clear_active();

        let error = rt
            .evaluate_score(
                &format!(
                    "const T = x => {{ throw new Error('uncaught'); }};
                     {call};
                     globalThis.__after = 1;
                     pure('never')"
                ),
                &TranspileOptions::default(),
            )
            .expect_err("an uncaught eager throw fails the evaluation");
        assert!(
            error.to_string().starts_with("Error: uncaught"),
            "{call}: {error}"
        );
        assert_eq!(
            rt.get_number("__after"),
            None,
            "{call}: the score kept running past the throwing call"
        );
    }
}

/// A score's OWN construction-time query of a pattern whose callback throws
/// is the score's business: strudel.cc's `queryArc` catches the throw and
/// answers silence, and the score evaluates. Only a callback the score's
/// method call itself ran eagerly fails the evaluation.
#[test]
fn a_construction_time_query_of_a_throwing_pattern_leaves_the_score_standing() {
    for thrower in [
        "note('c4 e4').fmap(x => { throw new Error('boom-in-query') })",
        // Patterned first arguments resolve the transformer at QUERY time,
        // through both the unary and the indexed callback paths.
        "s('bd sd').every(fastcat(1, 1), x => { throw new Error('boom-in-query') })",
        "s('bd sd').echoWith(fastcat(2, 2), 1/8, x => { throw new Error('boom-in-query') })",
    ] {
        let rt = runtime();
        eval(
            &rt,
            &format!(
                "const thrower = {thrower};
                 thrower.queryArc(0, 1);
                 thrower.query(new State(new TimeSpan(0, 1)));
                 globalThis.__queried = 1;
                 s('bd').every(2, x => x.fast(2))"
            ),
        );
        assert_eq!(rt.get_number("__queried"), Some(1.0), "{thrower}");
        assert!(
            !shown(&rt, 1).is_empty(),
            "{thrower}: the score after the query must install and play"
        );
        rt.clear_active();
    }

    let rt = runtime();
    let error = rt
        .evaluate_score(
            "s('bd sd').every(2, x => { throw new Error('boom-at-construction') })",
            &TranspileOptions::default(),
        )
        .expect_err("a numeric every's eager throw still fails the evaluation");
    assert!(
        error.to_string().contains("boom-at-construction"),
        "{error}"
    );
}

/// The eager throw reports the SCORE's line, like every other evaluation
/// error: the studio and `rustel check` jump to it, and the wrapped script's
/// line is one off.
#[test]
fn an_eager_callback_throw_names_the_scores_own_line() {
    let rt = runtime();
    let error = rt
        .evaluate_score(
            "s('bd sd')\n  .every(2,\n    x => { throw new Error('third line') })\n",
            &TranspileOptions::default(),
        )
        .expect_err("the eager throw fails the evaluation");
    assert_eq!(error.to_string(), "Error: third line - line 3");

    // The async wrapper holds one more line above the score.
    let error = rt
        .evaluate_score(
            "await Promise.resolve(0)\ns('bd sd')\n  .every(2, x => { throw new Error('third line') })\n",
            &TranspileOptions::default(),
        )
        .expect_err("the eager throw fails the async evaluation");
    assert_eq!(error.to_string(), "Error: third line - line 3");

    // An `all(...)` transform runs after the score, when the lanes finish.
    let error = rt
        .evaluate_score(
            "$: s('bd sd')\n\nall(x => x.every(2, y => { throw new Error('third line') }))\n",
            &TranspileOptions::default(),
        )
        .expect_err("the eager throw fails the lanes' finish");
    assert_eq!(error.to_string(), "Error: third line - line 3");
}

/// `stepJoin`/`stepBind` query cycle 0 during construction. A throw from a
/// transformer that this probe reaches belongs to the probe's boundary
/// (upstream's `queryArc` catches it), not to the score's evaluation or a
/// later registered call.
#[test]
fn a_construction_probe_of_a_lazy_throwing_transformer_is_not_an_eager_throw() {
    for probed in [
        r#"s("bd sd").every("<1 2>", x => { throw new Error("boom") }).stepBind(v => pure(v))"#,
        r#"s("bd sd").every(fastcat(1, 1), x => { throw new Error("boom") }).stepBind(v => pure(v))"#,
        r#"s("bd sd").every("<1 2>", x => { throw new Error("boom") }).fmap(v => pure(v)).stepJoin()"#,
    ] {
        let rt = runtime();
        eval(
            &rt,
            &format!(
                "const probed = {probed};
                 globalThis.__caught = 'nothing';
                 let later = probed;
                 try {{ later = probed.fast(2); }} catch (error) {{ globalThis.__caught = String(error); }}
                 later"
            ),
        );
        assert_eq!(
            rt.get_string("__caught").as_deref(),
            Some("nothing"),
            "{probed}: a later registered call rethrew the probe's throw"
        );
        assert_eq!(
            shown(&rt, 1),
            "",
            "{probed}: the throw is the query's, which answers silence"
        );
        rt.clear_active();
    }

    // A callback that throws ONLY while the score is constructed - its
    // state is set after the probe - plays exactly as if it never had.
    let rt = runtime();
    eval(
        &rt,
        r#"n("0 2").every("<1 2>", x => x.fast(2)).stepBind(v => pure(v))"#,
    );
    let reference = shown(&rt, 2);
    rt.clear_active();
    eval(
        &rt,
        r#"let state = null;
           const p = n("0 2").every("<1 2>", x => x.fast(state.factor)).stepBind(v => pure(v));
           state = { factor: 2 };
           p"#,
    );
    assert_eq!(shown(&rt, 2), reference);
    assert!(!reference.is_empty());
    rt.clear_active();
}

/// The score sees the first eager throw, and no user callback runs after it,
/// as upstream's synchronous calls unwind at it. A later callback could
/// catch the held throw and let the score install silence.
#[test]
fn the_first_eager_throw_wins_and_stops_the_rest() {
    for method in ["superimpose", "layer"] {
        let rt = runtime();
        eval(
            &rt,
            &format!(
                "globalThis.__first = new Error('first');
                 globalThis.__secondRan = 0;
                 globalThis.__caught = 'nothing';
                 try {{
                   s('bd sd').{method}(
                     x => {{ throw globalThis.__first; }},
                     x => {{ globalThis.__secondRan = 1; throw new Error('second'); }},
                   );
                 }} catch (error) {{
                   globalThis.__caught = error === globalThis.__first ? 'first' : String(error);
                 }}
                 pure('recovered')"
            ),
        );
        assert_eq!(
            rt.get_string("__caught").as_deref(),
            Some("first"),
            "{method}"
        );
        assert_eq!(
            rt.get_number("__secondRan"),
            Some(0.0),
            "{method}: a transformer ran after the first one threw"
        );
        rt.clear_active();
    }

    let rt = runtime();
    eval(
        &rt,
        "globalThis.__calls = 0;
         globalThis.__caught = 'nothing';
         try {
           s('bd sd').applyN(2, x => { throw new Error('call ' + (++globalThis.__calls)); });
         } catch (error) {
           globalThis.__caught = error.message;
         }
         pure('recovered')",
    );
    assert_eq!(rt.get_string("__caught").as_deref(), Some("call 1"));
    assert_eq!(
        rt.get_number("__calls"),
        Some(1.0),
        "applyN called its transformer again after the first throw"
    );
    rt.clear_active();

    // The second call, had it run, would swallow the first call's throw:
    // its `x.fast(2)` rethrows whatever eager throw is held.
    let rt = runtime();
    let error = rt
        .evaluate_score(
            r#"globalThis.__calls = 0;
               s("bd").applyN(2, x => {
                 if (++globalThis.__calls === 1) throw new Error("first");
                 try { return x.fast(2); } catch (error) { return x; }
               })"#,
            &TranspileOptions::default(),
        )
        .expect_err("the first call's throw must fail the evaluation");
    assert!(error.to_string().starts_with("Error: first"), "{error}");
    assert_eq!(rt.get_number("__calls"), Some(1.0));
}

/// A bounded evaluation that queued a job keeps its TYPED refusal when an
/// eager callback also threw: the throw is an ordinary exception, and the
/// queued job outranks an ordinary exception exactly as it does for a
/// score's own `throw`.
#[test]
fn an_eager_callback_throw_keeps_a_pending_job_refusal_typed() {
    let rt = runtime();
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let error = rt
        .evaluate_score_cancellable(
            "Promise.resolve().then(() => { globalThis.__jobRan = 1; });
             s('bd sd').every(2, x => { globalThis.__eagerRan = 1; throw new Error('eager'); })",
            &TranspileOptions::default(),
            std::time::Duration::from_secs(1),
            &cancellation,
        )
        .expect_err("queued work is refused");
    assert!(
        matches!(
            error,
            rustel_jsruntime::QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs)
        ),
        "the eager throw hid the typed pending-job refusal: {error:?}"
    );
    assert_eq!(
        rt.get_number("__eagerRan"),
        Some(1.0),
        "vacuous: no eager call"
    );
    assert_eq!(rt.get_number("__jobRan"), None);
}

/// Negative control: a callback id with no owner anywhere must still be a hard
/// error, not a silent wrong-callback invocation.
#[test]
fn an_unowned_callback_id_is_still_a_hard_error() {
    let rt = runtime();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x");
    // Deliberately install a graph referencing the id WITHOUT importing the
    // wrapper that owns it.
    let nb = rt.builder();
    let err = rt
        .set_active(
            &nb,
            rustel_core::pure(rustel_core::Value::Str("bd".into())).fmap_js(id),
        )
        .unwrap_err();
    assert!(
        err.contains(&format!("callback {id}")),
        "expected a missing-cell error naming the id, got: {err}"
    );
}

/// An interrupted callback leaves no scope and no scratch, and the runtime
/// stays usable. An interrupt unwinds on a different path from a JS `throw`
/// or a Rust error.
#[test]
fn an_interrupted_query_tears_down_every_scope() {
    let rt = runtime();
    eval(
        &rt,
        r#"fastcat("bd", "sd").every(fastcat(2, 3), x => {
             while (true) {}
           })"#,
    );

    let outcome = rt.with_deadline(std::time::Duration::from_millis(50), || {
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::int(2))
    });
    // Either a host error or an empty result is acceptable; what matters is
    // that it TERMINATED and unwound.
    let _ = outcome;
    assert!(rt.was_interrupted(), "interrupt handler did not fire");

    assert_eq!(
        rt.query_depth(),
        0,
        "query stack not unwound after interrupt"
    );
    assert_eq!(
        rustel_jsruntime::bridge_frame_depth(),
        0,
        "bridge frame not unwound after interrupt"
    );
    assert_eq!(
        rustel_jsruntime::bridge_scratch_len(),
        0,
        "bridge scratch survived an interrupt"
    );

    // ...and the runtime is still usable afterwards.
    rt.clear_active();
    eval(&rt, r#"fastcat("hh", "cp").fast(2)"#);
    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/4 | hh ];[ 1/4 → 1/2 | cp ];[ 1/2 → 3/4 | hh ];[ 3/4 → 1/1 | cp ]",
        "runtime unusable after an interrupted query"
    );
    rt.clear_active();
}
