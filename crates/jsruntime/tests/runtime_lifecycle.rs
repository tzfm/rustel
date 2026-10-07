//! Ownership on the shipping boundary.
//!
//! These tests verify the `JsRuntime` ownership model under repeated
//! evaluation. Whole-runtime teardown only proves the heap is empty after
//! everything is dropped; resources must also remain bounded while a
//! live-coding session is running.
//!
//! The test measures **plateaus during a long run**, per resource,
//! independently - never in aggregate.

use rustel_core::{Pattern, Value, fastcat, live_node_count, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot, cells_dropped, wrappers_dropped};

/// Serialises the resource-measurement tests.
///
/// `live_node_count()` is process-global, so two tests building patterns in
/// parallel perturb each other's plateau measurement. Resource-measurement
/// tests take this lock; everything else runs concurrently.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn measuring<R>(f: impl FnOnce() -> R) -> R {
    let _g = MEASURE.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

fn atom(s: &str) -> Pattern {
    pure(Value::Str(s.into()))
}
fn seq() -> Pattern {
    fastcat(vec![atom("bd"), atom("sd")])
}

const PR_CYCLES: usize = 500;
const NIGHTLY_CYCLES: usize = 10_000;

struct Sample {
    wrappers: u64,
    cells: u64,
    nodes: u64,
    js_objs: usize,
}

/// QuickJS's cycle collector can need more than one pass to reclaim a cycle,
/// so a single `run_gc()` leaves recently-freed graphs uncounted and makes the
/// baseline look inflated.
fn settle(rt: &JsRuntime) {
    for _ in 0..3 {
        rt.run_gc();
    }
}

fn sample(rt: &JsRuntime) -> Sample {
    Sample {
        wrappers: wrappers_dropped(),
        cells: cells_dropped(),
        nodes: live_node_count(),
        js_objs: rt.js_object_count(),
    }
}

/// Build a graph whose JS closure captures its own wrapper, forming the
/// cross-heap cycle: wrapper → cell → closure → wrapper.
fn cyclic_graph(rt: &JsRuntime) -> (rustel_jsruntime::GraphBuilder, Pattern) {
    let mut b = rt.builder();
    // A factory: it receives this graph's own wrapper, and the returned
    // closure captures it. `w` is used in the result, so the capture cannot
    // be elided. An eliminable capture would make this test vacuous.
    let id = b.callback(rt, "(w) => (x) => (w === null ? x : x + '!')");
    (b, seq().fmap_js(id))
}

fn run(cycles: usize) {
    measuring(|| run_inner(cycles))
}

