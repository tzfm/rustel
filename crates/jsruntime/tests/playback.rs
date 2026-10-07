//! Check scheduling and JS state while one thread queries and evaluates.
//! Candidate evaluation must preserve onsets, Stop and generation isolation.
//! Closures, handle identity, instanceof and partial side effects must survive
//! evaluation; replacing the JS heap would violate those contracts.

use rustel_core::{Pattern, Value, fastcat, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn atom(s: &str) -> Pattern {
    pure(Value::Str(s.into()))
}
fn seq() -> Pattern {
    fastcat(vec![atom("bd"), atom("sd")])
}

// ---------------------------------------------------------------------------
// 1. Genuine reentrancy - the first test, because everything else assumes it.
// ---------------------------------------------------------------------------

/// A JS callback queries a *different* pattern while the outer query is still
/// active. Two wrappers are live on the stack simultaneously.
///
/// Sequential queries would pass against a single "currently querying" slot;
/// this cannot. If the inner query clobbered the slot, the outer graph would
/// resume against the inner wrapper's cells and either fail to find its
/// callback or invoke the wrong one.
#[test]
fn reentrant_query_from_inside_a_callback() {
    let rt = JsRuntime::new().unwrap();
    rt.install_query_binding().unwrap();

    // Inner graph, held: tags with 'IN'.
    let mut ib = rt.builder();
    let inner_id = ib.callback(&rt, "(_w) => (x) => x + 'IN'");
    let inner_idx = rt.hold(&ib, seq().fmap_js(inner_id)).unwrap();

    // Outer graph: its callback queries the inner graph MID-QUERY, and folds
    // the result into its own value, so the nesting is observable.
    let mut ob = rt.builder();
    let outer_id = ob.callback(
        &rt,
        &format!(
            "(_w) => (x) => {{ \
               if (globalThis.__depth_seen === undefined) globalThis.__depth_seen = 0; \
               const inner = queryHeld({inner_idx}, 0, 1); \
               globalThis.__inner_result = inner; \
               return x + '<' + inner + '>'; \
             }}"
        ),
    );
    rt.set_active(&ob, seq().fmap_js(outer_id)).unwrap();

    assert_eq!(rt.query_depth(), 0, "stack must start empty");
    let outer = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();

    // The outer callback ran, and inside it the inner graph was queried.
    let vals: Vec<String> = outer.iter().map(|h| h.value.show()).collect();
    assert_eq!(vals.len(), 2, "outer graph produced: {vals:?}");
    assert!(
        vals.iter()
            .all(|v| v.contains("bdIN") && v.contains("sdIN")),
        "the inner graph's callback did not run inside the outer query: {vals:?}"
    );
    assert!(
        vals[0].starts_with("bd<") && vals[1].starts_with("sd<"),
        "the OUTER graph resumed against the wrong callback table: {vals:?}"
    );

    assert_eq!(rt.query_depth(), 0, "stack not fully unwound after nesting");
    rt.clear_active();
    rt.release_held(inner_idx).unwrap();
}

// ---------------------------------------------------------------------------
// 2. The evaluation deadline
// ---------------------------------------------------------------------------

/// `while (true)` must be interrupted, not hang.
#[test]
fn infinite_synchronous_loop_is_interrupted() {
    let rt = JsRuntime::new().unwrap();
    let start = std::time::Instant::now();
    let r = rt.with_deadline(Duration::from_millis(50), || {
        rt.eval("globalThis.spun = 0; while (true) { globalThis.spun++; }")
    });
    assert!(r.is_err(), "runaway loop should have been interrupted");
    assert!(rt.was_interrupted(), "interrupt handler did not fire");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "interrupt took too long: {:?}",
        start.elapsed()
    );
}

/// **Partial side effects of an interrupted evaluation persist.** There is
/// one JS context and evaluation mutates globals in place; if it throws
/// halfway, what already happened stays, matching strudel.cc. Rolling back
/// would be a divergence, not an improvement.
#[test]
fn partial_side_effects_persist_after_interruption() {
    let rt = JsRuntime::new().unwrap();
    rt.eval("globalThis.before = 0; globalThis.after = 0;")
        .unwrap();

    let r = rt.with_deadline(Duration::from_millis(50), || {
        rt.eval("globalThis.before = 1; while (true) {} globalThis.after = 1;")
    });
    assert!(r.is_err());

    assert_eq!(
        rt.get_number("before"),
        Some(1.0),
        "a side effect that happened BEFORE the interrupt must persist - \
         strudel.cc does not roll back"
    );
    assert_eq!(
        rt.get_number("after"),
        Some(0.0),
        "a side effect after the interrupt point must not have happened"
    );
}

