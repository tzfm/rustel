//! The boundary between a Rust query and JavaScript: QuickJS callbacks,
//! runtime `register()`, and the budgets that bound score code.
//!
//! A combinator body and user code run inside `queryArc`, so a query is not
//! real-time safe.

use rustel_core::{Pattern, Value, fastcat, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;
use std::time::Duration;

fn atom(s: &str) -> Pattern {
    pure(Value::Str(s.into()))
}

fn seq() -> Pattern {
    fastcat(vec![atom("bd"), atom("sd")])
}

/// `fmap` with a user JS callback.
#[test]
fn js_fmap_transforms_values() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x + '!'");
    let p = seq().fmap_js(id);

    assert!(!p.is_pure(), "a JS callback makes the pattern impure");

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    let shown: Vec<String> = haps.iter().map(|h| h.show()).collect();
    assert_eq!(
        shown,
        vec!["[ 0/1 → 1/2 | bd! ]", "[ 1/2 → 1/1 | sd! ]"],
        "the JS callback must run once per hap, inside the query"
    );
}

/// The callback runs per hap, at query time - not once at build time.
#[test]
fn js_callback_runs_once_per_hap_at_query_time() {
    let rt = JsRuntime::new().unwrap();
    // A counter in JS closure state, which also proves closure state survives
    // across queries (the property that killed the staging-runtime design).
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => { let n = 0; return (x) => `${x}${n++}`; }");
    let p = seq().fmap_js(id);
    rt.set_active(&b, p).unwrap();

    let first = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    let second = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();

    assert_eq!(first[0].value.show(), "bd0");
    assert_eq!(first[1].value.show(), "sd1");
    // The second query continues the same closure state: 2 and 3, not 0 and 1.
    assert_eq!(second[0].value.show(), "bd2");
    assert_eq!(
        second[1].value.show(),
        "sd3",
        "closure state must persist across queries; a heap swap would reset it"
    );
}

/// Each lone surrogate in a hap value reads as one U+FFFD, and the text
/// around it, surrogate pairs included, is kept.
#[test]
fn js_fmap_lone_surrogate_value_reads_as_replacement_character() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        r"(_w) => () => 'a' + String.fromCharCode(0xD800) + 'b' + String.fromCharCode(0xDC00) + '\u{1F600}'",
    );
    let p = atom("bd").fmap_js(id);

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        haps.iter().map(|h| h.value.show()).collect::<Vec<_>>(),
        vec!["a\u{FFFD}b\u{FFFD}\u{1F600}"]
    );
}

/// A BigInt hap value reads as `Number(bigint)`, without wrapping past 64 bits.
#[test]
fn js_bigint_hap_value_reads_as_number() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => () => 2n ** 64n + 1n");
    let p = atom("bd").fmap_js(id);

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        haps.iter().map(|h| h.value.as_f64()).collect::<Vec<_>>(),
        vec![Some(2f64.powi(64))]
    );
}

/// A BigInt argument to `pure` reads as `Number(bigint)`.
#[test]
fn pure_bigint_argument_reads_as_number() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.evaluate_score("pure(1n)", &TranspileOptions::default())
        .unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        haps.iter().map(|h| h.value.show()).collect::<Vec<_>>(),
        vec!["1"]
    );
}

/// Runtime `register()`: user code defines a combinator at run time, so its
/// name is not known at compile time. `Registration.names` is therefore
/// `Vec<Arc<str>>` and not `&'static [&'static str]`.
#[test]
fn runtime_register_from_user_code() {
    use rustel_core::register::{CombinatorFn, DeclaredIn, JoinKind, Registration, Registry};
    use std::sync::Arc;

    let rt = JsRuntime::new().unwrap();
    // The user's combinator body, defined at run time.
    let mut b = rt.builder();
    let cb = b.callback(&rt, "(_w) => (x) => x + '_reg'");

    let mut reg = Registry::new();
    reg.register(Registration {
        declared_in: DeclaredIn::PatternModule,
        takes_function: false,
        names: vec![Arc::from("userfn")],
        reference: rustel_core::reference::ReferenceEntry::blank("userfn"),
        arity: 1,
        patternify: true,
        preserve_steps: false,
        join: JoinKind::Inner,
        // Must be Dynamic: a body that touches JS cannot typecheck as Native.
        func: CombinatorFn::Dynamic(Arc::new(move |_args: &[Value], pat: Pattern| {
            pat.fmap_js(cb)
        })),
    });

    let entry = reg.get("userfn").expect("runtime-registered name resolves");
    let p = entry.call(&[], seq());
    assert!(!p.is_pure(), "a user combinator wrapping JS is impure");

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(
        haps.iter().map(|h| h.value.show()).collect::<Vec<_>>(),
        vec!["bd_reg", "sd_reg"]
    );
}

/// A JS callback inside the general path of `register()`: the argument is a
/// pattern, so the combinator body runs at query time and enters JS.
#[test]
fn js_callback_under_register_general_path() {
    use rustel_core::register::{CombinatorFn, DeclaredIn, JoinKind, Registration, Registry};
    use std::sync::Arc;

    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let cb = b.callback(&rt, "(_w) => (x) => `<${x}>`");

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
        // Must be Dynamic: a body that touches JS cannot typecheck as Native.
        func: CombinatorFn::Dynamic(Arc::new(move |_args: &[Value], pat: Pattern| {
            pat.fmap_js(cb)
        })),
    });

    let entry = reg.get("tagged").unwrap();
    // Patterned leading argument -> general path -> appLeft fold -> innerJoin.
    let p = entry.call(&[fastcat(vec![atom("1"), atom("2")])], seq());
    assert!(!p.is_pure());

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert!(
        !haps.is_empty(),
        "general path with a JS callback must produce haps"
    );
    assert!(
        haps.iter().all(|h| h.value.show().starts_with('<')),
        "every value passed through the JS callback: {:?}",
        haps.iter().map(|h| h.value.show()).collect::<Vec<_>>()
    );
}

/// A user-authored `new Pattern(state => …)` - the shape `fill` and `glide` use.
#[test]
fn user_authored_query_function() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => [{begin: state.span.begin, end: state.span.end, value: 'x'}]",
    );
    let p = rustel_core::js_query(id);
    assert!(!p.is_pure());

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value.show(), "x");
}

/// A query callback's BigInt time limbs beyond `i64` cross exactly, through
/// both the `u64` round trip (2^63) and the decimal fallback (2^64 + 1).
#[test]
fn user_authored_query_function_reads_oversized_bigint_limbs_exactly() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        r#"(_w) => (state) => [
              {begin: Fraction(2n ** 63n), end: Fraction(2n ** 63n + 1n), value: 'u64'},
              {begin: Fraction(2n ** 64n + 1n), end: Fraction(2n ** 64n + 2n), value: 'text'},
            ]"#,
    );
    let p = rustel_core::js_query(id);

    rt.set_active(&b, p).unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(haps.len(), 2, "both oversized haps must survive the read");
    // Sorted by part, so the 2^63 hap precedes the 2^64+1 hap.
    assert_eq!(haps[0].value.show(), "u64");
    assert_eq!(haps[0].part.begin, Fraction::new(2_i128.pow(63), 1));
    assert_eq!(haps[0].part.end, Fraction::new(2_i128.pow(63) + 1, 1));
    assert_eq!(haps[1].value.show(), "text");
    assert_eq!(haps[1].part.begin, Fraction::new(2_i128.pow(64) + 1, 1));
    assert_eq!(haps[1].part.end, Fraction::new(2_i128.pow(64) + 2, 1));
}

/// A query callback time whose limbs do not fit an `i128` fraction fails the
/// query, which then yields no haps, instead of being rounded, saturated or
/// overflowed.
#[test]
fn a_query_callback_time_beyond_i128_is_refused() {
    for end in [
        "Fraction(2n ** 130n + 1n).div(2n ** 131n)",
        "{n: 1e300, d: 1, s: 1}",
        "{n: 2n ** 126n, d: 1n, s: 2n}",
        "{n: -(2n ** 127n), d: -1n, s: 1n}",
    ] {
        let rt = JsRuntime::new().unwrap();
        rt.install_semantic_bindings().unwrap();
        let mut b = rt.builder();
        let id = b.callback(
            &rt,
            &format!("(_w) => (state) => [{{begin: Fraction(0), end: {end}, value: 'x'}}]"),
        );
        rt.set_active(&b, rustel_core::js_query(id)).unwrap();
        let haps = rt
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap();
        assert!(haps.is_empty(), "{end} must be refused, got {haps:?}");
    }
}

/// The runtime must tear down cleanly: QuickJS asserts
/// `list_empty(&rt->gc_obj_list)` in `JS_FreeRuntime`, so a leak aborts the
/// process rather than passing quietly.
#[test]
fn runtime_tears_down_cleanly() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (x) => x");
    let p = seq().fmap_js(id);
    rt.set_active(&b, p).unwrap();
    let _ = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    rt.clear_active();
    drop(rt);
    // Reaching here is the assertion.
}

/// The configured string parser is a runtime-private root and may retain a
/// Pattern whose callback closes the JS↔Rust ownership cycle. `JsRuntime::drop`
/// must release that root before QuickJS's final GC/free sequence.
#[test]
fn configured_parser_capture_is_released_before_runtime_teardown() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_prelude(
            "const captured = pure('x').fmap(value => `${value}!`); \
             setStringParser(() => captured);",
            &TranspileOptions::default(),
            Duration::from_secs(1),
        )
        .expect("configure callback-bearing parser");
    drop(runtime);
    // Reaching here is the assertion: QuickJS aborts on a live GC cycle.
}

#[test]
fn transpiled_mini_expression_installs_native_active_graph() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    let output = runtime
        .evaluate_score(
            "\"bd sd\".fast(2)",
            &rustel_transpiler::TranspileOptions::default(),
        )
        .unwrap();
    assert_eq!(output.output, "m('bd sd', 0).fast(2);");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(haps.len(), 4);
    assert_eq!(
        haps.iter().map(|hap| hap.value.show()).collect::<Vec<_>>(),
        ["bd", "sd", "bd", "sd"]
    );
}

#[test]
fn native_score_evaluation_rejects_module_syntax_before_execution() {
    use std::sync::atomic::AtomicBool;

    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score("pure('baseline')", &TranspileOptions::default())
        .expect("baseline score");
    let cancellation = AtomicBool::new(false);

    for source in [
        "import './module.js'; pure('replaced')",
        "import('./module.js'); pure('replaced')",
        "pure(import.meta.url)",
    ] {
        let error = runtime
            .evaluate_score_cancellable(
                source,
                &TranspileOptions::default(),
                Duration::from_secs(1),
                &cancellation,
            )
            .expect_err("module syntax must be rejected");
        assert!(
            error.to_string().contains("disabled in native score code"),
            "unexpected error for {source:?}: {error:?}"
        );
        assert_eq!(active_values(&runtime), ["baseline"]);
    }

    let setup_error = runtime
        .evaluate_prelude(
            "import './setup.js';",
            &TranspileOptions::default(),
            Duration::from_secs(1),
        )
        .expect_err("prebake source must have the same module boundary");
    assert!(
        setup_error
            .to_string()
            .contains("module declarations are disabled in native score code"),
        "unexpected prebake import error: {setup_error:?}"
    );
}

#[test]
fn runtime_generated_imports_hit_the_rejecting_module_resolver() {
    use std::sync::atomic::AtomicBool;

    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score("pure('baseline')", &TranspileOptions::default())
        .expect("baseline score");
    let cancellation = AtomicBool::new(false);
    for target in [
        "file:///etc/passwd",
        "https://127.0.0.1:9/module.js",
        "std",
        "os",
    ] {
        let source = format!(
            "await Function('target', 'return import(target)')('{target}'); pure('replaced')"
        );
        let error = runtime
            .evaluate_score_cancellable(
                &source,
                &TranspileOptions::default(),
                Duration::from_secs(1),
                &cancellation,
            )
            .expect_err("a constructed import must not bypass the source validator");
        assert!(
            error
                .to_string()
                .contains("module loading is disabled in native score code"),
            "constructed import {target:?} did not reach the rejecting resolver: {error:?}"
        );
        assert_eq!(active_values(&runtime), ["baseline"]);
    }
}

