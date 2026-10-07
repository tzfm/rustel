/*
rustel-jsruntime - ownership of callback pattern arguments
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Keep callback cells alive when JavaScript retains a pattern argument.
//! Release the original graph and run GC so the retained argument is the only
//! root. Returned-wrapper tests cover results; these cover arguments passed
//! through call_pattern and call_bind.

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

/// Pinned Node's answer for the callback-bearing graph these tests retain:
/// `"bd sd".every(fastcat(2, 3), x => x.fast(2))` over two cycles.
///
/// Both `every` arguments are patterned, so the transformer resolves at query
/// time. The retained graph must reach a live callback on every query.
const BASE_HAPS: &str = "[ 0/1 → 1/4 | bd ];[ 1/4 → 1/2 | sd ];[ 1/2 → 3/4 | bd ];\
[ 3/4 → 1/1 | sd ];[ 1/1 → 3/2 | bd ];[ 3/2 → 2/1 | sd ]";

/// Build the callback-bearing graph and bind it to `globalThis.base`.
fn install_base(rt: &JsRuntime) {
    eval(
        rt,
        r#"globalThis.base = "bd sd".every(fastcat(2, 3), x => x.fast(2));
           base"#,
    );
    assert_eq!(
        shown(rt, 2),
        BASE_HAPS,
        "the base graph does not match pinned Node, so nothing below means \
         anything"
    );
}

/// Drop every root except the retained argument, collect, and make the retained
/// argument the active graph.
///
/// The order matters: `base` is released BEFORE the GC, and the GC runs while
/// only `escaped` refers to the callback. A collection with the original still
/// bound would prove nothing.
fn strand_the_escaped_argument(rt: &JsRuntime) {
    rt.eval("globalThis.base = null;").unwrap();
    rt.clear_active();
    rt.run_gc();
    eval(rt, "escaped");
    rt.run_gc();
}

#[test]
fn a_pattern_argument_retained_by_a_transformer_keeps_its_callbacks() {
    // `call_pattern`. The transformer stashes the pattern it was handed and the
    // result is thrown away, so the ONLY surviving reference to that graph is
    // the JavaScript global the callback wrote.
    let rt = runtime();
    install_base(&rt);
    eval(
        &rt,
        r#"base.every(2, x => { globalThis.escaped = x; return x; })"#,
    );
    strand_the_escaped_argument(&rt);

    assert_eq!(
        shown(&rt, 2),
        BASE_HAPS,
        "the retained transformer argument lost its callback cells: the wrapper \
         handed to the callback carried `CallbackId`s it did not own"
    );
    // Repeatedly, with collections in between - a cell kept alive by luck
    // rather than ownership dies here.
    for round in 0..4 {
        rt.run_gc();
        assert_eq!(
            shown(&rt, 2),
            BASE_HAPS,
            "round {round}: the retained argument stopped resolving"
        );
    }
    assert_scopes_unwound(&rt, "retained transformer argument");
    rt.clear_active();
}

#[test]
fn a_pattern_argument_retained_by_a_bind_keeps_its_callbacks() {
    // `pure(base)` makes the outer hap pattern-valued, so `stepBind`'s
    // callback receives a wrapper. `stepBind` queries cycle zero during
    // construction, so the escape happens in the open evaluation frame.
    let rt = runtime();
    install_base(&rt);
    eval(
        &rt,
        r#"pure(base).stepBind(x => { globalThis.escaped = x; return x; })"#,
    );
    strand_the_escaped_argument(&rt);

    assert_eq!(
        shown(&rt, 2),
        BASE_HAPS,
        "the retained bind argument lost its callback cells"
    );
    for round in 0..4 {
        rt.run_gc();
        assert_eq!(
            shown(&rt, 2),
            BASE_HAPS,
            "round {round}: the retained bind argument stopped resolving"
        );
    }
    assert_scopes_unwound(&rt, "retained bind argument");
    rt.clear_active();
}

#[test]
fn a_retained_argument_from_a_builder_graph_keeps_its_callbacks() {
    // The OTHER wrapper family owns the cells. `with_callback` already accepts
    // either family on the query stack; the argument helper has to read either
    // one too, or a graph held through `hold()` would hand out uncollectable
    // ids.
    let rt = runtime();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(w) => (x) => x + '!'");
    let held = rt
        .hold(
            &b,
            rustel_core::pure(rustel_core::Value::Str("bd".into())).fmap_js(id),
        )
        .unwrap();
    rt.bind_global("p", Slot::Held, held).unwrap();

    eval(
        &rt,
        r#"pure(p).stepBind(x => { globalThis.escaped = x; return x; })"#,
    );
    // Release the held slot as well as the global, so the builder wrapper is
    // genuinely unreachable.
    rt.release_held(held).unwrap();
    rt.eval("globalThis.p = null;").unwrap();
    rt.clear_active();
    rt.run_gc();
    eval(&rt, "escaped");
    rt.run_gc();

    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/1 | bd! ]",
        "a retained argument whose cells belong to a `PatternWrapper` lost them"
    );
    assert_scopes_unwound(&rt, "retained builder argument");
    rt.clear_active();
}

#[test]
fn retained_arguments_do_not_accumulate_cells() {
    // The fix must not become "retain everything for ever". Each escape imports
    // the cells that graph reaches; repeating the escape and dropping the
    // result has to settle rather than climb.
    let rt = runtime();
    install_base(&rt);
    for _ in 0..3 {
        eval(
            &rt,
            r#"base.every(2, x => { globalThis.escaped = x; return x; })"#,
        );
    }
    rt.clear_active();
    rt.run_gc();
    let settled = rustel_jsruntime::cells_live();
    for _ in 0..8 {
        eval(
            &rt,
            r#"base.every(2, x => { globalThis.escaped = x; return x; })"#,
        );
    }
    rt.clear_active();
    rt.run_gc();
    assert!(
        rustel_jsruntime::cells_live() <= settled,
        "escaped arguments accumulate cells: settled at {settled}, now {}",
        rustel_jsruntime::cells_live()
    );
    assert_scopes_unwound(&rt, "repeated escapes");
}

#[test]
fn an_escaped_argument_does_not_root_an_unrelated_graph() {
    // An argument built from a precise graph imports only the ids that graph
    // reaches. Importing the whole querying wrapper's sidecar would retain
    // unrelated callbacks on every escape.
    let rt = runtime();
    rt.run_gc();
    let baseline = rustel_jsruntime::cells_live();

    // A callback-free receiver, so the argument handed to the transformer
    // reaches NO callbacks at all and must import none.
    eval(
        &rt,
        r#""hh cp".every(2, x => { globalThis.escaped = x; return x; })"#,
    );
    rt.clear_active();
    eval(&rt, "escaped");
    rt.run_gc();

    assert_eq!(
        shown(&rt, 1),
        "[ 0/1 → 1/2 | hh ];[ 1/2 → 1/1 | cp ]",
        "the callback-free retained argument is broken"
    );
    rt.clear_active();
    rt.eval("globalThis.escaped = null;").unwrap();
    rt.run_gc();
    assert_eq!(
        rustel_jsruntime::cells_live(),
        baseline,
        "a callback-free escaped argument is still rooting cells"
    );
}