/// The runtime must still be usable after an interrupted evaluation - the
/// active graph keeps playing.
#[test]
fn playback_survives_an_interrupted_evaluation() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x + '!'");
    rt.set_active(&b, seq().fmap_js(id)).unwrap();

    let before = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(before.len(), 2);

    let _ = rt.with_deadline(Duration::from_millis(50), || rt.eval("while (true) {}"));

    let after = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        after.iter().map(|h| h.value.show()).collect::<Vec<_>>(),
        before.iter().map(|h| h.value.show()).collect::<Vec<_>>(),
        "the playing graph stopped working after a candidate evaluation was \
         interrupted"
    );
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
}

/// The query stack unwinds without running JavaScript. After the deadline
/// fires, the interrupt handler refuses every JS call, so a pop that
/// evaluates JS would fail and leave the wrappers on the stack.
#[test]
fn query_stack_unwinds_while_the_interrupt_is_firing() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x + '!'");
    rt.set_active(&b, seq().fmap_js(id)).unwrap();

    // Arm a deadline that is ALREADY expired, so every JS call is refused.
    let depth = rt.with_deadline(Duration::from_millis(0), || {
        std::thread::sleep(Duration::from_millis(5));
        let _ = rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
        rt.query_depth()
    });
    assert_eq!(
        depth, 0,
        "query stack did not unwind while the interrupt handler was firing - \
         the pop must not require evaluating JS"
    );
    assert_eq!(rt.query_depth(), 0);
    rt.clear_active();
}

// ---------------------------------------------------------------------------
// 3. State continuity
// ---------------------------------------------------------------------------

/// `let n = 0; register("x", p => p.add(n++))` must keep counting across
/// evaluations. No heap swap can reconstruct this: `n` lives in a closure and
/// QuickJS heaps are not serialisable.
#[test]
fn mutable_closure_state_survives_evaluation() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => { let n = 0; return (x) => `${x}${n++}`; }");
    rt.set_active(&b, seq().fmap_js(id)).unwrap();

    let first: Vec<String> = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .iter()
        .map(|h| h.value.show())
        .collect();
    assert_eq!(first, vec!["bd0", "sd1"]);

    // A separate evaluation happens in between - the counter must not reset.
    rt.eval("globalThis.unrelated = 42;").unwrap();

    let second: Vec<String> = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .iter()
        .map(|h| h.value.show())
        .collect();
    assert_eq!(
        second,
        vec!["bd2", "sd3"],
        "closure counter reset - a runtime swap would do exactly this"
    );
    rt.clear_active();
}

/// A held **pattern handle** must remain the same object across evaluations,
/// keep its `instanceof`, and stay queryable.
///
/// The object under test is the actual `PatternWrapper`, whose lifetime the
/// ownership model governs, rather than an unrelated plain JavaScript object.
#[test]
fn held_pattern_handle_identity_and_instanceof_survive() {
    let rt = JsRuntime::new().unwrap();

    let mut hb = rt.builder();
    let held_id = hb.callback(&rt, "(_w) => (x) => x + 'H'");
    let held_idx = rt.hold(&hb, seq().fmap_js(held_id)).unwrap();

    // `globalThis.p = <the pattern>` - the real user-facing case.
    rt.bind_global("p", Slot::Held, held_idx).unwrap();
    rt.eval("globalThis.pRef = globalThis.p; globalThis.PatternCtor = globalThis.p.constructor;")
        .unwrap();

    for _ in 0..25 {
        let mut b = rt.builder();
        let id = b.callback(&rt, "(_w) => (x) => x");
        rt.set_active(&b, seq().fmap_js(id)).unwrap();
        let _ = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
    }
    rt.run_gc();
    rt.run_gc();

    rt.eval("globalThis.sameObject = (globalThis.p === globalThis.pRef) ? 1 : 0;")
        .unwrap();
    assert_eq!(
        rt.get_number("sameObject"),
        Some(1.0),
        "the held PATTERN handle changed identity across evaluations"
    );

    rt.eval(
        "globalThis.stillInstance = \
         (globalThis.p instanceof globalThis.PatternCtor) ? 1 : 0;",
    )
    .unwrap();
    assert_eq!(
        rt.get_number("stillInstance"),
        Some(1.0),
        "instanceof stopped holding for the held pattern handle"
    );

    // And it must still WORK - identity without queryability is useless.
    let haps = rt
        .query(Slot::Held, held_idx, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        haps.iter().map(|h| h.value.show()).collect::<Vec<_>>(),
        vec!["bdH", "sdH"],
        "held pattern stopped being queryable after 25 evaluations"
    );
    rt.clear_active();
    rt.release_held(held_idx).unwrap();
}