#[test]
fn score_effect_staging_is_bounded_and_native_recorders_are_not_global() {
    use std::sync::atomic::AtomicBool;

    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(
            "pure([typeof globalThis.__rustelSamplesEffect, \
                   typeof globalThis.__rustelPreloadEffect, \
                   typeof globalThis.fetch, typeof globalThis.XMLHttpRequest, \
                   typeof globalThis.WebSocket, typeof globalThis.require, \
                   typeof globalThis.process, typeof globalThis.load, \
                   typeof globalThis.read, typeof globalThis.readFile, \
                   typeof globalThis.scriptArgs, typeof globalThis.std, \
                   typeof globalThis.os].join(','))",
            &TranspileOptions::default(),
        )
        .expect("inspect public globals");
    let unavailable = vec![["undefined"; 13].join(",")];
    assert_eq!(active_values(&runtime), unavailable);

    let source = format!("{} pure('replaced')", "samples({});".repeat(65));
    let inspect_options = TranspileOptions {
        add_return: false,
        ..TranspileOptions::default()
    };
    let transformed = rustel_transpiler::transpile(&source, &inspect_options);
    assert!(
        transformed.diagnostics.is_empty(),
        "effect source did not transpile: {:?}",
        transformed.diagnostics
    );
    let error = runtime
        .evaluate_score_with_effects_cancellable(
            &source,
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .expect_err("host effects must stop at their count limit");
    assert!(
        error.to_string().contains("host effect limit"),
        "unexpected effect-limit error: {error:?}; output: {}",
        transformed.output
    );
    assert_eq!(active_values(&runtime), unavailable);
}

const BOUNDED_SCORE_CHILD: &str = "RUSTEL_BOUNDED_SCORE_CHILD";

fn active_values(runtime: &JsRuntime) -> Vec<String> {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("active graph remains queryable")
        .into_iter()
        .map(|hap| hap.value.show())
        .collect()
}

fn bounded_score_execution_child() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    let options = TranspileOptions::default();
    let cancellation = Arc::new(AtomicBool::new(false));

    runtime
        .evaluate_score(
            "(() => { let calls = 0; return pure('private-active').fmap(value => `${value}:${++calls}`); })()",
            &options,
        )
        .expect("callback-bearing baseline score");
    assert_eq!(active_values(&runtime), ["private-active:1"]);

    let deadline_budget = Duration::from_millis(40);
    let started = std::time::Instant::now();
    let deadline = runtime
        .evaluate_score_cancellable(
            "globalThis.__rustel_active = null; globalThis.scorePrefix = 17; while (true) {} s('sd')",
            &options,
            deadline_budget,
            &cancellation,
        )
        .expect_err("a synchronous runaway score must hit its deadline");
    assert!(
        matches!(
            deadline,
            QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline { millis: 40 })
        ),
        "deadline lost its typed refusal: {deadline:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "deadline returned too late: {:?}",
        started.elapsed()
    );
    assert_eq!(
        runtime.get_number("scorePrefix"),
        Some(17.0),
        "completed JavaScript side effects were rolled back"
    );
    runtime.run_gc();
    runtime.run_gc();
    assert_eq!(
        active_values(&runtime),
        ["private-active:2"],
        "an interrupted score replaced the private active wrapper or its callback"
    );

    // The deadline encloses post-eval wrapper validation as well as ctx.eval.
    // A query getter runs only after the score expression has returned its
    // Pattern; placing the runaway there catches a scope that covers eval but
    // expires before active-graph validation/publication is complete.
    let validation = runtime
        .evaluate_score_cancellable(
            "(() => { globalThis.__rustel_active = { query() { return []; } }; globalThis.__rustel_active.query = null; const p = s('sd'); Object.defineProperty(p, 'query', { configurable: true, get() { globalThis.validationPrefix = 19; while (true) {} } }); return p; })()",
            &options,
            deadline_budget,
            &cancellation,
        )
        .expect_err("runaway Pattern validation must hit the same deadline");
    assert!(
        matches!(
            validation,
            QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline { millis: 40 })
        ),
        "post-eval validation escaped the typed deadline: {validation:?}"
    );
    assert_eq!(runtime.get_number("validationPrefix"), Some(19.0));
    runtime.run_gc();
    runtime.run_gc();
    assert_eq!(active_values(&runtime), ["private-active:3"]);

    let queued = runtime
        .evaluate_score_cancellable(
            "globalThis.scoreQueued = 1; Promise.resolve().then(() => { globalThis.scoreJobRan = 1; }); s('sd')",
            &options,
            Duration::from_secs(1),
            &cancellation,
        )
        .expect_err("a score-created runnable job must be refused");
    assert!(
        matches!(
            &queued,
            QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs)
        ),
        "score-created job got the wrong refusal: {queued:?}"
    );
    assert_eq!(runtime.get_number("scoreQueued"), Some(1.0));
    assert_eq!(
        runtime.get_number("scoreJobRan"),
        None,
        "a score-created job ran instead of being discarded"
    );
    assert!(
        !runtime.jobs_pending(),
        "a refused score stranded a runnable job on the shared heap"
    );
    assert_eq!(
        active_values(&runtime),
        ["private-active:4"],
        "a score with unsupported queued work replaced the active graph"
    );

    let queued_then_thrown = runtime
        .evaluate_score_cancellable(
            "globalThis.queuedThrowPrefix = 1; Promise.resolve().then(() => { globalThis.queuedThrowJobRan = 1; }); throw new Error('later throw'); s('sd')",
            &options,
            Duration::from_secs(1),
            &cancellation,
        )
        .expect_err("a score that queues work before throwing must still refuse the job");
    assert!(
        matches!(
            &queued_then_thrown,
            QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs)
        ),
        "an ordinary throw hid its previously queued job: {queued_then_thrown:?}"
    );
    assert_eq!(runtime.get_number("queuedThrowPrefix"), Some(1.0));
    assert_eq!(
        runtime.get_number("queuedThrowJobRan"),
        None,
        "a job queued before a throw ran instead of being discarded"
    );
    assert!(
        !runtime.jobs_pending(),
        "a throw stranded its previously queued job on the shared heap"
    );
    assert_eq!(
        active_values(&runtime),
        ["private-active:5"],
        "a throwing score with unsupported queued work replaced the active graph"
    );

    runtime
        .eval("Promise.resolve().then(() => { globalThis.staleScoreJobRan = 1; });")
        .expect("seed a preexisting job through the legacy raw boundary");
    assert!(runtime.jobs_pending(), "stale-job control did not enqueue");
    let cancelled_with_stale = std::sync::atomic::AtomicBool::new(true);
    let cancelled_stale = runtime
        .evaluate_score_cancellable(
            "globalThis.cancelledStaleCandidateRan = 1; s('sd')",
            &options,
            Duration::from_secs(1),
            &cancelled_with_stale,
        )
        .expect_err("pre-set cancellation must win after stale-job cleanup");
    assert!(matches!(
        cancelled_stale,
        QueryError::Limit(rustel_core::QueryLimit::Cancelled)
    ));
    assert_eq!(runtime.get_number("staleScoreJobRan"), None);
    assert_eq!(runtime.get_number("cancelledStaleCandidateRan"), None);
    assert!(
        !runtime.jobs_pending(),
        "cancelled entry left its stale job"
    );

    runtime
        .eval("Promise.resolve().then(() => { globalThis.secondStaleJobRan = 1; });")
        .expect("seed a second preexisting job");
    let stale = runtime
        .evaluate_score_cancellable(
            "globalThis.staleCandidateRan = 1; s('sd')",
            &options,
            Duration::from_secs(1),
            &cancellation,
        )
        .expect_err("an unattributable preexisting job must refuse the score");
    assert!(
        matches!(
            &stale,
            QueryError::Limit(rustel_core::QueryLimit::JsPendingJobs)
        ),
        "preexisting job got the wrong refusal: {stale:?}"
    );
    assert_eq!(runtime.get_number("secondStaleJobRan"), None);
    assert_eq!(runtime.get_number("staleCandidateRan"), None);
    assert!(!runtime.jobs_pending(), "preexisting job was not discarded");
    assert_eq!(active_values(&runtime), ["private-active:6"]);

    // Use an eager callback route for recovery: this also makes the bounded
    // path's callback host and BridgeFrame load-bearing rather than merely
    // checking a pure graph that would work without either.
    runtime
        .evaluate_score_cancellable(
            "(() => { let calls = 0; return pure('recovered').every(1, pattern => pattern.fmap(value => `${value}:${++calls}`)); })()",
            &options,
            Duration::from_secs(1),
            &cancellation,
        )
        .expect("same-heap recovery after deadline");
    assert_eq!(active_values(&runtime), ["recovered:1"]);

    let cancellation_for_thread = cancellation.clone();
    let request = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        cancellation_for_thread.store(true, Ordering::Relaxed);
    });
    let started = std::time::Instant::now();
    let cancelled = runtime
        .evaluate_score_cancellable(
            "globalThis.__rustel_active = null; globalThis.cancelPrefix = 23; while (true) {} s('hh')",
            &options,
            Duration::from_secs(2),
            &cancellation,
        )
        .expect_err("caller cancellation must interrupt synchronous score JS");
    request.join().expect("cancellation requester");
    assert!(
        matches!(
            cancelled,
            QueryError::Limit(rustel_core::QueryLimit::Cancelled)
        ),
        "cancellation was reported as a deadline/user error: {cancelled:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancellation waited for the two-second deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(runtime.get_number("cancelPrefix"), Some(23.0));
    runtime.run_gc();
    runtime.run_gc();
    assert_eq!(
        active_values(&runtime),
        ["recovered:2"],
        "a cancelled score replaced the private active wrapper or its callback"
    );

    // A cancellation already requested must win before any user side effect.
    let already_cancelled = AtomicBool::new(true);
    let refused = runtime
        .evaluate_score_cancellable(
            "globalThis.preCancelledRan = 1; s('cp')",
            &options,
            Duration::from_secs(1),
            &already_cancelled,
        )
        .expect_err("pre-set cancellation must refuse the turn");
    assert!(matches!(
        refused,
        QueryError::Limit(rustel_core::QueryLimit::Cancelled)
    ));
    assert_eq!(runtime.get_number("preCancelledRan"), None);

    // User exceptions are still ordinary evaluation errors, not resource
    // limits, and their already-completed one-heap side effects remain.
    cancellation.store(false, Ordering::Relaxed);
    let thrown = runtime
        .evaluate_score_cancellable(
            "globalThis.__rustel_active = { query: null }; Object.freeze(globalThis.__rustel_active); globalThis.throwPrefix = 29; throw new Error('score boom'); s('cp')",
            &options,
            Duration::from_secs(1),
            &cancellation,
        )
        .expect_err("throwing score must fail");
    assert!(
        matches!(&thrown, QueryError::Message(message) if message.contains("score boom")),
        "ordinary throw was reclassified: {thrown:?}"
    );
    assert_eq!(runtime.get_number("throwPrefix"), Some(29.0));
    runtime.run_gc();
    runtime.run_gc();
    assert_eq!(active_values(&runtime), ["recovered:3"]);

    runtime
        .evaluate_score_cancellable("s(\"hh\")", &options, Duration::from_secs(1), &cancellation)
        .expect("same heap must recover after cancellation and throw");
    assert_eq!(active_values(&runtime), ["s:hh"]);
}