fn run_inner(cycles: usize) {
    let rt = JsRuntime::new().unwrap();

    // --- case A: a graph the user deliberately holds -----------------------
    let (hb, hp) = cyclic_graph(&rt);
    let held_idx = rt.hold(&hb, hp).unwrap();

    // --- case B: an OPAQUE graph, also held --------------------------------
    // Its reachable set is incomplete, so `gc_mark` must mark every cell.
    // If it marked only `reachable_callbacks()`, the callback would be swept
    // and this graph would stop being callable - a use-after-free, not a leak.
    let mut ob = rt.builder();
    let oid = ob.callback(&rt, "(w) => (x) => (w === null ? x : x + '?')");
    let opaque_pat = fastcat(vec![atom("1"), atom("2")])
        .fmap_to_pattern(move |_v| seq().fmap_js(oid))
        .inner_join();
    assert!(
        opaque_pat.purity().opaque,
        "precondition: graph must be opaque"
    );
    let opaque_idx = rt.hold(&ob, opaque_pat).unwrap();

    // Warm up, then take the baseline AFTER the held graphs exist.
    for _ in 0..20 {
        let (b, p) = cyclic_graph(&rt);
        rt.set_active(&b, p).unwrap();
        let _ = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
    }
    settle(&rt);
    let base = sample(&rt);

    // --- churn: replace the active graph `cycles` times --------------------
    for _ in 0..cycles {
        let (b, p) = cyclic_graph(&rt);
        rt.set_active(&b, p).unwrap(); // replaces -> previous is unreachable
        let _ = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
    }
    settle(&rt);
    let after = sample(&rt);

    // --- 1. obsolete resources are actually reclaimed, per resource --------
    let wrappers_freed = after.wrappers - base.wrappers;
    let cells_freed = after.cells - base.cells;
    assert!(
        wrappers_freed as usize >= cycles - 1,
        "obsolete WRAPPERS not reclaimed: {wrappers_freed} freed over {cycles} evaluations"
    );
    assert!(
        cells_freed as usize >= cycles - 1,
        "obsolete CALLBACK CELLS not reclaimed: {cells_freed} freed over {cycles} \
         evaluations. A permanent registry keeps every cell alive and fails here \
         while whole-runtime teardown still passes."
    );

    // --- 2. each resource PLATEAUS independently ---------------------------
    // Not RSS, not a single total: a wrong ownership model typically frees one
    // resource while another grows. Only GROWTH indicates a leak - shrinkage
    // just means the baseline still held collectable warm-up garbage.
    let node_growth = after.nodes as i64 - base.nodes as i64;
    assert!(
        node_growth <= 16,
        "Rust graph did not plateau: live nodes GREW by {node_growth} over \
         {cycles} evaluations (base {} -> {}). Each evaluation leaks ~{:.2} nodes.",
        base.nodes,
        after.nodes,
        node_growth as f64 / cycles as f64
    );
    let js_growth = after.js_objs as i64 - base.js_objs as i64;
    assert!(
        js_growth <= (base.js_objs as i64 / 10).max(64),
        "JS heap did not plateau: obj_count GREW by {js_growth} over {cycles} \
         evaluations (base {} -> {})",
        base.js_objs,
        after.js_objs
    );

    // Report the measurements: a test that only says pass/fail hides the
    // difference between "plateaued at 22" and "plateaued at 22,000".
    println!(
        "runtime lifecycle ({cycles} re-evaluations)\n           wrappers freed: {wrappers_freed}\n  cells freed:    {cells_freed}\n           live nodes:     {} -> {} ({node_growth:+})\n           js obj_count:   {} -> {} ({js_growth:+})",
        base.nodes, after.nodes, base.js_objs, after.js_objs
    );

    // --- 3. the active graph is still callable -----------------------------
    let active = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(active.len(), 2);
    assert!(
        active.iter().all(|h| h.value.show().ends_with('!')),
        "active graph stopped invoking its callback: {:?}",
        active.iter().map(|h| h.value.show()).collect::<Vec<_>>()
    );

    // --- 4. the HELD graph survived every GC -------------------------------
    let held = rt
        .query(Slot::Held, held_idx, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(held.len(), 2, "held graph was collected");
    assert!(
        held.iter().all(|h| h.value.show().ends_with('!')),
        "held graph's callback was swept"
    );

    // --- 5. the OPAQUE graph survived -- conservative marking works --------
    let opaque = rt
        .query(Slot::Held, opaque_idx, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert!(!opaque.is_empty(), "opaque graph produced nothing");
    assert!(
        opaque.iter().all(|h| h.value.show().ends_with('?')),
        "opaque graph's callback was swept - gc_mark did not mark conservatively \
         when the reachable set is incomplete: {:?}",
        opaque.iter().map(|h| h.value.show()).collect::<Vec<_>>()
    );

    // --- 6. clean teardown (the independent whole-heap check) --------------
    rt.clear_active();
    drop(rt);
}

#[test]
fn repeated_evaluation_resources_plateau() {
    run(PR_CYCLES);
}

#[test]
#[ignore = "nightly: 10,000 re-evaluations on the real runtime"]
fn repeated_evaluation_resources_plateau_long_run() {
    run(NIGHTLY_CYCLES);
}

/// There must be no permanent callback registry. Traced cells held in a global
/// array would remain alive for the entire run even if teardown eventually
/// reclaimed them.
#[test]
fn no_permanent_callback_registry() {
    let _g = MEASURE.lock().unwrap_or_else(|e| e.into_inner());
    let rt = JsRuntime::new().unwrap();
    for _ in 0..50 {
        let (b, p) = cyclic_graph(&rt);
        rt.set_active(&b, p).unwrap();
        let _ = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
    }
    rt.run_gc();
    assert!(
        cells_dropped() >= 40,
        "cells are not being reclaimed ({} dropped over 50 evaluations) - a \
         permanent registry has been reintroduced",
        cells_dropped()
    );
    rt.clear_active();
}

// ---------------------------------------------------------------------------
// Additional ownership regressions found after the first lifecycle test.
// ---------------------------------------------------------------------------

/// `__rustel_held` must not be append-only.
///
/// Reassigning `globalThis.p` has to release the previous wrapper; otherwise
/// the held array is a second permanent root and a user who rebinds a variable
/// in a loop leaks exactly as badly as the old global registry did.
#[test]
fn held_slot_replacement_plateaus() {
    measuring(held_slot_replacement_inner);
}

fn held_slot_replacement_inner() {
    let rt = JsRuntime::new().unwrap();
    let (b0, p0) = cyclic_graph(&rt);
    let idx = rt.hold(&b0, p0).unwrap();

    for _ in 0..20 {
        let (b, p) = cyclic_graph(&rt);
        rt.replace_held(idx, &b, p).unwrap();
    }
    settle(&rt);
    let base = sample(&rt);

    const N: usize = if cfg!(debug_assertions) { 500 } else { 10_000 };
    for _ in 0..N {
        let (b, p) = cyclic_graph(&rt);
        rt.replace_held(idx, &b, p).unwrap();
        let _ = rt
            .query(Slot::Held, idx, Fraction::ZERO, Fraction::ONE)
            .unwrap();
    }
    settle(&rt);
    let after = sample(&rt);

    let wrappers_freed = after.wrappers - base.wrappers;
    let cells_freed = after.cells - base.cells;
    let node_growth = after.nodes as i64 - base.nodes as i64;
    let js_growth = after.js_objs as i64 - base.js_objs as i64;
    println!(
        "held replacement ({N})\n  wrappers freed: {wrappers_freed}\n  \
         cells freed:    {cells_freed}\n  live nodes:     {} -> {} ({node_growth:+})\n  \
         js obj_count:   {} -> {} ({js_growth:+})",
        base.nodes, after.nodes, base.js_objs, after.js_objs
    );

    assert!(
        wrappers_freed as usize >= N - 2,
        "replaced HELD wrappers not reclaimed: {wrappers_freed}/{N} - the held \
         array is append-only, i.e. another permanent root"
    );
    assert!(
        cells_freed as usize >= N - 2,
        "replaced held cells not reclaimed"
    );
    assert!(node_growth <= 16, "Rust graph grew by {node_growth}");
    assert!(js_growth <= 64, "JS heap grew by {js_growth}");

    // Still callable after all that churn.
    let haps = rt
        .query(Slot::Held, idx, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(haps.len(), 2);

    // And releasing frees the last one.
    rt.release_held(idx).unwrap();
    settle(&rt);
    assert!(
        cells_dropped() > after.cells,
        "release_held did not free the wrapper"
    );
    rt.clear_active();
}

/// Callback ids are globally unique, not wrapper-local indexes. A graph
/// composed from a held impure pattern and a new one would otherwise share
/// id 0, and one lookup would invoke the other's callback.
#[test]
fn composed_graph_uses_globally_unique_ids() {
    let rt = JsRuntime::new().unwrap();

    // Older graph, held: tags values with '#'.
    let mut hb = rt.builder();
    let old_id = hb.callback(&rt, "(w) => (x) => (w === null ? x : x + '#')");
    let old_pat = seq().fmap_js(old_id);
    let held_idx = rt.hold(&hb, old_pat.clone()).unwrap();

    // Newer graph, composed from the old pattern plus a fresh callback.
    let mut nb = rt.builder();
    nb.import(Slot::Held, held_idx); // must carry the old cell across
    let new_id = nb.callback(&rt, "(w) => (x) => (w === null ? x : x + '@')");
    assert_ne!(old_id, new_id, "ids must be globally unique");

    let composed = rustel_core::stack(vec![old_pat, seq().fmap_js(new_id)]);
    rt.set_active(&nb, composed).unwrap();

    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    let vals: Vec<String> = haps.iter().map(|h| h.value.show()).collect();
    assert_eq!(vals.len(), 4, "both layers must produce haps: {vals:?}");
    assert_eq!(
        vals.iter().filter(|v| v.ends_with('#')).count(),
        2,
        "the OLD callback must still be invoked with its own id: {vals:?}"
    );
    assert_eq!(
        vals.iter().filter(|v| v.ends_with('@')).count(),
        2,
        "the NEW callback must be invoked with its own id: {vals:?}"
    );

    // The composed wrapper owns the imported cell, so releasing the source is safe.
    rt.release_held(held_idx).unwrap();
    settle(&rt);
    let after = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        after.len(),
        4,
        "composed graph broke after its source wrapper was released - the import \
         did not take independent ownership"
    );
    rt.clear_active();
}

/// Composing without importing must be a hard error, never a silent
/// wrong-callback invocation.
#[test]
fn missing_cell_is_a_hard_error() {
    let rt = JsRuntime::new().unwrap();
    let mut hb = rt.builder();
    let old_id = hb.callback(&rt, "(w) => (x) => (w === null ? x : x + '#')");
    let old_pat = seq().fmap_js(old_id);
    let _ = rt.hold(&hb, old_pat.clone()).unwrap();

    // Deliberately no import.
    let nb = rt.builder();
    let err = rt.set_active(&nb, old_pat).unwrap_err();
    assert!(
        err.contains(&format!("callback {old_id}")),
        "expected a missing-cell error naming the id, got: {err}"
    );
}

/// A throwing callback pops the query stack, so no wrapper stays rooted. The
/// query returns no haps, as upstream's `queryArc` does, and does not panic.
#[test]
fn failed_callback_does_not_leave_a_wrapper_rooted() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(w) => (x) => { throw new Error('boom'); }");
    rt.set_active(&b, seq().fmap_js(id)).unwrap();

    assert_eq!(rt.query_depth(), 0, "stack must start empty");
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("a throwing callback is a query error, not a host failure");
    assert!(
        haps.is_empty(),
        "a callback that throws yields NO haps on strudel.cc; got {}",
        haps.len()
    );
    assert_eq!(
        rt.query_depth(),
        0,
        "query stack not unwound after a failing callback - the wrapper stays rooted"
    );
    rt.clear_active();
}

/// Nested queries must not clobber each other's callback table.
///
/// An inner query runs while an outer one is live (a join queries its inner
/// pattern), so a single "currently querying" slot would hand the inner
/// wrapper's cells to the outer graph.
#[test]
fn nested_queries_keep_separate_callback_tables() {
    let rt = JsRuntime::new().unwrap();

    let mut hb = rt.builder();
    let inner_id = hb.callback(&rt, "(w) => (x) => (w === null ? x : x + 'IN')");
    let held_idx = rt.hold(&hb, seq().fmap_js(inner_id)).unwrap();

    let mut ob = rt.builder();
    let outer_id = ob.callback(&rt, "(w) => (x) => (w === null ? x : x + 'OUT')");
    rt.set_active(&ob, seq().fmap_js(outer_id)).unwrap();

    // Query the inner graph from inside the outer query's lifetime by nesting
    // explicitly: depth must return to its prior value afterwards.
    let outer = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert!(outer.iter().all(|h| h.value.show().ends_with("OUT")));
    let inner = rt
        .query(Slot::Held, held_idx, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert!(inner.iter().all(|h| h.value.show().ends_with("IN")));
    // Outer still correct after the inner query ran.
    let outer2 = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert!(
        outer2.iter().all(|h| h.value.show().ends_with("OUT")),
        "outer graph picked up the inner wrapper's callbacks: {:?}",
        outer2.iter().map(|h| h.value.show()).collect::<Vec<_>>()
    );
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
    rt.release_held(held_idx).unwrap();
}

/// Ids are allocated in one place, consecutively and without gaps. The
/// counter is per runtime, so parallel tests in one process add no gaps.
#[test]
fn callback_ids_are_consecutive_and_globally_unique() {
    let rt = JsRuntime::new().unwrap();
    let mut seen = Vec::new();

    for _ in 0..5 {
        let mut b = rt.builder();
        seen.push(b.callback(&rt, "(_w) => (x) => x"));
        seen.push(b.callback(&rt, "(_w) => (x) => x"));
        let first = *seen.get(seen.len() - 2).unwrap();
        rt.set_active(&b, seq().fmap_js(first)).unwrap();
    }

    // Consecutive from zero: installing must consume no ids of its own.
    assert_eq!(
        seen,
        (0..seen.len()).collect::<Vec<_>>(),
        "ids are not consecutive - something other than callback() is allocating"
    );
    assert_eq!(
        rt.alloc_id(),
        seen.len(),
        "next_id advanced past the ids actually handed out"
    );
    rt.clear_active();
}