/// A real `glide`-style pattern: `curr`/`prev`/`lastT` carried across queries,
/// with behaviour that depends on `state.controls._cps`.
///
/// The user-prebake idiom under test:
/// ```js
/// let curr = [], prev = [], lastT = null;
/// const trig = !!state.controls._cps;   // real trigger vs lookahead
/// if (trig && (lastT == null || lastT !== t)) { prev = curr; curr = []; lastT = t; }
/// ```
/// The assertions cover both state rotation and the `_cps` dependency.
#[test]
fn glide_style_state_rotation_and_cps_dependency() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => { \
           let curr = [], prev = [], lastT = null; \
           return (state) => { \
             const t = state.span.begin; \
             const trig = !!state.controls._cps; \
             if (trig && (lastT === null || lastT !== t)) { \
               prev = curr; curr = []; lastT = t; \
             } \
             curr.push(t); \
             return [{ begin: t, end: state.span.end, \
                       value: `trig=${trig?1:0} curr=${curr.length} prev=${prev.length}` }]; \
           }; }",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    // Lookahead queries: no _cps, so no rotation - curr keeps growing.
    let a = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    let c = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(a[0].value.show(), "trig=0 curr=1 prev=0");
    assert_eq!(
        c[0].value.show(),
        "trig=0 curr=2 prev=0",
        "without _cps the pattern must NOT rotate - it is a lookahead query"
    );

    // A real trigger: _cps present and the time changed, so curr rotates into
    // prev and curr restarts.
    let d = rt
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ONE,
            Fraction::int(2),
            &[("_cps", 0.5)],
        )
        .unwrap();
    assert_eq!(
        d[0].value.show(),
        "trig=1 curr=1 prev=2",
        "a real trigger must rotate curr into prev; state did not survive or \
         _cps was not visible"
    );

    // Same time again with _cps: lastT unchanged, so no further rotation.
    let e = rt
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ONE,
            Fraction::int(2),
            &[("_cps", 0.5)],
        )
        .unwrap();
    assert_eq!(
        e[0].value.show(),
        "trig=1 curr=2 prev=2",
        "lastT did not suppress a second rotation at the same time"
    );
    rt.clear_active();
}

/// An interrupted queued callback must consume its pending
/// exception, discard residual jobs, collect, and recover on the same heap.
///
/// rquickjs 0.14 fixes borrowed error-context ownership. Keep this path in a
/// child process because repeated interruption, collection, and same-heap
/// recovery could abort the test runner if the engine regresses. The focused
/// queued-throw test separately requires the exact pending exception to guard
/// exception consumption.
#[test]
fn interrupted_queued_job_consumes_exception_and_recovers_same_heap() {
    let exe = env!("CARGO_BIN_EXE_job_interrupt_recovery");
    let mut child = Command::new(exe)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the interrupted-job recovery control");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll recovery control") {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(10) {
            child.kill().expect("kill hung recovery control");
            let status = child.wait().expect("reap hung recovery control");
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .expect("stderr pipe")
                .read_to_string(&mut stderr)
                .expect("read stderr");
            panic!("interrupted-job recovery hung; status {status:?}; stderr: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert!(
        status.success(),
        "interrupted-job recovery failed: {status:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("JOB_INTERRUPT_RECOVERY turns=32"),
        "the positive control did not complete every recovery turn: {stdout}"
    );
    assert!(stderr.is_empty(), "unexpected child stderr: {stderr}");
}

/// The held array is host bookkeeping, like the active slot: no JavaScript
/// ever reads it, score code only names the pattern that goes in. A score that
/// wipes or shadows a `__rustel_held` homonym must not disturb what the user
/// is holding, and must not get an accessor invoked by host publication.
#[test]
fn a_public_held_homonym_does_not_disturb_the_private_held_graphs() {
    let rt = JsRuntime::new().unwrap();

    let mut hb = rt.builder();
    let held_id = hb.callback(&rt, "(_w) => (x) => x + 'H'");
    let held_idx = rt.hold(&hb, seq().fmap_js(held_id)).unwrap();
    rt.bind_global("p", Slot::Held, held_idx).unwrap();

    rt.eval(
        r#"
          globalThis.heldRootInitiallyAbsent =
            Object.hasOwn(globalThis, '__rustel_held') ? 0 : 1;
          globalThis.heldAccessorHits = 0;
          globalThis.__rustel_held = null;
          Object.defineProperty(globalThis, '__rustel_held', {
            configurable: false,
            get() {
              globalThis.heldAccessorHits++;
              throw new Error('public held getter ran');
            },
          });
        "#,
    )
    .unwrap();
    assert_eq!(
        rt.get_number("heldRootInitiallyAbsent"),
        Some(1.0),
        "the held root is still reachable under a public name"
    );

    // Publishing a new active graph must not touch the homonym either.
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x");
    rt.set_active(&b, seq().fmap_js(id)).unwrap();

    let haps = rt
        .query(Slot::Held, held_idx, Fraction::ZERO, Fraction::ONE)
        .expect("held graph survives a hostile homonym");
    assert!(
        haps.iter().all(|hap| hap.value.show().ends_with('H')),
        "held callback was lost: {haps:?}"
    );
    assert_eq!(
        rt.get_number("heldAccessorHits"),
        Some(0.0),
        "host bookkeeping invoked a score-installed accessor"
    );
}