/// Run the hostile score in a supervised child. If the deadline is removed,
/// the mutation kills only this child and the parent reports a bounded failure
/// instead of hanging the workspace test process.
#[test]
fn bounded_score_execution_is_typed_atomic_cancellable_and_recoverable() {
    if std::env::var_os(BOUNDED_SCORE_CHILD).is_some() {
        bounded_score_execution_child();
        return;
    }

    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("bounded_score_execution_is_typed_atomic_cancellable_and_recoverable")
        .arg("--nocapture")
        .env(BOUNDED_SCORE_CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bounded score child");
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait().expect("poll bounded score child") {
            let output = child.wait_with_output().expect("collect child output");
            assert!(
                status.success(),
                "bounded score child failed with {status}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().expect("kill hung bounded score child");
            let output = child.wait_with_output().expect("reap killed child");
            panic!(
                "bounded score child exceeded eight seconds; the QuickJS deadline/cancellation \
                 is not load-bearing:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn bounded_score_heap_refusal_is_typed_atomic_and_recoverable() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    let options = TranspileOptions::default();
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    runtime
        .evaluate_score("s(\"bd\").every(1, x => x.fast(2))", &options)
        .expect("callback-bearing baseline score");

    let live = runtime.heap_live();
    runtime
        .set_memory_limit(live.saturating_add(8 * 1024 * 1024))
        .expect("install a bounded heap headroom");
    let refused = runtime
        .evaluate_score_cancellable(
            "globalThis.heapPrefix = 31; try { globalThis.__hog = new Array(2000000).fill(7); } catch (_) { globalThis.heapCaught = 1; } s('sd')",
            &options,
            Duration::from_secs(1),
            &cancellation,
        )
        .expect_err("score allocation beyond the ceiling must be refused");
    assert!(
        matches!(
            refused,
            QueryError::Limit(rustel_core::QueryLimit::HostMemory)
        ),
        "the outer heap boundary lost its structural refusal: {refused:?}"
    );
    assert_eq!(runtime.get_number("heapPrefix"), Some(31.0));
    assert_eq!(
        active_values(&runtime),
        ["s:bd", "s:bd"],
        "caught heap refusal replaced the active graph or lost its exact callback sidecar"
    );

    runtime
        .evaluate_score_cancellable("s(\"cp\")", &options, Duration::from_secs(1), &cancellation)
        .expect("same heap must recover after score allocation refusal");
    assert_eq!(active_values(&runtime), ["s:cp"]);
}

/// The conversion of a query callback's array is checked against the hap
/// budget as the vector grows. The test injects a small budget: the 5,000,000
/// default would need a callback that returns five million objects.
#[test]
fn a_query_callback_cannot_exceed_the_hap_budget() {
    const BUDGET: u64 = 500;

    let build = |n: usize| {
        let rt = JsRuntime::new().unwrap();
        let mut b = rt.builder();
        let id = b.callback(
            &rt,
            &format!(
                "(_w) => (state) => Array.from({{length: {n}}}, (_, i) => ({{\
                   begin: state.span.begin, end: state.span.end, value: 'x' + i}}))"
            ),
        );
        rt.set_active(&b, rustel_core::js_query(id)).unwrap();
        rt
    };

    // Exactly the budget is allowed, in full.
    let rt = build(BUDGET as usize);
    let haps = rt
        .query_with_budget(Slot::Active, 0, Fraction::ZERO, Fraction::ONE, BUDGET)
        .expect("exactly the budget must be allowed");
    assert_eq!(
        haps.len() as u64,
        BUDGET,
        "the at-limit callback was truncated"
    );

    // One more is refused, and the Rust vector never reaches that size.
    let rt = build(BUDGET as usize + 1);
    rustel_core::reset_haps_materialised();
    let refused = rt.query_with_budget(Slot::Active, 0, Fraction::ZERO, Fraction::ONE, BUDGET);
    assert!(
        refused.is_err(),
        "a callback returning {} haps must be refused under a {BUDGET} budget",
        BUDGET + 1
    );
    let peak = rustel_core::peak_hap_vector();
    assert!(
        peak <= BUDGET,
        "a vector of {peak} haps was built under a {BUDGET} budget: the \
         conversion is not checked as it grows"
    );
}

#[test]
fn a_cancellable_pure_query_uses_and_restores_its_hap_budget() {
    let rt = JsRuntime::new().unwrap();
    let builder = rt.builder();
    rt.set_active(&builder, fastcat(vec![atom("bd"); 16]))
        .expect("pure active graph");
    let cancellation = std::sync::atomic::AtomicBool::new(false);

    let refused = rt.query_cancellable_with_hap_budget(
        Slot::Active,
        0,
        Fraction::ZERO,
        Fraction::ONE,
        Duration::from_secs(1),
        8,
        &cancellation,
    );
    assert!(
        matches!(
            &refused,
            Err(QueryError::Limit(rustel_core::QueryLimit::HapBudget {
                budget: 8
            }))
        ),
        "the pure fast path must honor the caller's hap budget: {refused:?}"
    );
    assert_eq!(
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("the previous budget was restored")
            .len(),
        16
    );
}

/// A sparse array is budgeted by the haps it holds, not by its length:
/// `Array(50_000)` with three elements set converts to three haps.
#[test]
fn a_sparse_callback_array_is_budgeted_by_converted_haps() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => { const a = new Array(50000);            a[0] = {begin: state.span.begin, end: state.span.end, value: 'a'};            a[10] = {begin: state.span.begin, end: state.span.end, value: 'b'};            a[20] = {begin: state.span.begin, end: state.span.end, value: 'c'};            return a; }",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    let haps = rt
        .query_with_budget(Slot::Active, 0, Fraction::ZERO, Fraction::ONE, 100)
        .expect("three converted haps must be allowed under a 100 budget");
    assert_eq!(
        haps.len(),
        3,
        "a sparse array must yield its set elements, not its length"
    );
}

/// The hap budget bounds the Rust conversion only. The callback builds its
/// array on the QuickJS heap before it returns; the heap ceiling tested below
/// bounds that allocation.
#[test]
fn a_refused_callback_array_still_costs_the_quickjs_heap() {
    let rt = JsRuntime::new().unwrap();
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => Array.from({length: 5000}, (_, i) => ({\
           begin: state.span.begin, end: state.span.end, value: i}))",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    // Refused during conversion - but the 5,000-element JS array was built
    // before Rust saw any of it.
    let refused = rt.query_with_budget(Slot::Active, 0, Fraction::ZERO, Fraction::ONE, 100);
    assert!(refused.is_err(), "the oversized array must be refused");
    // The Rust side stayed small, which is what this layer can guarantee.
    assert!(
        rustel_core::peak_hap_vector() <= 100,
        "the Rust conversion exceeded the budget"
    );
}

// -- the QuickJS heap ceiling ----------------------------------------------
//
// The hap budget bounds what Rust converts. It cannot bound what JavaScript
// allocated before returning, because the array already exists by then - a
// callback doing `Array.from({length: 1e9})` takes that memory inside QuickJS
// with nothing on the Rust side able to intervene.
//
// The ceiling is enforced by the allocator installed when QuickJS is created:
// an allocation past it fails and the engine raises a catchable JavaScript
// error instead of aborting. Every test here lowers the configured limit,
// because proving the behaviour at 512 MiB would mean allocating 512 MiB.
//
// A browser tab has its own memory ceiling and behavior; these tests cover the
// embedded runtime only.

/// A dense allocation of about 16 MiB of f64 slots against an 8 MiB limit.
/// Twice the limit proves the refusal and is harmless if the guard is removed.
const BOUNDED_HOG: &str = "globalThis.__hog = new Array(2000000).fill(7);";

#[test]
fn the_default_heap_ceiling_is_installed_and_cannot_be_raised() {
    let rt = JsRuntime::new().unwrap();
    assert_eq!(
        rt.memory_limit(),
        rustel_jsruntime::DEFAULT_JS_MEMORY_LIMIT,
        "the ceiling must be in force from construction, before any user code"
    );
    assert!(
        rt.set_memory_limit(rustel_jsruntime::DEFAULT_JS_MEMORY_LIMIT + 1)
            .is_err(),
        "raising the ceiling would let a caller remove the guard"
    );
    // QuickJS reads a limit of zero as unlimited, so accepting zero would
    // remove the bound.
    assert!(
        rt.set_memory_limit(0).is_err(),
        "a ceiling of zero means unlimited and must be refused"
    );
    assert_eq!(
        rt.memory_limit(),
        rustel_jsruntime::DEFAULT_JS_MEMORY_LIMIT,
        "a refused call must not have changed the ceiling"
    );

    assert!(
        rt.set_memory_limit(1024 * 1024).is_ok(),
        "lowering is allowed"
    );
    assert_eq!(rt.memory_limit(), 1024 * 1024);

    // MONOTONIC. Comparing only against the default let a caller lower to 1 MiB
    // and then climb back to 512 MiB, undoing a tightened bound rather than
    // respecting it.
    for raise in [
        2 * 1024 * 1024,
        8 * 1024 * 1024,
        rustel_jsruntime::DEFAULT_JS_MEMORY_LIMIT,
    ] {
        assert!(
            rt.set_memory_limit(raise).is_err(),
            "raising the ceiling from 1 MiB to {raise} must be refused"
        );
        assert_eq!(rt.memory_limit(), 1024 * 1024, "a refused raise changed it");
    }
    // ...and lowering further is still fine.
    assert!(rt.set_memory_limit(512 * 1024).is_ok());
    assert_eq!(rt.memory_limit(), 512 * 1024);
}

#[test]
fn ordinary_patterns_are_unaffected_by_the_ceiling() {
    // The half that matters most: a limit nobody notices is only useful if it
    // changes nothing below it. Queried against the same expectations the rest
    // of the suite uses.
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.evaluate_score(
        r#"s("bd sd").fast(2)"#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("an ordinary pattern must evaluate under the ceiling");
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert_eq!(haps.len(), 4);
    assert_eq!(haps[0].show(), "[ 0/1 → 1/4 | s:bd ]");
}

#[test]
fn runaway_javascript_allocation_is_refused_rather_than_fatal() {
    // The property being bought. Without a ceiling this takes the process down;
    // with one, QuickJS raises an ordinary error that the host reports.
    //
    // Timed, because "refused" and "refused after swapping for a minute" are
    // different outcomes.
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(8 * 1024 * 1024).unwrap();

    let started = std::time::Instant::now();
    let result = rt.eval(BOUNDED_HOG);
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "an allocation far past the ceiling must be refused"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the refusal took {elapsed:?}"
    );
}

#[test]
fn the_runtime_still_works_after_a_refused_allocation() {
    // Recovery is the difference between a bound and a kill switch. A runtime
    // that survives the refusal but is left unusable would pass the test above
    // and be useless in a live-coding session, where the next thing the user
    // does is fix their expression and re-evaluate.
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(8 * 1024 * 1024).unwrap();

    assert!(
        rt.eval(BOUNDED_HOG).is_err(),
        "the runaway allocation must be refused"
    );

    // The SAME runtime, immediately afterwards.
    rt.evaluate_score(
        r#"s("bd sd")"#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("the runtime must still evaluate after a refused allocation");
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("the runtime must still query after a refused allocation");
    assert_eq!(haps.len(), 2);
    assert_eq!(haps[0].show(), "[ 0/1 → 1/2 | s:bd ]");

    // ...and repeatedly, so recovery is not a one-off.
    for _ in 0..3 {
        assert!(rt.eval(BOUNDED_HOG).is_err());
        assert_eq!(
            rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("still queryable")
                .len(),
            2
        );
    }
}

/// A callback that exhausts the heap fails the query with a typed refusal.
/// The hap budget cannot reach the allocation: JavaScript builds the array
/// before Rust converts anything. The refusal comes from the allocator's
/// denial, not from a message match: `JS_ThrowOutOfMemory` throws null when
/// it cannot build its error, so no stable text exists.
#[test]
fn a_callback_that_allocates_past_the_ceiling_fails_the_query_not_the_process() {
    // The allocation happens inside a query
    // callback, where the hap budget cannot reach it because the array is built
    // before Rust converts anything.
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(8 * 1024 * 1024).unwrap();

    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => Array.from({length: 2000000}, () => ({\
           begin: state.span.begin, end: state.span.end, value: 1}))",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    let result = rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
    assert!(
        result.is_err(),
        "a callback allocating past the ceiling must fail the QUERY"
    );

    // And the runtime is still usable - the query failed, not the session.
    rt.evaluate_score(
        r#"s("bd")"#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("the runtime must survive a callback that blew the ceiling");
}

/// An ordinary throw is not a heap refusal. A message matcher would catch
/// each of these throws, but no allocation is denied. Each callback gives an
/// empty successful query, as strudel.cc does for a throwing callback.
#[test]
fn ordinary_throws_are_not_reported_as_heap_exhaustion() {
    for body in [
        "throw null;",
        "throw new Error('out of memory');",
        "throw new Error('null pointer');",
        "throw new Error('boom');",
        "throw 'a string';",
        "undefined.property;",
    ] {
        let rt = JsRuntime::new().unwrap();
        let mut b = rt.builder();
        let id = b.callback(&rt, &format!("(_w) => (state) => {{ {body} }}"));
        rt.set_active(&b, rustel_core::js_query(id)).unwrap();

        let out = rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
        match out {
            Ok(haps) => assert!(
                haps.is_empty(),
                "{body}: a throwing callback must empty the query"
            ),
            Err(e) => panic!(
                "{body}: an ordinary throw was reported as {e}. Only a DENIED \
                 ALLOCATION is a heap refusal; a message that merely mentions \
                 one is a user's own error."
            ),
        }
    }
}

/// The flag must not leak from one query into the next.
#[test]
fn a_heap_refusal_does_not_poison_a_later_query() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(8 * 1024 * 1024).unwrap();

    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => Array.from({length: 2000000}, () => ({\
           begin: state.span.begin, end: state.span.end, value: 1}))",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();
    assert!(
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .is_err(),
        "the oversized callback must be refused"
    );

    // A different, ordinary graph on the SAME runtime.
    rt.evaluate_score(
        r#"s("bd sd")"#,
        &rustel_transpiler::TranspileOptions::default(),
    )
    .expect("the runtime must still evaluate");
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("a later query must not inherit the refusal");
    assert_eq!(haps.len(), 2);
}

/// Lowering below live usage is refused, so a tightened ceiling cannot poison a
/// runtime that is forbidden from raising it again.
#[test]
fn the_ceiling_cannot_be_lowered_below_live_usage() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    let live = rt.heap_live();
    assert!(live > 0, "a constructed runtime holds some heap");
    assert!(
        rt.set_memory_limit(live / 2).is_err(),
        "lowering under {live} live bytes would deny every later allocation \
         with no way back"
    );
    assert!(
        rt.set_memory_limit(live * 4).is_ok(),
        "lowering to a value above live usage is fine"
    );
}

/// A wide JS `fmap` stops its loop when the deadline passes. The loop checks
/// the deadline between haps; a check on the finished vector is too late.
#[test]
fn a_wide_js_fmap_stops_at_the_deadline_instead_of_owning_the_thread() {
    let rt = JsRuntime::new().unwrap();
    rt.eval("globalThis.__calls = 0").unwrap();
    let mut b = rt.builder();
    // Deliberately slow per call, so a loop that ignores the deadline cannot
    // finish inside the assertion window whatever the machine's speed.
    let id = b.callback(
        &rt,
        "(_w) => (v) => { globalThis.__calls++; \
           let s = 0; for (let i = 0; i < 200000; i++) s += i; return v; }",
    );
    let wide = fastcat(
        (0..2000)
            .map(|i| pure(Value::F64(i as f64)))
            .collect::<Vec<_>>(),
    );
    rt.set_active(&b, wide.fmap_js(id)).unwrap();

    let started = std::time::Instant::now();
    let refused = rt.with_deadline(Duration::from_millis(50), || {
        rt.query_with_budget(Slot::Active, 0, Fraction::ZERO, Fraction::ONE, 1_000_000)
    });
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "a wide JS fmap ran {elapsed:?} past a 50 ms deadline; the loop is unbounded"
    );
    assert!(
        refused.is_err(),
        "a deadline-abandoned query reported success instead of a refusal"
    );
    let calls = rt.get_number("__calls").expect("callback count") as u64;
    assert!(
        calls < 2000,
        "the loop mapped all {calls} haps: the deadline was checked only after the loop"
    );
}

// -- host materialization of a JS-controlled `length` -------------------------
//
// `new Array(N)` costs the sandbox almost nothing - QuickJS records the length
// and no elements - but materializing such a value into host `Value`s would
// reserve `N * size_of::<Value>()` outside the allocator the budget watches.
// `new Array(1e9)` is a ~32 GiB reservation against a 512 MiB ceiling, and a
// failed `with_capacity` aborts the process. The guard derives an element cap
// from the live ceiling and refuses an oversized length with a catchable
// RangeError before reserving anything.
//
// Same rule as BOUNDED_HOG: the probes are sized from a lowered ceiling so
// that, if the guard is ever removed, they cost a few MiB rather than tens of
// GiB. At the default 512 MiB ceiling the identical check refuses
// `new Array(1e9)` instantly - the cap is the largest reservation the sandbox
// itself could ever pay for, and the length test happens before any iteration.

const MATERIALIZE_CEILING: usize = 8 * 1024 * 1024;

/// The element cap the guard derives from [`MATERIALIZE_CEILING`], mirroring
/// `js_element_cap::<Value>` so probes stay one element past it.
fn materialize_cap() -> usize {
    MATERIALIZE_CEILING / std::mem::size_of::<Value>()
}

/// An oversized sparse array is refused BEFORE host memory is reserved, pinned
/// by the message only the guard produces.
#[test]
fn a_sparse_array_past_the_ceiling_is_refused_before_host_reservation() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    // `valueToMidi` materializes its argument during evaluation, so the
    // refusal surfaces directly as the eval's error string.
    let probe = materialize_cap() + 1;
    let result = rt.eval(&format!("valueToMidi(new Array({probe}))"));

    let Err(message) = result else {
        panic!("a sparse length of {probe} must be refused, not materialized")
    };
    assert!(
        message.contains("cannot materialize"),
        "the refusal must come from the heap-ceiling guard, got: {message}"
    );

    // A normal-sized array still materializes. This failure is the core
    // semantic check (a list of numbers is not a note object), not the guard.
    let semantic = rt.eval("valueToMidi([60, 62])").unwrap_err();
    assert!(
        semantic.contains("expected object value"),
        "an honest array must reach the core conversion, got: {semantic}"
    );
    rt.eval("valueToMidi({note: 60})")
        .expect("an acceptable value must still materialize under the ceiling");
}

/// Cycles and deep chains are refused at the depth cap, before they exhaust
/// the Rust stack, whatever element budget they spend. The refusal is
/// catchable and leaves the runtime able to convert a later, ordinary value.
#[test]
fn cyclic_and_deep_js_values_are_refused_during_materialization() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    for expression in [
        "const o = {}; o.self = o; valueToMidi(o)",
        "const a = []; a.push(a); valueToMidi(a)",
        "let o = {note: 60}; for (let i = 0; i < 300; i++) o = {inner: o}; valueToMidi(o)",
    ] {
        let refusal = caught(&rt, expression);
        assert!(
            refusal.starts_with("RangeError: cannot materialize a JS value nested too deeply"),
            "{expression} must be refused by the depth guard, got: {refusal}"
        );
    }

    rt.eval("globalThis.__v = valueToMidi({note: 60, extra: {velocity: 0.5}})")
        .expect("a shallow object must still materialize after refusals");
    assert_eq!(rt.get_number("__v"), Some(60.0));
}

/// A child shared under two named keys doubles the walk at each level. The
/// element budget refuses it; the depth cap alone does not.
#[test]
fn a_child_shared_under_named_keys_is_refused_by_the_element_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let refusal = caught(
        &rt,
        "let o = {note: 60}; for (let i = 0; i < 20; i++) o = {a: o, b: o}; valueToMidi(o)",
    );
    assert!(
        refusal.starts_with("RangeError: cannot materialize")
            && !refusal.contains("nested too deeply"),
        "a shared child must hit the element budget, got: {refusal}"
    );

    rt.eval("globalThis.__v = valueToMidi({note: 60, a: {b: 1}, c: {b: 1}})")
        .expect("a small object must still materialize");
    assert_eq!(rt.get_number("__v"), Some(60.0));
}

/// A self-containing or over-deep array given as a pattern argument is
/// refused at the list depth limit with a catchable RangeError, before it
/// exhausts the Rust stack. A shallow nested argument still reifies.
#[test]
fn cyclic_and_deep_pattern_arguments_are_refused_at_the_list_depth_limit() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    for expression in [
        r#"const a = []; a.push(a); s("bd").add(a)"#,
        r#"const a = []; a.push(a); s("bd").fast(a, 1)"#,
        r#"let a = 1; for (let i = 0; i < 300; i++) a = [a]; s("bd").add(a)"#,
    ] {
        assert_eq!(
            caught(&rt, expression),
            "RangeError: nested list depth exceeds the native limit",
            "{expression}"
        );
    }

    assert_eq!(caught(&rt, r#"s("bd").add([1, [2, 3]])"#), "no throw");
}

/// Pins that typed arrays and String wrappers charge one element per index
/// key on every path that mirrors an object under the element budget, while
/// honest ones under the ceiling still materialize.
#[test]
fn typed_arrays_and_string_wrappers_charge_the_element_budget_per_index() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    // A sibling array spends all but one element, so two index keys tip the
    // budget over. In a materialized object the two named keys charge one
    // element each as well.
    let pad = materialize_cap() - 1;
    let nested_pad = pad - 2;
    for call in [
        // `valueToMidi` materializes its argument.
        format!("valueToMidi({{pad: new Array({nested_pad}), value: new Uint8Array(2)}})"),
        format!("valueToMidi({{pad: new Array({nested_pad}), value: new String('xy')}})"),
        // A `Pattern.query` State mirrors its controls' keys; index keys
        // enumerate first.
        format!(
            "{{ const controls = new Uint8Array(2); controls.pad = new Array({pad}); \
               pure(1).query(new State(new TimeSpan(0, 1), controls)); }}"
        ),
    ] {
        let refusal = caught(&rt, &call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call} must hit the element budget, got: {refusal}"
        );
    }

    // A partial's rebuild shares one budget, so past it the partial is
    // bridged as a callback rather than rebuilt natively.
    let half = materialize_cap() / 2 + 1;
    assert!(
        calls_through_js(
            &rt,
            &format!(
                "pure('bd').every(2, counting(euclid(new Array({half}), new Uint8Array({half}))))"
            )
        ) > 0.0,
        "a bound typed array past the rebuild budget was rebuilt natively"
    );

    // An object keeps its named property beside a typed array, and a String
    // wrapper falls back like any object.
    rt.eval("globalThis.__v = valueToMidi({note: 60, extra: new Uint8Array([60, 62])})")
        .expect("an honest typed array must materialize under the ceiling");
    assert_eq!(
        rt.get_number("__v").expect("the honest value must convert"),
        60.0,
        "a typed array next to a note must not break the materialization"
    );
    rt.eval("globalThis.__v = valueToMidi(new String('abcd'))")
        .expect("an honest String wrapper must materialize under the ceiling");
    assert_eq!(
        rt.get_number("__v").expect("the honest value must convert"),
        36.0,
        "a String wrapper must materialize and fall back like any object"
    );
}

/// The host materializes every hap value a JS callback returns. A
/// `value: new Array(...)` past the cap empties the query (strudel parity
/// for a throwing callback) and reserves no unbudgeted host memory.
#[test]
fn a_huge_sparse_hap_value_empties_the_query_instead_of_reserving_host_memory() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let probe = materialize_cap() + 1;
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        &format!(
            "(_w) => (state) => [{{begin: state.span.begin, end: state.span.end, \
               value: globalThis.__huge ? new Array({probe}) : ['a', 'b']}}]"
        ),
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    rt.eval("globalThis.__huge = true").unwrap();
    let refused = rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
    let haps = refused.expect(
        "a throwing callback empties the query; the guard throws, it is not a typed limit refusal",
    );
    assert!(
        haps.is_empty(),
        "a {probe}-element sparse value produced {} haps: the host reservation is not bounded \
         by the heap ceiling",
        haps.len()
    );

    // Same runtime, same callback, honest value: materialization unchanged,
    // and the refusal did not poison the session.
    rt.eval("globalThis.__huge = false").unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("the runtime must still query after the refusal");
    assert_eq!(
        haps.len(),
        1,
        "the honest callback value produced no hap after a refusal"
    );
    assert!(
        matches!(&haps[0].value, Value::List(items) if items.len() == 2),
        "the honest value must materialize as a two-element list, got {}",
        haps[0].show()
    );
}

/// `stepBind` calls its callback at construction and materializes a
/// non-pattern return there. A sparse return past the cap becomes silence.
/// A materialized non-pattern fails construction (see `bind_ownership`), so
/// a successful evaluation shows the refusal. The counter shows that the
/// callback ran, and the empty query shows that nothing materialized.
#[test]
fn a_sparse_bind_return_past_the_ceiling_is_refused_without_failing_construction() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let probe = materialize_cap() + 1;
    rt.eval("globalThis.__bindCalls = 0").unwrap();
    rt.evaluate_score(
        &format!(
            "pure('bd').stepBind(x => {{ globalThis.__bindCalls++; return new Array({probe}); }})"
        ),
        &TranspileOptions::default(),
    )
    .expect("a refused sparse return must become silence, not a construction failure or an abort");
    let calls = rt.get_number("__bindCalls").expect("callback count") as u64;
    assert!(
        calls >= 1,
        "the stepBind callback never ran during construction; this test proves nothing"
    );

    // Querying the refused bind yields silence, not materialized haps.
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("the runtime must still query after a refused bind");
    assert!(
        haps.is_empty(),
        "a refused bind produced {} haps",
        haps.len()
    );

    // The runtime survives: the next edit plays normally.
    rt.evaluate_score(r#"s("bd")"#, &TranspileOptions::default())
        .expect("the runtime must still evaluate after a refused bind");
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("the runtime must still query after a refused bind");
    assert_eq!(haps.len(), 1, "the recovery score produced no hap");
}

/// Nested sparse arrays must share ONE budget: two inner arrays that each fit
/// under the cap must not add up to twice it (K inner arrays → K × cap of
/// host memory).
#[test]
fn nested_sparse_arrays_share_one_materialization_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    // Each inner array fits the cap alone; together they cannot.
    let inner = materialize_cap() / 2 + 1;
    let result = rt.eval(&format!(
        "valueToMidi([new Array({inner}), new Array({inner})])"
    ));
    let Err(message) = result else {
        panic!("two nested arrays of {inner} elements must exceed the shared budget")
    };
    assert!(
        message.contains("cannot materialize"),
        "the second nested array must hit the shared budget, got: {message}"
    );
}

/// Evaluate `expression` inside a JavaScript `try` and return what its
/// `catch` saw, as `"Name: message"` (or `"no throw"`). Only a refusal score
/// code can CATCH gets here: a host panic or abort never does.
fn caught(rt: &JsRuntime, expression: &str) -> String {
    rt.eval(&format!(
        "try {{ {expression}; globalThis.__caught = 'no throw'; }} \
         catch (e) {{ globalThis.__caught = `${{e.name}}: ${{e.message}}`; }}"
    ))
    .unwrap_or_else(|error| panic!("{expression} escaped its catch: {error}"));
    rt.get_string("__caught")
        .expect("the catch records a string")
}

/// QuickJS stores an array length past `i32::MAX` as a float64, and rquickjs'
/// `Array::len` asserts an int, so reading such a length through it PANICKED
/// before the guard could refuse it. Every way to write that length must
/// reach the guard as a catchable RangeError.
#[test]
fn a_sparse_length_past_i32_max_is_refused_instead_of_panicking() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    for array in [
        "new Array(2 ** 31)",
        "new Array(2 ** 32 - 1)",
        "Object.assign([], { length: 3e9 })",
    ] {
        let refusal = caught(&rt, &format!("valueToMidi({array})"));
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "valueToMidi({array}) must be refused by the heap-ceiling guard, got: {refusal}"
        );
    }
}

/// The same float64 length as a query callback's hap value: the query
/// empties, as it does for any throwing callback, instead of panicking the
/// host thread.
#[test]
fn a_hap_value_length_past_i32_max_empties_the_query_instead_of_panicking() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => [{begin: state.span.begin, end: state.span.end, \
           value: new Array(2 ** 32 - 1)}]",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("a throwing callback empties the query");
    assert!(
        haps.is_empty(),
        "a 2^32 - 1 sparse hap value produced {} haps",
        haps.len()
    );
}

/// The array a callback returns is itself JS-controlled: a length past
/// `i32::MAX` made rquickjs' `iter()` panic before any hap was read. It
/// empties the query like any other refused callback.
#[test]
fn a_callback_returning_a_length_past_i32_max_empties_the_query_instead_of_panicking() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let mut b = rt.builder();
    let id = b.callback(&rt, "(_w) => (state) => new Array(2 ** 31)");
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("a refused callback empties the query");
    assert!(
        haps.is_empty(),
        "a 2^31 sparse callback result produced {} haps",
        haps.len()
    );
}

/// The haps one callback returns share one materialization budget. Each of
/// these haps holds the same sparse array of just over half the cap. A
/// budget per hap accepts each one, so N haps reserve N/2 heap ceilings.
#[test]
fn haps_from_one_callback_share_one_materialization_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let inner = materialize_cap() / 2 + 1;
    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        &format!(
            "(_w) => (state) => {{ \
               const value = globalThis.__huge ? new Array({inner}) : ['a']; \
               return [0, 1, 2, 3].map(() => \
                 ({{begin: state.span.begin, end: state.span.end, value}})); }}"
        ),
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();

    rt.eval("globalThis.__huge = true").unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("a throwing callback empties the query");
    assert!(
        haps.is_empty(),
        "four haps sharing a {inner}-element sparse value produced {} haps: each hap \
         drew on its own budget",
        haps.len()
    );

    // The same four haps with an honest value all materialize.
    rt.eval("globalThis.__huge = false").unwrap();
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("the runtime must still query after the refusal");
    assert_eq!(haps.len(), 4, "the honest callback value lost haps");
}

/// Object property values draw on the same budget as array elements: two
/// sparse arrays reached through properties, each fitting the cap alone,
/// cannot sum past it.
#[test]
fn sparse_arrays_behind_object_properties_share_one_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let inner = materialize_cap() / 2 + 1;
    let refusal = caught(
        &rt,
        &format!("valueToMidi({{a: new Array({inner}), b: new Array({inner})}})"),
    );
    assert!(
        refusal.starts_with("RangeError: cannot materialize"),
        "the second property's array must hit the shared budget, got: {refusal}"
    );
}

/// The other callers that keep several materialized values together draw on
/// one budget as well: the controls of a `Pattern.query` State, and every
/// argument of an extension value callable.
#[test]
fn state_controls_and_callable_arguments_share_one_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let inner = materialize_cap() / 2 + 1;
    for call in [
        format!(
            "{{ const x = new Array({inner}); \
               pure(1).query(new State(new TimeSpan(0, 1), {{a: x, b: x}})); }}"
        ),
        format!("{{ const x = new Array({inner}); toMajorKey(x, x); }}"),
    ] {
        let refusal = caught(&rt, &call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call} must hit the shared budget, got: {refusal}"
        );
    }
}

/// The element cap for a site that mirrors a JS array as raw QuickJS values,
/// mirroring `js_element_cap::<rquickjs::Value>` at [`MATERIALIZE_CEILING`].
fn js_value_cap() -> usize {
    MATERIALIZE_CEILING / std::mem::size_of::<rquickjs::Value<'static>>()
}

/// Score globals that reserve one host slot per element of an array score
/// code hands them go through the same guard: a sparse length past the cap -
/// or past `i32::MAX` - is a catchable RangeError, not a host abort or panic.
#[test]
fn score_globals_refuse_a_sparse_length_past_the_ceiling() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let probe = js_value_cap() + 1;
    for call in [
        format!("zipWith((a, b) => a, new Array({probe}), [])"),
        // `pairs` reserves one slot fewer than the length.
        format!("pairs(new Array({}))", probe + 1),
        format!("objectMap(new Array({probe}), (x) => x)"),
        "zipWith((a, b) => a, new Array(2 ** 32 - 1), [])".to_string(),
        "pairs(new Array(2 ** 32 - 1))".to_string(),
        "objectMap(new Array(2 ** 32 - 1), (x) => x)".to_string(),
        "parray(new Array(2 ** 32 - 1))".to_string(),
        "gamepad().btnSequence(new Array(2 ** 32 - 1))".to_string(),
    ] {
        let refusal = caught(&rt, &call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call} must be refused by the heap-ceiling guard, got: {refusal}"
        );
    }

    // Honest arrays still go through.
    assert_eq!(
        caught(&rt, "zipWith((a, b) => a + b, [1, 2], [3, 4])"),
        "no throw"
    );
    assert_eq!(caught(&rt, "pairs([1, 2, 3])"), "no throw");
    assert_eq!(caught(&rt, "objectMap([1, 2], (x) => x)"), "no throw");
}

/// `register`'s name array is the same class of JS-controlled length: each
/// declared element pays a full `register_one` on the host, and a sparse
/// length past the cap must be the guard's catchable RangeError rather than a
/// loop the sandbox's budget cannot see.
#[test]
fn register_refuses_a_sparse_name_array_past_the_ceiling() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let probe = js_value_cap() + 1;
    let refusal = caught(&rt, &format!("register(new Array({probe}), x => x)"));
    assert!(
        refusal.starts_with("RangeError: cannot materialize"),
        "register(new Array({probe}), ...) must be refused by the heap-ceiling guard, \
         got: {refusal}"
    );

    // Honest arrays still go through, AND their names are actually
    // installed: a length read that yielded 0 would loop nothing and pass a
    // no-throw-only control. The four holes each register the name
    // "undefined" (overwriting the last); the real names land as callables
    // in the projected scope `register_one` publishes into.
    assert_eq!(caught(&rt, "register(new Array(4), x => x)"), "no throw");
    assert_eq!(
        installed_type(&rt, "undefined"),
        "function",
        "the holes of an honest sparse array must still be registered"
    );
    assert_eq!(caught(&rt, "register(['a', 'b'], x => x)"), "no throw");
    assert_eq!(installed_type(&rt, "a"), "function");
    assert_eq!(installed_type(&rt, "b"), "function");
}

/// `typeof rustelScope[name]`. `register_one` publishes into the projected
/// scope, so this shows that a name is installed, not only that the call
/// did not throw.
fn installed_type(rt: &JsRuntime, name: &str) -> String {
    rt.eval(&format!(
        "globalThis.__installed = typeof rustelScope['{name}']"
    ))
    .unwrap_or_else(|error| panic!("reading rustelScope['{name}'] failed: {error}"));
    rt.get_string("__installed").expect("a typeof is a string")
}

/// `register_one` recurses into nested name arrays, so the name tree of one
/// `register` call draws on one budget. Two nested arrays of cap/2+1 each
/// fit a cap per level; the shared budget refuses the second array with a
/// catchable RangeError.
#[test]
fn nested_register_name_arrays_share_one_materialization_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    // Each array fits the cap alone; together they cannot. The inner one
    // sits at index 0 of the outer, so the shared budget refuses it at the
    // first recursion, before a single hole is registered, where a fresh
    // cap per level would accept both and loop both sets of holes.
    let half = js_value_cap() / 2 + 1;
    let refusal = caught(
        &rt,
        &format!(
            "const outer = new Array({half}); outer[0] = new Array({half}); \
             register(outer, x => x)"
        ),
    );
    assert!(
        refusal.starts_with("RangeError: cannot materialize"),
        "a nested name array that only fits a fresh cap must hit the shared budget, \
         got: {refusal}"
    );
}

/// Each score global that walks a JavaScript array refuses a length past
/// `i32::MAX` with the guard's catchable RangeError. rquickjs' `len()` and
/// `iter()` panic on such a length (see `js_array_len`). The `2 ** 31 - 1`
/// probes fit an `i32`, so only the element budget refuses them.
#[test]
fn array_walking_globals_refuse_a_sparse_length_past_i32_max() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    // The sort-based walks run QuickJS' own `sort` first, which steps every
    // claimed index inside the engine; a 2^31 claim would spend seconds there
    // before the guard fired. A just-over-cap claim proves the same refusal
    // for a fraction of the walk.
    let just_over_cap = js_value_cap() + 1;
    for call in [
        "flatten(new Array(2 ** 31))".to_string(),
        "uniq(new Array(2 ** 32 - 1))".to_string(),
        format!("uniqsort(new Array({just_over_cap}))"),
        format!("uniqsortr(new Array({just_over_cap}))"),
        "removeUndefineds(new Array(2 ** 31))".to_string(),
        "averageArray(new Array(2 ** 31))".to_string(),
        "averageArray(new Array(2 ** 32 - 1))".to_string(),
        "chooseWith(0, new Array(2 ** 31))".to_string(),
        // Just under 2^31: the length fits an i32, so nothing asserts - the
        // budget refusal is the only guard left, and it must hold.
        "flatten(new Array(2 ** 31 - 1))".to_string(),
        "uniq(new Array(2 ** 31 - 1))".to_string(),
        "averageArray(new Array(2 ** 31 - 1))".to_string(),
        // `flatten`'s NESTED arrays go through the same guard, past and just
        // under `i32::MAX`, and all of them draw on ONE budget with the outer
        // array: two nested halves that each fit the cap cannot sum past it.
        "flatten([new Array(2 ** 31)])".to_string(),
        "flatten([new Array(2 ** 31 - 1)])".to_string(),
        format!(
            "flatten([new Array({half}), new Array({half})])",
            half = js_value_cap() / 2 + 1
        ),
    ] {
        let refusal = caught(&rt, &call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call} must be refused by the heap-ceiling guard, got: {refusal}"
        );
    }

    // Honest arrays still go through.
    assert_eq!(caught(&rt, "flatten([1, [2, 3], 4])[1]"), "no throw");
    assert_eq!(caught(&rt, "uniq([1, 1, 2])[1]"), "no throw");
    assert_eq!(caught(&rt, "uniqsort([3, 1, 2])[0]"), "no throw");
    assert_eq!(caught(&rt, "removeUndefineds([1, , 2])[1]"), "no throw");
    assert_eq!(caught(&rt, "averageArray([1, 2, 3])"), "no throw");
    assert_eq!(caught(&rt, "chooseWith(0, ['a', 'b'])"), "no throw");
    // The empty-array TypeError survives: the guard refuses nothing an honest
    // empty walk would have thrown anyway.
    assert_eq!(
        caught(&rt, "averageArray([])"),
        "TypeError: reduce of empty array with no initial value"
    );
}

/// Install an indexed setter on `Array.prototype` that stores the value it
/// intercepts and then re-lengthens the array to `2 ** 31`. rquickjs'
/// `Array::set` honours it - the fast path is skipped once the prototype has
/// an indexed property - so a host list filled that way keeps its elements
/// but carries a float64 length that `len()`/`iter()` panic on. The setter
/// pollutes the whole realm, so each probe gets a fresh runtime.
fn runtime_with_a_re_lengthening_array_setter(setup: &str) -> JsRuntime {
    runtime_re_lengthening_arrays_to("2 ** 31", setup)
}

/// [`runtime_with_a_re_lengthening_array_setter`], re-lengthening every array
/// the setter sees to `length` instead.
fn runtime_re_lengthening_arrays_to(length: &str, setup: &str) -> JsRuntime {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.install_voicings_prebake().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();
    rt.eval(setup).unwrap();
    rt.eval(&format!(
        "Object.defineProperty(Array.prototype, 0, {{ configurable: true, \
           set(value) {{ \
             Object.defineProperty(this, 0, \
               {{ value, writable: true, enumerable: true, configurable: true }}); \
             this.length = {length}; }} }})"
    ))
    .unwrap();
    rt
}

/// `cat`/`stack`/`fastcat` flatten through the private spread helper, which
/// COPIES the argument array before Rust reads it. That copy is filled
/// through `Array::set`, so an `Array.prototype` setter can re-length it past
/// `i32::MAX`, and reading it through `iter()` PANICKED. The copy is read
/// through the guard and refused instead.
#[test]
fn list_constructors_refuse_a_copy_re_lengthened_past_i32_max() {
    for call in ["cat(['a'])", "fastcat(['a'])", "stack(['a'])"] {
        let rt = runtime_with_a_re_lengthening_array_setter("");
        let refusal = caught(&rt, call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call} must be refused by the heap-ceiling guard, got: {refusal}"
        );
    }

    // A sparse 2^31 argument itself dies earlier, inside the engine's copy,
    // as a catchable allocation refusal; honest lists still build.
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();
    for call in [
        "cat(new Array(2 ** 31))",
        "fastcat(new Array(2 ** 31))",
        "stack(new Array(2 ** 31))",
    ] {
        let refusal = caught(&rt, call);
        assert!(
            refusal != "no throw",
            "{call} must be refused, not materialized"
        );
    }
    assert_eq!(caught(&rt, "cat(['a', 'b'])"), "no throw");
    assert_eq!(caught(&rt, "stack(['a', 'b'])"), "no throw");
}

/// The stepwise combinators charge a lane count from `js_array_len` before
/// they read an element. A 2^31 length is refused as a typed limit pattern,
/// the refusal shape of these combinators. `polymeter` reads its nested
/// lane inside its walk, not in the top-level fold.
#[test]
fn stepwise_combinators_refuse_a_sparse_length_as_typed_limits() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    for (score, operation) in [
        ("polymeter(new Array(2 ** 31))", "polymeter"),
        ("polymeter([new Array(2 ** 31)])", "polymeter"),
        ("stepalt(new Array(2 ** 31))", "stepalt"),
        ("s('bd').tour(new Array(2 ** 31))", "tour"),
    ] {
        rt.evaluate_score(score, &TranspileOptions::default())
            .unwrap_or_else(|error| {
                panic!("{score} must refuse by typed limit, not by panicking: {error}")
            });
        let refusal = rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
        assert!(
            matches!(
                &refusal,
                Err(QueryError::Limit(rustel_core::QueryLimit::StepwiseExpansion {
                    operation: refused,
                    ..
                })) if *refused == operation
            ),
            "{score} must query as the {operation} stepwise limit, got: {refusal:?}"
        );
    }

    // The session still works afterwards.
    rt.evaluate_score(r#"s("bd")"#, &TranspileOptions::default())
        .expect("the runtime must survive the refused lane counts");
}

/// Score code can forge the `__rustel_combinator` tag, which is a plain
/// property. A forged tag whose `args` cannot be walked safely fails closed:
/// the function bridges as an ordinary callback.
#[test]
fn forged_combinator_tags_with_huge_args_are_bridged_not_panicked() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.evaluate_score(
        "{ const direct = () => {}; \
           direct.__rustel_combinator = {name: 'rev', args: new Array(2 ** 31)}; \
           const nested = () => {}; \
           nested.__rustel_combinator = {name: 'rev', args: [direct]}; \
           pure('bd').every(1, direct); \
           pure('bd').every(1, nested); \
           pure('ok') }",
        &TranspileOptions::default(),
    )
    .expect("a forged tag must fail closed into the callback bridge, not panic");

    rt.evaluate_score(r#"s("bd")"#, &TranspileOptions::default())
        .expect("the runtime must survive forged tags");
}

// -- length claims past the budget, and claims that move mid-walk ------------
//
// The guard above refuses a length it reads. These probes cover the claims it
// used to miss: a JS array a combinator partial binds BY REFERENCE, a forged
// tag's args just under `i32::MAX`, a length a getter re-writes after it was
// charged, the score globals still on rquickjs' asserting `len()`, and host
// copies an `Array.prototype` setter re-lengths.

/// Install `counted(length)`: a sparse array of `length` whose index 0 counts
/// its reads in `__reads`. A walk that refuses BEFORE it starts leaves the
/// count at zero; any walk of the claim reads index 0 first.
fn install_counted_arrays(rt: &JsRuntime) {
    rt.eval(
        "globalThis.__reads = 0; \
         globalThis.counted = (length) => { \
           const array = new Array(length); \
           Object.defineProperty(array, 0, { get() { globalThis.__reads++; return 1; } }); \
           return array; }",
    )
    .unwrap();
}

fn reads(rt: &JsRuntime) -> f64 {
    rt.get_number("__reads").expect("the read counter")
}

/// Evaluate `score`, in which `counting(partial)` wraps a combinator partial
/// in a Proxy that counts every call made through it, and return the count.
/// A partial rebuilt natively is never called through JavaScript; one
/// bridged as a callback is - these combinators apply it at construction.
/// (Purity cannot tell them apart here: the eager application leaves a pure
/// graph either way.)
fn calls_through_js(rt: &JsRuntime, score: &str) -> f64 {
    rt.eval(
        "globalThis.__calls = 0; \
         globalThis.counting = (partial) => new Proxy(partial, { \
           apply(target, self, args) { \
             globalThis.__calls++; return Reflect.apply(target, self, args); } })",
    )
    .unwrap();
    rt.evaluate_score(score, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{score} must evaluate: {error}"));
    rt.get_number("__calls").expect("the call counter")
}

/// A partial application binds its arguments by reference:
/// `fast(new Array(2 ** 31))` is tagged `{name: 'fast', args: [that array]}`.
/// The native rebuild charges the bound array to the element budget before
/// it walks the array. Past the budget the function stays a bridged callback.
#[test]
fn arrays_bound_by_a_combinator_partial_are_charged_before_the_rebuild_walks_them() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();
    install_counted_arrays(&rt);

    let probe = materialize_cap() + 1;
    for score in [
        // Just past the budget first: without the charge these are walked
        // (and read) in full, quickly.
        format!("s('bd').jux(fast(counted({probe})))"),
        format!("s('bd').superimpose(fast(counted({probe})))"),
        format!("s('bd').jux(fast({{x: counted({probe})}}))"),
        // Past `i32::MAX`, bound by the real curry and by a forged tag, and
        // behind an object property.
        "s('bd').jux(fast(counted(2 ** 31)))".to_string(),
        "pure('bd').every(1, fast(counted(2 ** 31)))".to_string(),
        "{ const f = () => {}; \
           f.__rustel_combinator = {name: 'rev', args: [counted(2 ** 31)]}; \
           pure('bd').every(1, f) }"
            .to_string(),
        "{ const f = () => {}; \
           f.__rustel_combinator = {name: 'fast', args: [counted(2 ** 31)]}; \
           pure('bd').every(1, f) }"
            .to_string(),
        "{ const f = () => {}; \
           f.__rustel_combinator = {name: 'fast', args: [{x: counted(2 ** 31)}]}; \
           pure('bd').every(1, f) }"
            .to_string(),
    ] {
        if let Err(error) = rt.evaluate_score(&score, &TranspileOptions::default()) {
            assert!(
                error.to_string().contains("cannot materialize"),
                "{score} failed some other way than the guard's refusal: {error}"
            );
        }
        assert_eq!(
            reads(&rt),
            0.0,
            "{score}: the bound array was walked instead of charged first"
        );
    }

    // An honest bound array is still rebuilt natively, and it plays. A
    // bridged callback would play too.
    assert_eq!(
        calls_through_js(&rt, "s('bd').jux(counting(fast([1, 2])))"),
        0.0,
        "the honest bound array was bridged instead of rebuilt natively"
    );
    let haps = rt
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("the honest partial must query");
    assert!(!haps.is_empty(), "the honest partial produced no haps");
}

/// The rebuild charges one budget for everything it mirrors. Two bound
/// arrays that each fit the cap alone, but not together, fail closed: the
/// partial is bridged as a callback and not rebuilt natively.
#[test]
fn a_partial_s_bound_arrays_share_one_rebuild_budget() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    assert_eq!(
        calls_through_js(&rt, "pure('bd').every(2, counting(euclid([3], [8])))"),
        0.0,
        "two honest bound arrays must rebuild natively"
    );
    let half = materialize_cap() / 2 + 1;
    assert_eq!(
        calls_through_js(
            &rt,
            &format!("pure('bd').every(2, counting(euclid(new Array({half}), [8])))")
        ),
        0.0,
        "one bound array under the cap must rebuild natively"
    );
    assert!(
        calls_through_js(
            &rt,
            &format!("pure('bd').every(2, counting(euclid(new Array({half}), new Array({half}))))")
        ) > 0.0,
        "two bound arrays that only fit the cap separately were rebuilt natively"
    );
}

/// A curry calls through when `arity` arguments are bound, so a genuine tag
/// holds fewer. A tag with `arity` or more `args` is forged and fails closed
/// before an element is read, at any nesting depth. The walk over
/// genuine-shaped tags draws on the element budget of the rebuild.
#[test]
fn forged_combinator_tags_fail_closed_before_walking_their_args() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();
    install_counted_arrays(&rt);

    for score in [
        // `rev` takes only the pattern: three bound args is no curry's tag.
        "{ const f = () => {}; \
           f.__rustel_combinator = {name: 'rev', args: counted(3)}; \
           pure('bd').every(1, f) }"
            .to_string(),
        // The same forged tag NESTED in a genuine-shaped one: the walkers
        // that inspect nested tags apply the same rule.
        "{ const inner = () => {}; \
           inner.__rustel_combinator = {name: 'rev', args: counted(3)}; \
           const outer = () => {}; \
           outer.__rustel_combinator = {name: 'fast', args: [inner]}; \
           pure('bd').every(1, outer) }"
            .to_string(),
        "{ const f = () => {}; \
           f.__rustel_combinator = {name: 'rev', args: counted(2 ** 31 - 1)}; \
           pure('bd').every(1, f) }"
            .to_string(),
        "{ const inner = () => {}; \
           inner.__rustel_combinator = {name: 'rev', args: counted(2 ** 31 - 1)}; \
           const outer = () => {}; \
           outer.__rustel_combinator = {name: 'fast', args: [inner]}; \
           pure('bd').every(1, outer) }"
            .to_string(),
        // `echoWith`'s index-2 transformer probes the tag on its own path.
        "{ const f = () => {}; \
           f.__rustel_combinator = {name: 'rev', args: counted(2 ** 31 - 1)}; \
           pure('bd').echoWith(2, 1 / 8, f) }"
            .to_string(),
    ] {
        rt.evaluate_score(&score, &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("{score} must fail closed into the bridge: {error}"));
        assert_eq!(
            reads(&rt),
            0.0,
            "{score}: the forged args were walked before the tag was refused"
        );
    }

    // Genuine-shaped tags can still share their args: a chain of `every`
    // partials each binding the next one twice is a DAG whose tree walk
    // doubles per level. The walk charges every args array to the one
    // rebuild budget, so it stops within that budget instead of visiting
    // 2^40 nodes.
    rt.eval("globalThis.__reads = 0").unwrap();
    rt.evaluate_score(
        "{ let next = 1; \
           for (let level = 0; level < 40; level++) { \
             const shared = next; const args = [shared, shared]; \
             Object.defineProperty(args, 0, \
               { get() { globalThis.__reads++; return shared; } }); \
             const f = () => {}; \
             f.__rustel_combinator = {name: 'every', args}; \
             next = f; } \
           pure('bd').every(1, next) }",
        &TranspileOptions::default(),
    )
    .expect("a forged DAG must fail closed into the bridge");
    assert!(
        reads(&rt) <= materialize_cap() as f64,
        "a forged DAG was walked {} times, past the {}-element rebuild budget",
        reads(&rt),
        materialize_cap()
    );

    // Genuine partials are still rebuilt natively.
    rt.evaluate_score("s('bd sd').every(1, rev)", &TranspileOptions::default())
        .expect("a genuine tag must still evaluate");
    assert!(
        rt.active_pattern().is_some_and(|pattern| pattern.is_pure()),
        "a genuine `rev` reference no longer rebuilds natively"
    );
}

/// `tour` charges one expansion from the widths it reads, then walks exactly
/// those widths. A getter on an earlier array that re-lengthens a later one
/// does not change the expansion.
#[test]
fn tour_walks_exactly_the_widths_it_charged() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    let shown = |score: &str| -> Vec<String> {
        rt.evaluate_score(score, &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("{score} must evaluate: {error}"));
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .unwrap_or_else(|error| panic!("{score} must query: {error:?}"))
            .iter()
            .map(|hap| hap.show())
            .collect()
    };
    let honest = shown("s('bd').tour(['y'], ['x'])");
    assert!(!honest.is_empty(), "the honest tour produced no haps");

    for length in ["3", "2 ** 31"] {
        let score = format!(
            "(() => {{ const later = ['x']; const earlier = []; \
               Object.defineProperty(earlier, 0, {{ \
                 get() {{ later.length = {length}; return 'y'; }}, enumerable: true }}); \
               return s('bd').tour(earlier, later); }})()"
        );
        assert_eq!(
            shown(&score),
            honest,
            "a getter that re-lengthens a later tour array to {length} changed the expansion"
        );
    }
}

/// Three score globals still read a score's array length through rquickjs'
/// `len()`, which asserts an `i32` and PANICS past it. Each now reads it
/// through the guard; the two that walk every claimed element also charge it.
#[test]
fn length_reading_score_globals_refuse_past_i32_max() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.install_voicings_prebake().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    for (call, refusal) in [
        // Only the `== 2` shape test reads the length; the three-slot form
        // then refuses its missing start.
        (
            "seqPLoop(new Array(2 ** 31))",
            "RangeError: seqPLoop start must be finite",
        ),
        (
            "register(new Array(2 ** 31), (x, pat) => pat)",
            "RangeError: cannot materialize",
        ),
        (
            "register(new Array(2 ** 31 - 1), (x, pat) => pat)",
            "RangeError: cannot materialize",
        ),
        (
            "voicingAlias('m', 'x', new Array(2 ** 31))",
            "RangeError: cannot materialize",
        ),
        (
            "voicingAlias('m', 'x', new Array(2 ** 31 - 1))",
            "RangeError: cannot materialize",
        ),
    ] {
        let caught = caught(&rt, call);
        assert!(
            caught.starts_with(refusal),
            "{call} must be refused with {refusal:?}, got: {caught}"
        );
    }

    // Honest calls still go through.
    assert_eq!(caught(&rt, "seqPLoop([0, 1, 'a'], [2, 'b'])"), "no throw");
    assert_eq!(
        caught(&rt, "register(['aliasOne', 'aliasTwo'], (x, pat) => pat)"),
        "no throw"
    );
    assert_eq!(
        caught(&rt, "voicingAlias('m', 'mAlias', [{m: 1}])"),
        "no throw"
    );
}

/// `arrange` refuses with a catchable RangeError when section cycles that fit
/// one by one sum past the native fraction range, as it does for non-finite
/// cycles.
#[test]
fn arrange_total_cycle_overflow_refuses_instead_of_panicking() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    let refusal = caught(&rt, "arrange([1e38, 'bd'], [1e38, 'sd'])");
    assert_eq!(refusal, "RangeError: arrangement total cycles overflow");

    // An honest arrangement still evaluates.
    assert_eq!(caught(&rt, "arrange([4, 'bd'], [2, 'sd'])"), "no throw");
}

/// A `seqPLoop` section bound that leaves the native fraction range once
/// divided by the total throws a catchable RangeError. When both scaled bounds
/// fit but their difference does not, the query refuses as
/// `NativeFraction { operation: "compress" }`.
#[test]
fn seqploop_section_bounds_past_the_fraction_range_refuse_instead_of_panicking() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    assert_eq!(
        caught(&rt, "seqPLoop([1e38, 1e-7, 'bd'])"),
        "RangeError: seqPLoop section bounds exceed the native fraction range"
    );

    rt.evaluate_score(
        "seqPLoop([1/9999991, 1/9999973, 'a'], [0, 1e30, 'b'])",
        &TranspileOptions::default(),
    )
    .expect("bounds that fit evaluate");
    assert!(matches!(
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(rustel_core::QueryLimit::NativeFraction {
            operation: "compress"
        }))
    ));

    // Honest calls still go through.
    assert_eq!(caught(&rt, "seqPLoop([0, 1, 'a'], [2, 'b'])"), "no throw");
}

/// A hap's `context.locations` comes from score code. Its length is charged
/// to the element budget that the callback's haps share. A length past the
/// budget, alone or summed, or past `i32::MAX`, empties the query.
#[test]
fn hap_context_locations_past_i32_max_empty_the_query_instead_of_panicking() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    let mut b = rt.builder();
    let id = b.callback(
        &rt,
        "(_w) => (state) => globalThis.__locations.map((locations) => \
           ({begin: state.span.begin, end: state.span.end, value: 1, context: {locations}}))",
    );
    rt.set_active(&b, rustel_core::js_query(id)).unwrap();
    let haps = |locations: &str| {
        rt.eval(&format!("globalThis.__locations = {locations}"))
            .unwrap();
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("a refused callback empties the query")
            .len()
    };

    let half = materialize_cap() / 2 + 1;
    for locations in [
        format!("[new Array({})]", materialize_cap() + 1),
        format!("[new Array({half}), new Array({half})]"),
        "[new Array(2 ** 31 - 1)]".to_string(),
        "[new Array(2 ** 31)]".to_string(),
    ] {
        assert_eq!(
            haps(&locations),
            0,
            "locations {locations} were walked instead of refused"
        );
    }

    // Honest locations still attach.
    assert_eq!(haps("[[{start: 0, end: 1}], [{start: 1, end: 2}]]"), 2);
}

/// The other host-built arrays a score can re-length the same way - spread
/// copies and `Array::set`-filled lists that Rust reads back - each refuse
/// the float64 length instead of panicking in `len()`/`iter()`.
#[test]
fn host_built_arrays_re_lengthened_past_i32_max_are_refused() {
    for (setup, call, expected) in [
        // A one-input method given several arguments sequences them, and
        // each array argument goes through the spread copy.
        (
            "",
            "pure('a').fast([1], 2)",
            "RangeError: cannot materialize",
        ),
        // The shrink body spreads the list its (overridable) `shrinklist`
        // returned.
        (
            "globalThis.shrunk = s('a b'); \
             shrunk.shrinklist = function () { return [this]; };",
            "shrunk.shrink(1)",
            "RangeError: cannot materialize",
        ),
        // The lookup-shape helpers' entries lists.
        ("", "s('0').pick(['a'])", "RangeError: cannot materialize"),
        (
            "",
            "squeeze(pure(0), ['a'])",
            "RangeError: cannot materialize",
        ),
        // A registered function's fast path collects its pure arguments'
        // source locations into a list.
        (
            "register('fastLoc', (x, pat) => pat.fast(x)); \
             globalThis.located = pure(2); \
             located.__pure_loc = { start: 0, end: 1 };",
            "s('a').fastLoc(located)",
            "RangeError: cannot materialize",
        ),
        // `voicingAlias` wraps a single set in a one-element list.
        (
            "",
            "voicingAlias('m', 'x', {})",
            "RangeError: cannot materialize",
        ),
        // `mapArgs` copies its arguments into a list; the walk covers the
        // arguments it copied, whatever the copy's length became.
        ("", "mapArgs((...xs) => xs.length, (x) => x)(1)", "no throw"),
    ] {
        let rt = runtime_with_a_re_lengthening_array_setter(setup);
        let caught = caught(&rt, call);
        assert!(
            caught.starts_with(expected),
            "{call} must end in {expected:?}, got: {caught}"
        );
    }
}

/// A list constructor copies every nested level of its arguments before any
/// level is finished, so all of those copies are alive together, and they
/// draw on ONE element budget. Re-lengthened by the `Array.prototype` setter
/// to just over half the cap, each copy fits alone; two nested levels do
/// not. A fresh budget per level let a few hundred such levels each claim a
/// heap ceiling of host memory.
#[test]
fn nested_list_copies_share_one_budget() {
    let value_half = js_value_cap() / 2 + 1;
    for call in ["cat([['a']])", "fastcat([['a']])", "stack([['a']])"] {
        let rt = runtime_re_lengthening_arrays_to(&value_half.to_string(), "");
        let refusal = caught(&rt, call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call}: two nested copies of {value_half} must exceed the shared budget, got: {refusal}"
        );
    }

    // Sequenced arguments reify nested lists through the same kind of copy,
    // budgeted in host Patterns.
    let pattern_half = MATERIALIZE_CEILING / std::mem::size_of::<Pattern>() / 2 + 1;
    let rt = runtime_re_lengthening_arrays_to(&pattern_half.to_string(), "");
    let refusal = caught(&rt, "pure('a').fast([[1]], 2)");
    assert!(
        refusal.starts_with("RangeError: cannot materialize"),
        "two nested sequence copies of {pattern_half} must exceed the shared budget, got: {refusal}"
    );

    // One level of the same length still fits.
    let rt = runtime_re_lengthening_arrays_to("2", "");
    assert_eq!(caught(&rt, "cat([['a']])"), "no throw");
    assert_eq!(caught(&rt, "pure('a').fast([[1]], 2)"), "no throw");
}

/// Modern `polymeter` queries the same under an `Array.prototype` setter that
/// re-lengthens arrays as without it, and reads no slot past its operands.
#[test]
fn modern_polymeter_is_untouched_by_an_array_prototype_setter() {
    // Counts reads of index 2, which no array in these calls owns; any walk
    // past their operands reads it through the prototype.
    const COUNT_READS_PAST_THE_OPERANDS: &str = "globalThis.__reads = 0; \
        Object.defineProperty(Array.prototype, 2, \
          { configurable: true, get() { globalThis.__reads++; } })";
    let outcome = |rt: &JsRuntime, score: &str| {
        rt.evaluate_score(score, &TranspileOptions::default())
            .map_err(|error| error.to_string())?;
        rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .map(|haps| haps.iter().map(|hap| hap.show()).collect::<Vec<_>>())
            .map_err(|error| format!("{error:?}"))
    };
    let plain = JsRuntime::new().unwrap();
    plain.install_semantic_bindings().unwrap();
    plain.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    for (score, plays) in [
        // Mini-notation lanes of two steps and one, so the result is paced.
        (r#"polymeter(s("a b"), s("c"))"#, true),
        // Every operand is filtered out, so the result is silence.
        ("polymeter('raw')", false),
        ("polymeter(sine)", false),
    ] {
        let expected = outcome(&plain, score);
        assert!(
            expected.as_ref().is_ok_and(|haps| haps.is_empty() != plays),
            "{score} without the setter is not the expected baseline: {expected:?}"
        );
        for length in ["1 << 20", "2 ** 32 - 1"] {
            let rt = runtime_re_lengthening_arrays_to(length, COUNT_READS_PAST_THE_OPERANDS);
            assert_eq!(
                outcome(&rt, score),
                expected,
                "{score}: a setter re-lengthening arrays to {length} changed the result"
            );
            assert_eq!(
                reads(&rt),
                0.0,
                "{score}: a setter re-lengthening arrays to {length} made the walk read past the operands"
            );
        }
    }
}

/// A modern `polymeter` lane count past the stepwise cap is refused as the
/// typed stepwise limit before any lane is unwrapped, so a non-Pattern last
/// lane cannot turn it into a TypeError.
#[test]
fn modern_polymeter_refuses_too_many_lanes_before_unwrapping_one() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();

    let lanes = rustel_core::MAX_STEPWISE_ENTRIES + 1;
    rt.evaluate_score(
        &format!(
            "polymeter(...Array({lanes} - 1).fill(pure(1)), \
               {{ hasSteps: true, pace: pure(1).pace }})"
        ),
        &TranspileOptions::default(),
    )
    .expect("too many lanes must evaluate to a typed refusal, not throw");
    let refusal = rt.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE);
    assert!(
        matches!(
            &refusal,
            Err(QueryError::Limit(rustel_core::QueryLimit::StepwiseExpansion {
                operation: "polymeter",
                minimum_entries,
                ..
            })) if *minimum_entries == lanes
        ),
        "{lanes} lanes must query as the polymeter stepwise limit, got: {refusal:?}"
    );
}

/// `pick`/`squeeze` build their lookup entries from the result of the
/// lookup's own `map`, which a score can replace with anything that has a
/// `length`. The length is read once and charged before a walk that runs no
/// JavaScript, so `{length: Infinity}` is refused.
#[test]
fn lookup_entries_charge_the_length_their_map_returns() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    for length in [(js_value_cap() + 1).to_string(), "Infinity".to_string()] {
        for call in ["s('0').pick(lookup)", "squeeze(pure(0), lookup)"] {
            let refusal = caught(
                &rt,
                &format!("const lookup = ['x']; lookup.map = () => ({{length: {length}}}); {call}"),
            );
            assert!(
                refusal.starts_with("RangeError: cannot materialize"),
                "{call} with a map result of length {length} must be refused, got: {refusal}"
            );
        }
    }

    assert_eq!(caught(&rt, "s('0').pick(['x'])"), "no throw");
    assert_eq!(caught(&rt, "squeeze(pure(0), ['x'])"), "no throw");
}

/// A hydra call records at most 16 arguments, and a score that swaps
/// `Proxy` can hand the recorder's `apply` trap any array. Its length is
/// held to that limit BEFORE a single element is copied into host slots.
#[cfg(feature = "hydra")]
#[test]
fn hydra_call_arguments_are_refused_before_they_are_copied() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();
    install_counted_arrays(&rt);

    rt.evaluate_score_with_effects_cancellable(
        "await initHydra(); \
         const Real = Proxy; let target; let handler; \
         globalThis.Proxy = function (t, h) { target = t; handler = h; return new Real(t, h); }; \
         osc.rotate(); \
         globalThis.Proxy = Real; \
         try { handler.apply(target, undefined, counted(17)); globalThis.__caught = 'no throw'; } \
         catch (e) { globalThis.__caught = `${e.name}: ${e.message}`; } \
         s('bd')",
        &TranspileOptions::default(),
        Duration::from_secs(5),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .expect("the hydra score must evaluate");
    let caught = rt
        .get_string("__caught")
        .expect("the catch records a string");
    assert!(
        caught.contains("at most 16 hydra arguments"),
        "17 recorded arguments must be refused, got: {caught}"
    );
    assert_eq!(
        reads(&rt),
        0.0,
        "the arguments were copied before the limit refused them"
    );
}

/// `listRange` reads no array: its length is arithmetic on score-supplied
/// numbers. That length goes through the same element-cap reservation, so a
/// range past the ceiling is a catchable RangeError.
#[test]
fn list_range_refuses_a_numeric_length_past_the_ceiling() {
    let rt = JsRuntime::new().unwrap();
    rt.install_semantic_bindings().unwrap();
    rt.set_memory_limit(MATERIALIZE_CEILING).unwrap();

    // `length` is `max - min + 1`, so this range is one element past the cap
    // the guard derives from the live ceiling.
    let probe = js_value_cap();
    for call in [
        format!("listRange(0, {probe})"),
        // The reproduction from the report.
        "listRange(0, 2600000000)".to_string(),
        // The largest length the `u32::MAX` pre-check still admits.
        "listRange(0, 4294967294)".to_string(),
    ] {
        let refusal = caught(&rt, &call);
        assert!(
            refusal.starts_with("RangeError: cannot materialize"),
            "{call} must be refused by the heap-ceiling guard, got: {refusal}"
        );
    }

    // An honest range still goes through, whole and in order: the element
    // loop was rewritten along with the reservation.
    rt.eval("globalThis.__range = listRange(0, 10).length")
        .unwrap();
    assert_eq!(rt.get_number("__range").unwrap(), 11.0);
    rt.eval("globalThis.__values = listRange(2, 5).join(',')")
        .unwrap();
    assert_eq!(rt.get_string("__values").unwrap(), "2,3,4,5");
}

/// Giving back the names a score's `register()` displaced happens before
/// the next score's deadline starts, so it runs no score code: a setter the
/// previous score left on `Object.prototype` for a registered name is not
/// called, and the next score ends within its own budget.
#[test]
fn restoring_a_registered_name_runs_no_inherited_setter() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    const ARMS_A_SETTER: &str = "globalThis.armed = false; \
        Object.defineProperty(Object.prototype, 'zz', { configurable: true, \
          get() { return 1 }, \
          set(v) { if (globalThis.armed) while (true) {} \
            Object.defineProperty(this, 'zz', { value: v, writable: true, configurable: true }) } }); \
        register('zz', p => p); \
        globalThis.armed = true; \
        pure('a')";

    // The realm lives on its own thread, so a turn that never returns fails
    // the test instead of hanging it.
    let (first_done, first) = mpsc::channel();
    let (second_done, second) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = JsRuntime::new().unwrap();
        rt.install_semantic_bindings().unwrap();
        let options = TranspileOptions::default();
        let cancellation = AtomicBool::new(false);
        let outcome = rt
            .evaluate_score_with_effects_cancellable(
                ARMS_A_SETTER,
                &options,
                Duration::from_secs(5),
                &cancellation,
            )
            .map(|_| ())
            .map_err(|error| format!("{error:?}"));
        first_done.send(outcome).unwrap();
        let outcome = rt
            .evaluate_score_with_effects_cancellable(
                "pure('b')",
                &options,
                Duration::from_millis(100),
                &cancellation,
            )
            .map(|_| ())
            .map_err(|error| format!("{error:?}"));
        let _ = second_done.send(outcome);
    });

    first
        .recv_timeout(Duration::from_secs(60))
        .expect("the first score returns")
        .expect("the first score evaluates");
    let started = std::time::Instant::now();
    let outcome = second
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "the next score did not end within its 100 ms budget; the restore ran score code"
            )
        });
    assert!(
        outcome.is_ok()
            || outcome
                .as_ref()
                .is_err_and(|error| error.starts_with("Limit(")),
        "the next score must finish or be refused by a limit, got {outcome:?} after {:?}",
        started.elapsed()
    );
}
