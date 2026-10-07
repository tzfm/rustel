//! Bounded synchronous JavaScript query turns.
//!
//! These are resource-boundary tests, not realtime claims. One deadline spans
//! an outer direct query and every reentrant callback it reaches; runnable
//! microtasks are refused and discarded rather than executed.

use rustel_core::{Pattern, QueryLimit, Value, fastcat, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{DEFAULT_QUERY_JS_BUDGET, JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const QUERY_DEADLINE_CHILD: &str = "RUSTEL_QUERY_DEADLINE_CHILD";
const LEGACY_QUERY_DEADLINE_CHILD: &str = "RUSTEL_LEGACY_QUERY_DEADLINE_CHILD";

fn atom(value: &str) -> Pattern {
    pure(Value::Str(value.into()))
}

fn sequence() -> Pattern {
    fastcat(vec![atom("bd"), atom("sd")])
}

fn assert_clean(runtime: &JsRuntime, label: &str) {
    assert_eq!(runtime.query_depth(), 0, "{label}: query stack leaked");
    assert_eq!(
        rustel_jsruntime::bridge_frame_depth(),
        0,
        "{label}: bridge frame leaked"
    );
    assert_eq!(
        rustel_jsruntime::bridge_scratch_len(),
        0,
        "{label}: bridge scratch leaked"
    );
    assert!(!runtime.jobs_pending(), "{label}: pending job leaked");
}

fn semantic_runtime(source: &str) -> JsRuntime {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("could not install `{source}`: {error}"));
    runtime
}

fn query_active(
    runtime: &JsRuntime,
    begin: Fraction,
    end: Fraction,
) -> Result<Vec<rustel_core::Hap>, QueryError> {
    runtime.query_cancellable(
        Slot::Active,
        0,
        begin,
        end,
        Duration::from_secs(5),
        &AtomicBool::new(false),
    )
}

#[test]
fn continuous_arithmetic_query_keeps_wholeless_haps_across_cycles() {
    for source in ["s(rand.mul(3))", "s(rand.range(0, 4))", "s(irand(3))"] {
        let runtime = semantic_runtime(source);
        for cycles in [1, 2, 3] {
            let haps = query_active(&runtime, Fraction::ZERO, Fraction::int(cycles))
                .expect("query continuous signal");
            assert_eq!(haps.len(), cycles as usize, "{source} over {cycles} cycles");
            assert!(haps.iter().all(|hap| hap.whole.is_none()));
        }
    }

    let runtime = semantic_runtime("s(rand.mul(3))");
    let haps = runtime
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::int(2),
            &[("_cps", 0.5)],
        )
        .expect("scheduler query continuous signal");
    assert_eq!(haps.len(), 2);
    assert!(haps.iter().all(|hap| hap.whole.is_none()));

    let explicitly_sorted = semantic_runtime("s(rand.mul(3)).sortHapsByPart()");
    assert!(
        query_active(&explicitly_sorted, Fraction::ZERO, Fraction::int(2))
            .expect("explicit comparator throw is caught")
            .is_empty()
    );
    assert!(explicitly_sorted.take_query_throw().is_some());
}

#[test]
fn controls_query_accepts_the_native_active_wrapper() {
    let runtime = semantic_runtime(r#"s("bd")"#);
    let haps = runtime
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
        )
        .expect("query native active Pattern with scheduler controls");
    assert_eq!(haps.len(), 1);
    assert_eq!(haps[0].value.get("s").and_then(Value::as_str), Some("bd"));
}

fn enter_supervised_child(test_name: &str, marker: &str, timeout: Duration) -> bool {
    if std::env::var_os(marker).is_some() {
        return true;
    }

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(marker, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn supervised query child");
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll supervised query child") {
            let output = child
                .wait_with_output()
                .expect("collect supervised query child");
            assert!(
                status.success(),
                "supervised query child failed with {status}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return false;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill hung supervised query child");
            let output = child
                .wait_with_output()
                .expect("reap supervised query child");
            panic!(
                "supervised query child exceeded {timeout:?}:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

const QUERY_HOST_ENTRY_CASES: &[(&str, &str)] = &[
    ("value", "pure('x').fmap(value => { while (true) {} })"),
    (
        "pattern",
        "pure('x').every(fastcat(1, 1), pattern => { while (true) {} })",
    ),
    (
        "indexed pattern batch",
        "pure('x').echoWith(fastcat(1, 1), 1/8, (pattern, index) => { while (true) {} })",
    ),
    ("bind", "pure('x').polyBind(value => { while (true) {} })"),
    (
        "haps/parser",
        "pure(0).arpWith(haps => { while (true) {} })",
    ),
    (
        "configured parser after hap callback",
        "(() => { setStringParser(value => { while (true) {} }); return pure(0).arpWith(() => ['dynamic'].join('')); })()",
    ),
    ("custom query", "new Pattern(state => { while (true) {} })"),
    (
        "custom-query hap getter",
        "new Pattern(state => { const hap = {begin: state.span.begin, end: state.span.end}; Object.defineProperty(hap, 'value', { get() { while (true) {} } }); return [hap]; })",
    ),
    (
        "value materialization getter",
        "(() => { const value = {}; Object.defineProperty(value, 'x', { enumerable: true, get() { while (true) {} } }); return pure(value); })()",
    ),
    (
        "pick lookup proxy",
        "(() => { globalThis.blockPickLookup = false; const target = {x: 'ok'}; const choices = new Proxy(target, { ownKeys(value) { if (globalThis.blockPickLookup) while (true) {} return Reflect.ownKeys(value); } }); const lookup = pure(0).arpWith(haps => haps[0].withValue(() => choices)); return pure('x').pick(lookup); })()",
    ),
];

#[test]
fn every_javascript_host_entry_observes_the_same_query_deadline() {
    // Each source reaches a different CallbackHost method. Keeping the
    // resource check outside just `new Pattern(...query...)` is load-bearing:
    // control/bind/parser and JS-owned-value paths execute different bridge
    // code and can run getters after the callback itself has returned.
    let cancelled = AtomicBool::new(false);
    for &(label, source) in QUERY_HOST_ENTRY_CASES {
        let runtime = semantic_runtime(source);
        if label == "pick lookup proxy" {
            runtime.eval("globalThis.blockPickLookup = true;").unwrap();
        }
        let start = Instant::now();
        let error = runtime
            .query_cancellable(
                Slot::Active,
                0,
                Fraction::ZERO,
                Fraction::ONE,
                Duration::from_millis(30),
                &cancelled,
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 30 })
            ),
            "{label}: unexpected result: {error:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{label}: deadline was not enforced in time"
        );
        assert_clean(&runtime, label);
        runtime.clear_active();
    }
}

#[test]
fn every_host_entry_publishes_its_deadline_inside_a_scheduler_style_scope() {
    // Unlike direct query_cancellable, scheduler mode deliberately does not
    // translate an interrupt after the closure returns: by then Scheduler may
    // already have committed its cursor. Every host entry must therefore set
    // core's structural refusal before try_query_arc_sorted unwinds.
    let cancelled = AtomicBool::new(false);
    for &(label, source) in QUERY_HOST_ENTRY_CASES {
        let runtime = semantic_runtime(source);
        if label == "pick lookup proxy" {
            runtime.eval("globalThis.blockPickLookup = true;").unwrap();
        }
        let pattern = runtime.active_pattern().expect("active Pattern");
        let outcome = runtime
            .with_active_scope_cancellable(Duration::from_millis(30), &cancelled, || {
                pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            })
            .unwrap_or_else(|error| {
                panic!("{label}: scheduler scope failed outside query: {error}")
            });
        assert!(
            matches!(outcome, Err(QueryLimit::JsCpuDeadline { millis: 30 })),
            "{label}: host entry did not publish a structural deadline: {outcome:?}"
        );
        assert_clean(&runtime, label);
        runtime.clear_active();
    }
}

#[test]
fn ordinary_throw_with_deadline_words_remains_query_silence() {
    let runtime = semantic_runtime(
        "new Pattern(() => { throw new Error('CPU deadline JsCpuDeadline Cancelled'); })",
    );
    let cancelled = AtomicBool::new(false);
    let haps = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(100),
            &cancelled,
        )
        .unwrap();
    assert!(haps.is_empty(), "ordinary query throw must remain silence");
    assert_clean(&runtime, "ordinary throw");
    runtime.clear_active();
}

#[test]
fn a_public_active_homonym_is_ignored_by_private_publication_and_queries() {
    let runtime = semantic_runtime("pure('safe').fmap(value => value)");
    runtime
        .eval(
            r#"
              globalThis.activeRootInitiallyAbsent =
                Object.hasOwn(globalThis, '__rustel_active') ? 0 : 1;
              globalThis.activeAccessorHits = 0;
              Object.defineProperty(globalThis, '__rustel_active', {
                configurable: false,
                get() {
                  globalThis.activeAccessorHits++;
                  throw new Error('public active getter ran');
                },
                set(_) {
                  globalThis.activeAccessorHits++;
                  throw new Error('public active setter ran');
                },
              });
            "#,
        )
        .expect("install hostile public homonym");
    assert_eq!(runtime.get_number("activeRootInitiallyAbsent"), Some(1.0));

    runtime
        .evaluate_score(
            "pure('next').fmap(value => value)",
            &TranspileOptions::default(),
        )
        .expect("private publication must ignore the public homonym");

    assert!(runtime.active_needs_host());
    let cancelled = AtomicBool::new(false);
    let haps = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(100),
            &cancelled,
        )
        .unwrap();
    assert_eq!(haps[0].value.show(), "next");
    assert_eq!(runtime.get_number("activeAccessorHits"), Some(0.0));
    assert_clean(&runtime, "private active root");
    runtime.clear_active();
    assert!(runtime.active_pattern().is_none());
    assert_eq!(runtime.get_number("activeAccessorHits"), Some(0.0));
}

#[test]
fn interrupted_combinator_tag_getter_consumes_its_exception_and_recovers() {
    let runtime = semantic_runtime(
        r#"
          globalThis.blockCombinatorTag = true;
          const taggedValue = value => value + '!';
          Object.defineProperty(taggedValue, '__rustel_combinator', {
            get() {
              if (globalThis.blockCombinatorTag) while (true) {}
              return undefined;
            },
          });
          pure('x')
            .fmap(() => taggedValue)
            .fmap(callback => callback('ok'))
        "#,
    );
    let cancelled = AtomicBool::new(false);
    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(40),
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 40 })
    ));
    assert_clean(&runtime, "interrupted combinator tag getter");

    runtime
        .eval("globalThis.blockCombinatorTag = false")
        .unwrap();
    let recovered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "ok!");
    runtime.run_gc();
    runtime.run_gc();
    assert_clean(&runtime, "combinator tag getter recovery");
    runtime.clear_active();
}

#[test]
fn callback_deadline_is_typed_and_the_same_heap_recovers() {
    if std::env::var_os(QUERY_DEADLINE_CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("callback_deadline_is_typed_and_the_same_heap_recovers")
            .arg("--nocapture")
            .env(QUERY_DEADLINE_CHILD, "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn bounded-query child");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().expect("poll bounded-query child") {
                let output = child
                    .wait_with_output()
                    .expect("collect bounded-query child");
                assert!(
                    status.success(),
                    "bounded-query child failed with {status}:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill hung bounded-query child");
                let output = child.wait_with_output().expect("reap bounded-query child");
                panic!(
                    "bounded-query child exceeded five seconds; the QuickJS interrupt is not load-bearing:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let runtime = JsRuntime::new().unwrap();
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        "(_owner) => (value) => { \
           globalThis.queryPrefix = (globalThis.queryPrefix || 0) + 1; \
           if (globalThis.blockQuery) while (true) {} \
           return value + '!'; \
         }",
    );
    runtime
        .set_active(&builder, sequence().fmap_js(callback))
        .unwrap();
    runtime.eval("globalThis.blockQuery = true;").unwrap();

    let cancelled = AtomicBool::new(false);
    let start = Instant::now();
    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(40),
            &cancelled,
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 40 })
        ),
        "unexpected deadline result: {error:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(
        runtime.get_number("queryPrefix"),
        Some(1.0),
        "the mapping loop kept entering QuickJS after the deadline had already          fired; the side effect from the interrupted hap must persist, but the          REMAINING haps must not be mapped - a wide query holds tens of          thousands of them, and continuing owned the live producer thread for          minutes with the ring dry"
    );
    assert_clean(&runtime, "deadline");

    runtime.eval("globalThis.blockQuery = false;").unwrap();
    let recovered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(
        recovered
            .iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["bd!", "sd!"]
    );
    runtime.run_gc();
    runtime.run_gc();
    assert_clean(&runtime, "recovery");
    runtime.clear_active();
}

#[test]
fn cancellation_interrupts_javascript_before_the_longer_deadline() {
    let runtime = JsRuntime::new().unwrap();
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        "(_owner) => (value) => { \
           globalThis.cancelPrefix = 1; \
           if (globalThis.blockQuery) while (true) {} \
           return value; \
         }",
    );
    runtime
        .set_active(&builder, atom("bd").fmap_js(callback))
        .unwrap();
    runtime.eval("globalThis.blockQuery = true;").unwrap();

    let cancelled = Arc::new(AtomicBool::new(false));
    let setter = cancelled.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        setter.store(true, Ordering::Relaxed);
    });
    let start = Instant::now();
    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_secs(2),
            &cancelled,
        )
        .unwrap_err();
    thread.join().unwrap();
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::Cancelled)),
        "unexpected cancellation result: {error:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(runtime.get_number("cancelPrefix"), Some(1.0));
    assert_clean(&runtime, "cancellation");

    cancelled.store(false, Ordering::Relaxed);
    runtime.eval("globalThis.blockQuery = false;").unwrap();
    assert_eq!(
        runtime
            .query_cancellable(
                Slot::Active,
                0,
                Fraction::ZERO,
                Fraction::ONE,
                Duration::from_millis(200),
                &cancelled,
            )
            .unwrap()[0]
            .value
            .show(),
        "bd"
    );
    assert_clean(&runtime, "post-cancellation recovery");
    runtime.clear_active();
}

fn scheduler_style_caught_heap_then_spin(
    budget: Duration,
    cancellation: &AtomicBool,
    before_query: impl FnOnce(),
) -> (JsRuntime, Result<Vec<rustel_core::Hap>, QueryLimit>) {
    let runtime = semantic_runtime(
        r#"
          new Pattern(state => {
            try {
              globalThis.__queryTurnHog = new Array(2000000).fill(7);
            } catch (_) {
              globalThis.queryTurnHeapCaught =
                (globalThis.queryTurnHeapCaught || 0) + 1;
            }
            while (true) {}
          })
        "#,
    );
    let pattern = runtime.active_pattern().expect("active Pattern");
    runtime
        .set_memory_limit(runtime.heap_live().saturating_add(8 * 1024 * 1024))
        .unwrap();
    before_query();
    let outcome = match runtime.with_active_scope_cancellable(budget, cancellation, || {
        pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
    }) {
        Ok(outcome) => outcome,
        // Cancellation observed by the scope's entry check is equivalent to
        // cancellation observed from inside the query.
        Err(QueryError::Limit(QueryLimit::Cancelled)) => Err(QueryLimit::Cancelled),
        Err(other) => {
            panic!("a scheduler-style structural refusal is carried by the query result: {other:?}")
        }
    };
    (runtime, outcome)
}

#[test]
fn scheduler_deadline_and_cancellation_dominate_a_caught_heap_denial() {
    let cancelled = AtomicBool::new(false);
    let (deadline_runtime, deadline) =
        scheduler_style_caught_heap_then_spin(Duration::from_millis(40), &cancelled, || {});
    assert!(matches!(
        deadline,
        Err(QueryLimit::JsCpuDeadline { millis: 40 })
    ));
    assert_eq!(
        deadline_runtime.get_number("queryTurnHeapCaught"),
        Some(1.0)
    );
    assert_clean(&deadline_runtime, "heap plus deadline");
    deadline_runtime.clear_active();

    let cancelled = Arc::new(AtomicBool::new(false));
    let setter = cancelled.clone();
    let mut thread = None;
    let (cancel_runtime, cancellation) =
        scheduler_style_caught_heap_then_spin(Duration::from_secs(2), &cancelled, || {
            thread = Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                setter.store(true, Ordering::Relaxed);
            }));
        });
    thread.expect("cancellation timer").join().unwrap();
    assert!(matches!(cancellation, Err(QueryLimit::Cancelled)));
    assert_eq!(cancel_runtime.get_number("queryTurnHeapCaught"), Some(1.0));
    assert_clean(&cancel_runtime, "heap plus cancellation");
    cancel_runtime.clear_active();
}

#[test]
fn query_jobs_are_refused_discarded_and_never_run_later() {
    let runtime = JsRuntime::new().unwrap();
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        "(_owner) => (value) => { \
           globalThis.queryJobCalls = (globalThis.queryJobCalls || 0) + 1; \
           if (globalThis.queueQueryJob) { \
             queueMicrotask(() => { globalThis.queryJobRan = 1; }); \
             if (globalThis.throwAfterQueryJob) throw new Error('later query throw'); \
           } \
           return value + '!'; \
         }",
    );
    runtime
        .set_active(&builder, atom("bd").fmap_js(callback))
        .unwrap();
    runtime
        .eval("globalThis.queueQueryJob = true; globalThis.throwAfterQueryJob = true;")
        .unwrap();
    let cancelled = AtomicBool::new(false);

    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap_err();
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::JsPendingJobs)),
        "unexpected pending-job result: {error:?}"
    );
    assert_eq!(runtime.get_number("queryJobCalls"), Some(1.0));
    assert_eq!(runtime.get_number("queryJobRan"), None);
    assert_clean(&runtime, "pending-job refusal");

    runtime
        .eval("globalThis.queueQueryJob = false; globalThis.throwAfterQueryJob = false;")
        .unwrap();
    let recovered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "bd!");
    assert_eq!(runtime.get_number("queryJobRan"), None);
    assert_clean(&runtime, "pending-job recovery");
    runtime.clear_active();
}

#[test]
fn stale_job_preflight_runs_before_any_new_query_callback() {
    let runtime = JsRuntime::new().unwrap();
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        "(_owner) => (value) => { globalThis.newQueryRan = 1; return value; }",
    );
    runtime
        .set_active(&builder, atom("bd").fmap_js(callback))
        .unwrap();
    let cancelled = AtomicBool::new(false);

    let zero = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::ZERO,
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(
        zero,
        QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 0 })
    ));
    assert_eq!(
        runtime.get_number("newQueryRan"),
        None,
        "zero budget executed the callback"
    );

    runtime
        .eval("queueMicrotask(() => { globalThis.staleQueryJobRan = 1; });")
        .unwrap();
    assert!(runtime.jobs_pending());
    cancelled.store(true, Ordering::Relaxed);
    let combined = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(combined, QueryError::Limit(QueryLimit::Cancelled)));
    assert!(
        !runtime.jobs_pending(),
        "cancelled preflight left stale job"
    );
    assert_eq!(runtime.get_number("newQueryRan"), None);
    assert_eq!(runtime.get_number("staleQueryJobRan"), None);

    cancelled.store(false, Ordering::Relaxed);
    runtime
        .eval("queueMicrotask(() => { globalThis.staleQueryJobRan = 2; });")
        .unwrap();

    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::JsPendingJobs)
    ));
    assert_eq!(runtime.get_number("newQueryRan"), None);
    assert_eq!(runtime.get_number("staleQueryJobRan"), None);
    assert_clean(&runtime, "stale-job preflight");

    let recovered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "bd");
    assert_eq!(runtime.get_number("newQueryRan"), Some(1.0));
    runtime.clear_active();
}

#[test]
fn reentrant_query_inherits_the_outer_absolute_deadline() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_query_binding().unwrap();

    let mut inner_builder = runtime.builder();
    let inner_callback = inner_builder.callback(
        &runtime,
        "(_owner) => (value) => { \
           globalThis.innerQueryCalls = (globalThis.innerQueryCalls || 0) + 1; \
           if (globalThis.blockInnerQuery) while (true) {} \
           return value + 'I'; \
         }",
    );
    let inner = runtime
        .hold(&inner_builder, atom("in").fmap_js(inner_callback))
        .unwrap();

    let mut outer_builder = runtime.builder();
    let outer_callback = outer_builder.callback(
        &runtime,
        &format!("(_owner) => (value) => value + '<' + queryHeld({inner}, 0, 1) + '>'"),
    );
    runtime
        .set_active(&outer_builder, atom("out").fmap_js(outer_callback))
        .unwrap();
    runtime.eval("globalThis.blockInnerQuery = true;").unwrap();
    let cancelled = AtomicBool::new(false);

    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(40),
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 40 })
    ));
    assert_eq!(runtime.get_number("innerQueryCalls"), Some(1.0));
    assert_clean(&runtime, "reentrant deadline");

    runtime.eval("globalThis.blockInnerQuery = false;").unwrap();
    let recovered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "out<inI>");
    assert_clean(&runtime, "reentrant recovery");
    runtime.clear_active();
    runtime.release_held(inner).unwrap();
}

/// A throw in a reentrant `queryHeld` does not leak into the outer report.
/// The throw slot is written after each query, and the outer query succeeds.
#[test]
fn a_reentrant_throwing_query_does_not_leak_its_throw_into_the_outer_report() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_query_binding().unwrap();

    let mut inner_builder = runtime.builder();
    let inner_callback = inner_builder.callback(
        &runtime,
        "(_owner) => (_value) => { throw new Error('inner-threw'); }",
    );
    let inner = runtime
        .hold(&inner_builder, atom("in").fmap_js(inner_callback))
        .unwrap();

    let mut outer_builder = runtime.builder();
    let outer_callback = outer_builder.callback(
        &runtime,
        &format!("(_owner) => (value) => value + '<' + queryHeld({inner}, 0, 1) + '>'"),
    );
    runtime
        .set_active(&outer_builder, atom("out").fmap_js(outer_callback))
        .unwrap();

    let answered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(answered[0].value.show(), "out<>");
    assert_eq!(
        runtime.take_query_throw(),
        None,
        "the inner query's throw leaked into the outer report"
    );
    runtime.clear_active();
    runtime.release_held(inner).unwrap();
}

#[test]
fn reentrant_query_cannot_clear_an_outer_caught_heap_refusal() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_query_binding().unwrap();

    let inner_builder = runtime.builder();
    let inner = runtime.hold(&inner_builder, atom("in")).unwrap();
    let mut outer_builder = runtime.builder();
    let outer_callback = outer_builder.callback(
        &runtime,
        &format!(
            "(_owner) => (value) => {{ \
               if (globalThis.blockNestedHeap) {{ \
                 try {{ \
                   globalThis.__nestedQueryHog = new Array(2000000).fill(7); \
                 }} catch (_) {{ \
                   globalThis.nestedQueryHeapCaught = 1; \
                 }} \
               }} \
               return value + '<' + queryHeld({inner}, 0, 1) + '>'; \
             }}"
        ),
    );
    runtime
        .set_active(&outer_builder, atom("out").fmap_js(outer_callback))
        .unwrap();
    runtime.eval("globalThis.blockNestedHeap = true;").unwrap();
    runtime
        .set_memory_limit(runtime.heap_live().saturating_add(8 * 1024 * 1024))
        .unwrap();
    let cancelled = AtomicBool::new(false);

    let error = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap_err();
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::HostMemory)),
        "nested query cleared the outer heap refusal: {error:?}"
    );
    assert_eq!(runtime.get_number("nestedQueryHeapCaught"), Some(1.0));
    assert_clean(&runtime, "nested heap refusal");

    runtime.eval("globalThis.blockNestedHeap = false;").unwrap();
    let recovered = runtime
        .query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "out<in>");
    assert_clean(&runtime, "nested heap recovery");
    runtime.clear_active();
    runtime.release_held(inner).unwrap();
}

#[test]
fn legacy_query_apis_default_deadlines_are_exact_and_heaps_recover() {
    if !enter_supervised_child(
        "legacy_query_apis_default_deadlines_are_exact_and_heaps_recover",
        LEGACY_QUERY_DEADLINE_CHILD,
        Duration::from_secs(12),
    ) {
        return;
    }

    assert_eq!(DEFAULT_QUERY_JS_BUDGET, Duration::from_millis(2_000));
    let runtime = semantic_runtime(
        r#"
          pure('legacy').fmap(value => {
            globalThis.legacyDefaultCalls =
              (globalThis.legacyDefaultCalls || 0) + 1;
            if (globalThis.blockLegacyDefault) while (true) {}
            return value + '!';
          })
        "#,
    );
    runtime
        .eval("globalThis.blockLegacyDefault = true;")
        .unwrap();

    let started = Instant::now();
    let error = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap_err();
    assert!(
        matches!(
            error,
            QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 2_000 })
        ),
        "legacy query lost its exact default typed deadline: {error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the two-second legacy query deadline was not enforced in time: {:?}",
        started.elapsed()
    );
    assert_eq!(runtime.get_number("legacyDefaultCalls"), Some(1.0));
    assert_clean(&runtime, "legacy default deadline");

    runtime
        .eval("globalThis.blockLegacyDefault = false;")
        .unwrap();
    let recovered = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(recovered[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyDefaultCalls"), Some(2.0));
    assert_clean(&runtime, "legacy default deadline recovery");
    runtime.clear_active();

    let scope_runtime = semantic_runtime(
        r#"
          pure('scope').fmap(value => {
            globalThis.legacyScopeCalls =
              (globalThis.legacyScopeCalls || 0) + 1;
            if (globalThis.blockLegacyScope) while (true) {}
            return value + '!';
          })
        "#,
    );
    scope_runtime
        .eval("globalThis.blockLegacyScope = true;")
        .unwrap();
    let pattern = scope_runtime.active_pattern().expect("active Pattern");
    let started = Instant::now();
    let error = scope_runtime
        .with_active_scope(|| pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE))
        .unwrap_err();
    assert!(
        matches!(
            error,
            QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 2_000 })
        ),
        "legacy active scope lost its exact default typed deadline: {error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the two-second legacy scope deadline was not enforced in time: {:?}",
        started.elapsed()
    );
    assert_eq!(scope_runtime.get_number("legacyScopeCalls"), Some(1.0));
    assert_clean(&scope_runtime, "legacy scope default deadline");

    scope_runtime
        .eval("globalThis.blockLegacyScope = false;")
        .unwrap();
    let recovered = scope_runtime
        .with_active_scope(|| pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE))
        .unwrap()
        .unwrap();
    assert_eq!(recovered[0].value.show(), "scope!");
    assert_eq!(scope_runtime.get_number("legacyScopeCalls"), Some(2.0));
    assert_clean(&scope_runtime, "legacy scope default deadline recovery");
    scope_runtime.clear_active();

    let controls_runtime = JsRuntime::new().unwrap();
    let mut builder = controls_runtime.builder();
    let callback = builder.callback(
        &controls_runtime,
        r#"
          (_owner) => (state) => {
            globalThis.legacyControlsCalls =
              (globalThis.legacyControlsCalls || 0) + 1;
            if (globalThis.blockLegacyControls) while (true) {}
            return [{
              begin: state.span.begin,
              end: state.span.end,
              value: state.controls._cps === 0.5 ? 'controls!' : 'missing-cps'
            }];
          }
        "#,
    );
    controls_runtime
        .set_active(&builder, rustel_core::js_query(callback))
        .unwrap();
    controls_runtime
        .eval("globalThis.blockLegacyControls = true;")
        .unwrap();
    let started = Instant::now();
    let error = controls_runtime
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
        )
        .unwrap_err();
    assert_eq!(
        error,
        QueryLimit::JsCpuDeadline { millis: 2_000 }.to_string(),
        "legacy controls query lost its exact compatibility deadline"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the two-second legacy controls deadline was not enforced in time: {:?}",
        started.elapsed()
    );
    assert_eq!(
        controls_runtime.get_number("legacyControlsCalls"),
        Some(1.0)
    );
    assert_clean(&controls_runtime, "legacy controls default deadline");

    controls_runtime
        .eval("globalThis.blockLegacyControls = false;")
        .unwrap();
    let recovered = controls_runtime
        .query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "controls!");
    assert_eq!(
        controls_runtime.get_number("legacyControlsCalls"),
        Some(2.0)
    );
    assert_clean(
        &controls_runtime,
        "legacy controls default deadline recovery",
    );
    controls_runtime.clear_active();
}

#[test]
fn legacy_query_inherits_an_enclosing_raw_deadline() {
    let runtime = semantic_runtime(
        r#"
          pure('legacy').fmap(value => {
            if (globalThis.blockRawDeadline) while (true) {}
            return value;
          })
        "#,
    );
    runtime.eval("globalThis.blockRawDeadline = true;").unwrap();

    let started = Instant::now();
    let _ = runtime.with_deadline(Duration::from_millis(40), || {
        runtime.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
    });
    assert!(
        runtime.was_interrupted(),
        "the enclosing deadline did not fire"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "legacy query replaced the enclosing 40 ms deadline with its two-second default: {:?}",
        started.elapsed()
    );
    assert_clean(&runtime, "raw deadline inheritance");

    runtime
        .eval("globalThis.blockRawDeadline = false;")
        .unwrap();
    let recovered = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(recovered[0].value.show(), "legacy");
    assert_clean(&runtime, "raw deadline inheritance recovery");
    runtime.clear_active();
}

fn legacy_pending_job_runtime() -> JsRuntime {
    let runtime = JsRuntime::new().unwrap();
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        r#"
          (_owner) => (state) => {
            globalThis.legacyPendingCalls =
              (globalThis.legacyPendingCalls || 0) + 1;
            if (globalThis.queueLegacyPendingJob) {
              queueMicrotask(() => { globalThis.legacyPendingJobRan = 1; });
            }
            return [{
              begin: state.span.begin,
              end: state.span.end,
              value: 'legacy!'
            }];
          }
        "#,
    );
    runtime
        .set_active(&builder, rustel_core::js_query(callback))
        .unwrap();
    runtime
        .eval("globalThis.queueLegacyPendingJob = true;")
        .unwrap();
    runtime
}

fn exercise_typed_legacy_pending_job_entry(
    label: &str,
    invoke: impl Fn(&JsRuntime) -> Result<Vec<rustel_core::Hap>, QueryError>,
) {
    let runtime = legacy_pending_job_runtime();
    let error = invoke(&runtime).unwrap_err();
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::JsPendingJobs)),
        "{label}: callback-created job lost its typed refusal: {error:?}"
    );
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(1.0));
    assert_eq!(runtime.get_number("legacyPendingJobRan"), None);
    assert_clean(&runtime, &format!("{label} callback-created job"));

    runtime
        .eval("globalThis.queueLegacyPendingJob = false;")
        .unwrap();
    let recovered = invoke(&runtime).expect("same heap must recover after job refusal");
    assert_eq!(recovered[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(2.0));

    runtime
        .eval("queueMicrotask(() => { globalThis.staleLegacyJobRan = 1; });")
        .unwrap();
    assert!(runtime.jobs_pending(), "{label}: stale job was not queued");
    let stale = invoke(&runtime).unwrap_err();
    assert!(
        matches!(stale, QueryError::Limit(QueryLimit::JsPendingJobs)),
        "{label}: stale job lost its typed refusal: {stale:?}"
    );
    assert_eq!(
        runtime.get_number("legacyPendingCalls"),
        Some(2.0),
        "{label}: stale-job preflight ran the new callback"
    );
    assert_eq!(runtime.get_number("staleLegacyJobRan"), None);
    assert_clean(&runtime, &format!("{label} stale-job preflight"));

    let retried = invoke(&runtime).expect("same heap must recover after stale-job refusal");
    assert_eq!(retried[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(3.0));
    assert_clean(&runtime, &format!("{label} final recovery"));
    runtime.clear_active();
}

fn exercise_string_legacy_pending_job_entry(
    label: &str,
    invoke: impl Fn(&JsRuntime) -> Result<Vec<rustel_core::Hap>, String>,
) {
    let runtime = legacy_pending_job_runtime();
    let expected = QueryLimit::JsPendingJobs.to_string();
    let error = invoke(&runtime).unwrap_err();
    assert_eq!(error, expected, "{label}: wrong compatibility error");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(1.0));
    assert_eq!(runtime.get_number("legacyPendingJobRan"), None);
    assert_clean(&runtime, &format!("{label} callback-created job"));

    runtime
        .eval("globalThis.queueLegacyPendingJob = false;")
        .unwrap();
    let recovered = invoke(&runtime).expect("same heap must recover after job refusal");
    assert_eq!(recovered[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(2.0));

    runtime
        .eval("queueMicrotask(() => { globalThis.staleLegacyJobRan = 1; });")
        .unwrap();
    assert!(runtime.jobs_pending(), "{label}: stale job was not queued");
    let stale = invoke(&runtime).unwrap_err();
    assert_eq!(stale, expected, "{label}: wrong stale-job error");
    assert_eq!(
        runtime.get_number("legacyPendingCalls"),
        Some(2.0),
        "{label}: stale-job preflight ran the new callback"
    );
    assert_eq!(runtime.get_number("staleLegacyJobRan"), None);
    assert_clean(&runtime, &format!("{label} stale-job preflight"));

    let retried = invoke(&runtime).expect("same heap must recover after stale-job refusal");
    assert_eq!(retried[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(3.0));
    assert_clean(&runtime, &format!("{label} final recovery"));
    runtime.clear_active();
}

#[test]
fn every_legacy_query_entry_refuses_discards_and_recovers_from_pending_jobs() {
    exercise_typed_legacy_pending_job_entry("query", |runtime| {
        runtime.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
    });
    exercise_typed_legacy_pending_job_entry("query_with_budget", |runtime| {
        runtime.query_with_budget(Slot::Active, 0, Fraction::ZERO, Fraction::ONE, 10)
    });
    exercise_typed_legacy_pending_job_entry("with_active_scope", |runtime| {
        let pattern = runtime.active_pattern().expect("active Pattern");
        runtime
            .with_active_scope(|| pattern.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE))
            .and_then(|outcome| outcome.map_err(QueryError::from))
    });
    exercise_string_legacy_pending_job_entry("query_with_controls", |runtime| {
        runtime.query_with_controls(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
        )
    });
}

#[test]
fn cancellable_controls_query_preserves_typed_limits_and_controls() {
    let runtime = JsRuntime::new().unwrap();
    let mut builder = runtime.builder();
    let callback = builder.callback(
        &runtime,
        r#"
          (_owner) => (state) => {
            globalThis.controlsQueryCalls =
              (globalThis.controlsQueryCalls || 0) + 1;
            if (globalThis.blockControlsQuery) while (true) {}
            return [{
              begin: state.span.begin,
              end: state.span.end,
              value: state.controls._cps === 0.5 ? 'cps=0.5' : 'missing-cps'
            }];
          }
        "#,
    );
    runtime
        .set_active(&builder, rustel_core::js_query(callback))
        .unwrap();
    runtime
        .eval("globalThis.blockControlsQuery = true;")
        .unwrap();

    let cancelled = Arc::new(AtomicBool::new(false));
    let deadline = runtime
        .query_with_controls_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
            Duration::from_millis(30),
            &cancelled,
        )
        .unwrap_err();
    assert!(matches!(
        deadline,
        QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 30 })
    ));
    assert_clean(&runtime, "controls query deadline");

    let trigger = cancelled.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        trigger.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let cancellation = runtime
        .query_with_controls_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
            Duration::from_secs(2),
            &cancelled,
        )
        .unwrap_err();
    thread.join().unwrap();
    assert!(matches!(
        cancellation,
        QueryError::Limit(QueryLimit::Cancelled)
    ));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "controls query cancellation waited for its deadline: {:?}",
        started.elapsed()
    );
    assert_clean(&runtime, "controls query cancellation");

    cancelled.store(false, Ordering::Relaxed);
    runtime
        .eval("globalThis.blockControlsQuery = false;")
        .unwrap();
    let recovered = runtime
        .query_with_controls_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            &[("_cps", 0.5)],
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap();
    assert_eq!(recovered[0].value.show(), "cps=0.5");
    assert_clean(&runtime, "controls query recovery");
    runtime.clear_active();

    let span_runtime = JsRuntime::new().unwrap();
    let span_builder = span_runtime.builder();
    span_runtime
        .set_active(&span_builder, atom("span"))
        .unwrap();
    let span = span_runtime
        .query_with_controls_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::int(rustel_core::MAX_QUERY_SPAN_CYCLES + 1),
            &[("_cps", 0.5)],
            Duration::from_millis(200),
            &cancelled,
        )
        .unwrap_err();
    assert!(
        matches!(
            span,
            QueryError::Limit(QueryLimit::QuerySpan { cycles })
                if cycles == rustel_core::MAX_QUERY_SPAN_CYCLES
        ),
        "controls query swallowed its structural span refusal: {span:?}"
    );
    assert_clean(&span_runtime, "controls query span refusal");
    span_runtime.clear_active();
}

#[test]
fn nested_legacy_query_leaves_setup_owned_jobs_to_the_prelude_turn() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime.install_query_binding().unwrap();
    let builder = runtime.builder();
    let held = runtime.hold(&builder, atom("inner")).unwrap();

    runtime
        .evaluate_prelude(
            &format!(
                "queueMicrotask(() => {{ globalThis.setupOwnedJobRan = 1; }}); \
                 globalThis.setupNestedValue = queryHeld({held}, 0, 1);"
            ),
            &TranspileOptions::default(),
            Duration::from_millis(200),
        )
        .expect("nested legacy query must not claim or discard the setup-owned job");
    assert_eq!(runtime.get_number("setupOwnedJobRan"), Some(1.0));
    assert_eq!(
        runtime.get_string("setupNestedValue").as_deref(),
        Some("inner")
    );
    assert_clean(&runtime, "nested setup job ownership");
    runtime.release_held(held).unwrap();
}

#[test]
fn nested_legacy_query_cannot_clear_a_caught_prelude_heap_refusal() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime.install_query_binding().unwrap();
    let builder = runtime.builder();
    let held = runtime.hold(&builder, atom("inner")).unwrap();
    runtime
        .set_memory_limit(runtime.heap_live().saturating_add(8 * 1024 * 1024))
        .unwrap();

    let error = runtime
        .evaluate_prelude(
            &format!(
                "try {{ \
                   globalThis.setupHeapHog = new Array(2_000_000).fill(7); \
                 }} catch (_) {{ \
                   globalThis.setupHeapCaught = 1; \
                 }} \
                 globalThis.setupHeapNestedValue = queryHeld({held}, 0, 1);"
            ),
            &TranspileOptions::default(),
            Duration::from_millis(500),
        )
        .unwrap_err();
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::HostMemory)),
        "nested legacy query cleared the setup turn's caught heap refusal: {error:?}"
    );
    assert_eq!(runtime.get_number("setupHeapCaught"), Some(1.0));
    assert_eq!(
        runtime.get_string("setupHeapNestedValue").as_deref(),
        Some("inner"),
        "the nested query did not run after the caught allocation denial"
    );
    assert_clean(&runtime, "nested setup heap ownership");

    runtime
        .evaluate_prelude(
            "globalThis.afterSetupHeapRefusal = 1;",
            &TranspileOptions::default(),
            Duration::from_millis(200),
        )
        .expect("same heap must recover after the nested setup refusal");
    assert_eq!(runtime.get_number("afterSetupHeapRefusal"), Some(1.0));
    assert_clean(&runtime, "nested setup heap recovery");
    runtime.release_held(held).unwrap();
}

#[test]
fn nested_legacy_query_leaves_outer_query_owned_jobs_for_outer_cleanup() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_query_binding().unwrap();

    let inner_builder = runtime.builder();
    let inner = runtime.hold(&inner_builder, atom("in")).unwrap();
    let mut outer_builder = runtime.builder();
    let callback = outer_builder.callback(
        &runtime,
        &format!(
            "(_owner) => (value) => {{ \
               if (globalThis.queueNestedOwnedJob) \
                 queueMicrotask(() => {{ globalThis.nestedOwnedJobRan = 1; }}); \
               return value + '<' + queryHeld({inner}, 0, 1) + '>'; \
             }}"
        ),
    );
    runtime
        .set_active(&outer_builder, atom("out").fmap_js(callback))
        .unwrap();
    runtime
        .eval("globalThis.queueNestedOwnedJob = true;")
        .unwrap();

    let error = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap_err();
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::JsPendingJobs)),
        "nested legacy query stole the outer job: {error:?}"
    );
    assert_eq!(runtime.get_number("nestedOwnedJobRan"), None);
    assert_clean(&runtime, "nested outer job ownership");

    runtime
        .eval("globalThis.queueNestedOwnedJob = false;")
        .unwrap();
    let recovered = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap();
    assert_eq!(recovered[0].value.show(), "out<in>");
    assert_eq!(runtime.get_number("nestedOwnedJobRan"), None);
    assert_clean(&runtime, "nested outer job recovery");
    runtime.clear_active();
    runtime.release_held(inner).unwrap();
}

#[test]
fn tour_square_expansion_is_preflighted_at_the_exact_boundary() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime("pure('x').tour(...Array(127).fill(silence))");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "127 arguments should materialise exactly (127 + 1)^2 entries"
    );
    assert!(query_active(&exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&exact, "exact tour expansion");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             globalThis.tourParserHits = 0;
             setStringParser(value => {
               globalThis.tourParserHits++;
               return pure(value);
             });
             return pure('x').tour(...Array(128).fill('raw'));
           })()"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "the (128 + 1)^2 overshoot completed expanded entries or reified before refusal"
    );
    assert_eq!(
        over.get_number("tourParserHits"),
        Some(0.0),
        "the overshoot reached the configured parser before preflight"
    );
    let error = query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "tour",
            minimum_entries: 16_641,
            limit: 16_384,
        })
    ));
    assert_clean(&over, "oversized tour expansion");

    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("same runtime should accept a new score after refusal");
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "tour preflight recovery");
    over.clear_active();
}

#[test]
fn query_time_tour_calls_share_one_cumulative_allowance_and_recover() {
    let accepted = semantic_runtime(
        r#"(() => {
             const many = Array(89).fill(silence);
             return fastcat(pure(0), pure(1))
               .polyBind(() => pure('x').tour(...many));
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&accepted, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_200,
        "two individually legal 8,100-entry calls did not share the query allowance"
    );
    assert_clean(&accepted, "cumulative tour exact side");
    accepted.clear_active();

    let refused = semantic_runtime(
        r#"(() => {
             const many = Array(90).fill(silence);
             return fastcat(pure(0), pure(1))
               .polyBind(() => pure('x').tour(...many));
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    let error = query_active(&refused, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "tour",
            minimum_entries: 16_562,
            limit: 16_384,
        })
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_281,
        "the second call materialised before the cumulative overshoot refused it"
    );
    assert_clean(&refused, "cumulative tour refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_281,
        "the top-level query allowance did not reset after refusal"
    );
    assert_clean(&refused, "cumulative tour recovery");
    refused.clear_active();
}

#[test]
fn tour_array_tail_preflight_counts_concat_flattening_before_reification() {
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime("pure('x').tour(Array(16381).fill(silence))");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "one mapped pair, the receiver, and the flattened array tail must fill the bound"
    );
    assert!(query_active(&exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&exact, "exact flattened tour tail");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             globalThis.tourTailParserHits = 0;
             setStringParser(value => {
               globalThis.tourTailParserHits++;
               return pure(value);
             });
             return pure('x').tour(Array(16382).fill('raw'));
           })()"#,
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_eq!(over.get_number("tourTailParserHits"), Some(0.0));
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "tour",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&over, "oversized flattened tour tail");
    over.clear_active();
}

#[test]
fn shrink_and_tour_share_the_same_query_time_stepwise_allowance() {
    let runtime = semantic_runtime(
        r#"(() => {
             const many = Array(89).fill(silence);
             return fastcat(pure(0), pure(1)).polyBind(value =>
               value === 0
                 ? gap(9000).shrink(0)
                 : pure('x').tour(...many)
             );
           })()"#,
    );

    rustel_core::reset_stepwise_entries_materialised();
    let error = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "tour",
            minimum_entries: 17_100,
            limit: 16_384,
        })
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        9_000,
        "tour used a separate allowance or materialised after the shared overshoot"
    );
    assert_clean(&runtime, "mixed shrink/tour refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&runtime, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 9_000);
    assert_clean(&runtime, "mixed shrink/tour recovery");
    runtime.clear_active();
}

#[test]
fn shrinklist_growlist_helpers_preserve_surface_ranges_and_dynamic_dispatch() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .eval(
            r#"(() => {
              const check = (condition, label) => {
                if (!condition) throw new Error(label);
              };
              const isConstructor = value => {
                try { Reflect.construct(function () {}, [], value); return true; }
                catch (_) { return false; }
              };
              const flags = (object, name) => {
                const descriptor = Object.getOwnPropertyDescriptor(object, name);
                return descriptor && descriptor.writable
                  && descriptor.enumerable && descriptor.configurable;
              };

              check(shrinklist.name === 'shrinklist' && shrinklist.length === 2,
                'shrinklist arrow reflection');
              check(growlist.name === 'growlist' && growlist.length === 2,
                'growlist arrow reflection');
              check(!Object.hasOwn(shrinklist, 'prototype') && !isConstructor(shrinklist),
                'shrinklist must be a nonconstructible arrow');
              check(!Object.hasOwn(growlist, 'prototype') && !isConstructor(growlist),
                'growlist must be a nonconstructible arrow');
              check(s_taperlist === shrinklist, 'free taperlist identity');
              check(flags(globalThis, 'shrinklist') && flags(globalThis, 'growlist')
                && flags(globalThis, 's_taperlist'), 'ordinary global descriptors');

              const protoShrink = Pattern.prototype.shrinklist;
              const protoGrow = Pattern.prototype.growlist;
              check(protoShrink.name === '' && protoShrink.length === 1
                && isConstructor(protoShrink), 'prototype shrinklist reflection');
              check(protoGrow.name === '' && protoGrow.length === 1
                && isConstructor(protoGrow), 'prototype growlist reflection');
              check(flags(Pattern.prototype, 'shrinklist')
                && flags(Pattern.prototype, 'growlist')
                && flags(Pattern.prototype, 's_taperlist'),
                'ordinary prototype descriptors');
              check(Pattern.prototype.s_taperlist === protoShrink,
                'prototype taperlist identity');
              check(Array.isArray(Reflect.construct(protoShrink, [1])),
                'constructible shrinklist result');
              let growConstruction = 'none';
              try { Reflect.construct(protoGrow, [1]); }
              catch (error) { growConstruction = error.name; }
              check(growConstruction === 'TypeError', 'growlist constructor phase');
              check([
                '_shrinklist', '_growlist', '_s_taperlist', 'taperlist',
                's_shrinklist', 's_growlist', 'waxlist', 'wanelist'
              ].every(name => !Object.hasOwn(globalThis, name)
                && !Object.hasOwn(Pattern.prototype, name)), 'invented list aliases');

              const base = sequence(pure('a'), pure('b'), pure('c'), pure('d'));
              const steps = list => list.map(pattern => pattern._steps.show()).join(',');
              const one = base.shrinklist(1);
              check(Array.isArray(one) && Object.getPrototypeOf(one) === Array.prototype,
                'ordinary result array');
              check(one.length === 4 && steps(one) === '4/1,3/1,2/1,1/1',
                `positive ranges: ${one.length}|${steps(one)}`);
              check(one.every((pattern, index) => pattern instanceof Pattern
                && one.indexOf(pattern) === index), 'distinct Pattern wrappers');
              const state = { span: { begin: 0, end: 1 }, controls: {} };
              const windows = list => list.map(pattern => pattern.query(state).map(hap =>
                `${hap.part.begin.show()}>${hap.part.end.show()}:${hap.value}`
              ).join(';')).join('|');
              check(windows(one) ===
                '0/1>1/4:a;1/4>1/2:b;1/2>3/4:c;3/4>1/1:d|'
                + '0/1>1/3:b;1/3>2/3:c;2/3>1/1:d|'
                + '0/1>1/2:c;1/2>1/1:d|0/1>1/1:d',
                'positive query windows');
              check(steps(base.shrinklist(-1)) === '4/1,3/1,2/1,1/1',
                'negative ranges');
              check(windows(base.shrinklist(-1)) ===
                '0/1>1/4:a;1/4>1/2:b;1/2>3/4:c;3/4>1/1:d|'
                + '0/1>1/3:a;1/3>2/3:b;2/3>1/1:c|'
                + '0/1>1/2:a;1/2>1/1:b|0/1>1/1:a',
                'negative query windows');
              const zero = base.shrinklist(0);
              check(zero.length === 4 && steps(zero) === '4/1,4/1,4/1,4/1'
                && zero.every(pattern => pattern !== base), 'zero amount behavior');
              const terminal = base.shrinklist(2);
              check(steps(terminal) === '4/1,2/1,0/1'
                && terminal[2] !== nothing && terminal[2] instanceof Pattern
                && terminal[2].query(state).length === 0,
                'distinct inclusive terminal range');
              check(base.shrinklist(5).length === 1, 'large amount cutoff');
              const thirds = sequence(pure('a'), pure('b'), pure('c'))
                .shrinklist([1, 2]);
              check(steps(thirds) === '3/1,2/1'
                && windows(thirds) ===
                  '0/1>1/3:a;1/3>2/3:b;2/3>1/1:c|'
                  + '0/1>1/2:b;1/2>1/1:c',
                'exact non-binary rational projection');
              check(steps(base.shrinklist([1, 2.5])) === '4/1,3/1,2/1',
                'fractional times loop');
              check(base.shrinklist([1, -1]).length === 0,
                'negative times loop');
              check(base.shrinklist([1, 0])[0] === base,
                'strict numeric zero shortcut identity');
              const noSteps = pure('x').setSteps(undefined);
              check(noSteps.shrinklist(Symbol('ignored'))[0] === noSteps,
                'no-step shortcut precedes coercion');
              check(steps(base.growlist(1)) === '1/1,2/1,3/1,4/1',
                'growlist reverses shrinklist');
              check(windows(base.growlist(1)) ===
                '0/1>1/1:d|0/1>1/2:c;1/2>1/1:d|'
                + '0/1>1/3:b;1/3>2/3:c;2/3>1/1:d|'
                + '0/1>1/4:a;1/4>1/2:b;1/2>3/4:c;3/4>1/1:d',
                'growlist query windows');

              let zeroStepError = 'none';
              try { gap(0).shrinklist(1); }
              catch (error) { zeroStepError = error.message; }
              check(zeroStepError.includes('Division by Zero'),
                'zero-step eager division phase');
              const publicFraction = globalThis.Fraction;
              globalThis.Fraction = () => { throw new Error('mutable global Fraction'); };
              check(base.shrinklist(1).length === 4, 'lexical Fraction capture');
              globalThis.Fraction = publicFraction;

              const dispatch = [];
              const receiver = {
                shrinklist(value) { dispatch.push(`free-shrink:${value}`); return ['S']; },
                growlist(value) { dispatch.push(`free-grow:${value}`); return ['G']; },
              };
              check(shrinklist(2, receiver)[0] === 'S'
                && s_taperlist(3, receiver)[0] === 'S'
                && growlist(4, receiver)[0] === 'G', 'free dynamic dispatch');
              const reversed = ['left', 'right'];
              let reverseReceiver;
              const growReceiver = {
                shrinklist(value) {
                  dispatch.push(`proto-grow:${value}`);
                  reversed.reverse = function () {
                    reverseReceiver = this;
                    return Array.prototype.reverse.call(this);
                  };
                  return reversed;
                },
              };
              const grown = protoGrow.call(growReceiver, 5);
              check(grown === reversed && reverseReceiver === reversed
                && grown.join(',') === 'right,left',
                'growlist dynamic shrinklist and in-place reverse');
              check(dispatch.join('|') ===
                'free-shrink:2|free-shrink:3|free-grow:4|proto-grow:5',
                'dynamic dispatch arguments');
              globalThis.__shrinklistSurfaceOkay = 1;
            })()"#,
        )
        .unwrap();
    assert_eq!(runtime.get_number("__shrinklistSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "shrinklist helper surface");
}

#[test]
fn shrinklist_preflight_is_atomic_typed_and_recovers() {
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime("stack(...gap(16384).shrinklist(0))");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "the exact helper boundary did not materialise its complete wrapper list"
    );
    assert!(query_active(&exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&exact, "exact shrinklist boundary");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             const source = gap(16385);
             globalThis.shrinklistZoomHits = 0;
             source.zoom = function (...args) {
               globalThis.shrinklistZoomHits++;
               return Pattern.prototype.zoom.call(this, ...args);
             };
             return stack(...source.shrinklist(0));
           })()"#,
    );
    assert_eq!(
        over.get_number("shrinklistZoomHits"),
        Some(0.0),
        "oversized shrinklist entered mutable zoom before preflight"
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "oversized shrinklist completed wrappers before refusal"
    );
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&over, "oversized shrinklist refusal");

    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("same runtime should recover after shrinklist refusal");
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "shrinklist recovery");
    over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let started = Instant::now();
    let infinite = semantic_runtime(
        r#"(() => {
             const source = gap(1);
             globalThis.infiniteShrinklistZoomHits = 0;
             source.zoom = function () {
               globalThis.infiniteShrinklistZoomHits++;
               return this;
             };
             return stack(...source.shrinklist([0, Infinity]));
           })()"#,
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "infinite shrinklist times escaped the bounded planner"
    );
    assert_eq!(infinite.get_number("infiniteShrinklistZoomHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&infinite, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&infinite, "infinite-times shrinklist refusal");
    infinite.clear_active();
}

#[test]
fn shrinklist_and_growlist_share_the_cumulative_stepwise_allowance() {
    let exact = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(() =>
             stack(...gap(8192).growlist(0))
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "growlist charged or materialised its shrinklist result twice"
    );
    assert_clean(&exact, "cumulative growlist exact side");
    exact.clear_active();

    let refused = semantic_runtime(
        r#"(() => {
             globalThis.cumulativeShrinklistZoomHits = 0;
             return fastcat(pure(0), pure(1)).polyBind(value => {
               const source = gap(value === 0 ? 8192 : 8193);
               source.zoom = function (...args) {
                 globalThis.cumulativeShrinklistZoomHits++;
                 return Pattern.prototype.zoom.call(this, ...args);
               };
               return stack(...source.shrinklist(0));
             });
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&refused, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_192,
        "the cumulative overshoot materialised its refused branch"
    );
    assert_eq!(
        refused.get_number("cumulativeShrinklistZoomHits"),
        Some(8_192.0),
        "the refused branch reached dynamic zoom"
    );
    assert_clean(&refused, "cumulative shrinklist refusal");

    refused
        .eval("globalThis.cumulativeShrinklistZoomHits = 0")
        .unwrap();
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_192);
    assert_eq!(
        refused.get_number("cumulativeShrinklistZoomHits"),
        Some(8_192.0)
    );
    assert_clean(&refused, "cumulative shrinklist recovery");
    refused.clear_active();

    let mixed = semantic_runtime(
        r#"(() => {
             globalThis.mixedShrinklistZoomHits = 0;
             return fastcat(pure(0), pure(1)).polyBind(value => {
               if (value === 0) return gap(8000).shrink(0);
               const source = gap(8385);
               source.zoom = function (...args) {
                 globalThis.mixedShrinklistZoomHits++;
                 return Pattern.prototype.zoom.call(this, ...args);
               };
               return stack(...source.shrinklist(0));
             });
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&mixed, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_000,
        "shrinklist used a separate allowance or materialised after mixed refusal"
    );
    assert_eq!(mixed.get_number("mixedShrinklistZoomHits"), Some(0.0));
    assert_clean(&mixed, "mixed shrink/shrinklist refusal");
    mixed.clear_active();
}

#[test]
fn retained_shrinklist_entry_keeps_its_js_owned_value_alive() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 let marker = { marker: 'retained-shrinklist-entry' };
                 globalThis.retainedShrinklistRef = new WeakRef(marker);
                 let source = sequence(pure(marker), pure('tail'));
                 let list = source.shrinklist(1);
                 const retained = list[0];
                 marker = null;
                 source = null;
                 list = null;
                 return retained;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("retain one wrapper from a shrinklist array");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            r#"globalThis.retainedShrinklistAlive = Number(
                 retainedShrinklistRef.deref()?.marker === 'retained-shrinklist-entry'
               )"#,
        )
        .unwrap();
    assert_eq!(runtime.get_number("retainedShrinklistAlive"), Some(1.0));
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values
            .iter()
            .any(|value| value.contains("retained-shrinklist-entry")),
        "retained wrapper lost the JS-owned value from its source sidecar: {values:?}"
    );
    assert_clean(&runtime, "retained shrinklist entry ownership");
    runtime.clear_active();
}

#[test]
fn retained_zero_width_shrinklist_entry_keeps_its_source_sidecar_alive() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 let marker = { marker: 'retained-zero-width-source' };
                 globalThis.zeroWidthShrinklistRef = new WeakRef(marker);
                 let source = sequence(
                   pure(marker), pure('b'), pure('c'), pure('d')
                 );
                 let list = source.shrinklist([1, 5]);
                 const terminal = list[4];
                 globalThis.retainedZeroWidthTerminal = terminal;
                 marker = null;
                 source = null;
                 list = null;
                 return terminal;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("retain only the zero-width shrinklist terminal");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            r#"globalThis.zeroWidthShrinklistSourceAlive = Number(
                 zeroWidthShrinklistRef.deref()?.marker
                   === 'retained-zero-width-source'
               )"#,
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("zeroWidthShrinklistSourceAlive"),
        Some(1.0),
        "the distinct terminal wrapper pruned its pinned source ownership"
    );
    assert_eq!(
        runtime
            .active_pattern()
            .expect("zero-width shrinklist terminal")
            .steps,
        Some(Fraction::ZERO)
    );
    assert!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&runtime, "retained zero-width shrinklist ownership");
    runtime.clear_active();
}

#[test]
fn canonical_shrink_grow_pair_arrays_keep_identity_asymmetry_and_aliases() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                 const check = (condition, message) => {
                   if (!condition) throw new Error(message);
                 };
                 const base = sequence('a', 'b', 'c', 'd');
                 const pair = [1, 2];
                 phase = 'shrink';
                 const shrunk = base.shrink(pair);
                 phase = 'grow';
                 const grown = base.grow(pair);
                 phase = 'taper';
                 const tapered = base.s_taper(pair);
                 phase = 'shrink-steps';
                 check(shrunk._steps.show() === '7/1', 'shrink pair steps');
                 phase = 'grow-steps';
                 check(grown._steps.show() === '13/1', 'grow pair steps');
                 phase = 'taper-steps';
                 check(tapered._steps.show() === '7/1', 'taper pair steps');
                 phase = 'free-alias';
                 check(s_taper === shrink, 'free taper identity');
                 phase = 'proto-alias';
                 check(Pattern.prototype.s_taper === Pattern.prototype.shrink,
                   'prototype taper identity');
                 phase = 'raw';
                 const rawShrink = Pattern.prototype._shrink;
                 const rawGrow = Pattern.prototype._grow;
                 check(typeof rawShrink === 'function'
                   && typeof rawGrow === 'function', 'raw pair absent');
                 check(rawShrink !== Pattern.prototype.shrink
                   && rawGrow !== Pattern.prototype.grow
                   && rawShrink !== rawGrow, 'raw pair identity');
                 for (const [name, raw] of [
                   ['_shrink', rawShrink], ['_grow', rawGrow]
                 ]) {
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, name
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     `${name} descriptor`);
                   check(raw.name === '' && raw.length === 0
                     && Object.prototype.hasOwnProperty.call(raw, 'prototype'),
                     `${name} reflection`);
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, `${name} constructibility`);
                   check(!(name in globalThis)
                     && !Object.prototype.hasOwnProperty.call(rustelScope, name),
                     `${name} publication`);
                 }
                 const rawOrder = Object.keys(Pattern.prototype).filter(name => [
                   'shrinklist', 'growlist', 'shrink', '_shrink',
                   'grow', '_grow', 's_taper', 's_taperlist'
                 ].includes(name)).join('|');
                 check(rawOrder
                   === 'shrinklist|growlist|shrink|_shrink|grow|_grow|s_taper|s_taperlist',
                   `raw pair installation order: ${rawOrder}`);
                 check(!('_s_taper' in Pattern.prototype)
                   && !('_s_taper' in globalThis)
                   && !Object.prototype.hasOwnProperty.call(
                     rustelScope, '_s_taper'
                   ), 'raw taper leaked');

                 const receiver = sequence('a', 'b', 'c', 'd');
                 let shrinkArgument;
                 receiver.shrinklist = function (argument) {
                   shrinkArgument = argument;
                   return [this];
                 };
                 phase = 'instance-shrink';
                 receiver.shrink(pair);
                 check(shrinkArgument === pair, 'shrink cloned its pair');

                 phase = 'entries';
                 const left = gap(2);
                 const right = gap(3);
                 const returned = [left, right];
                 const order = [];
                 returned.reverse = function () {
                   order.push('reverse');
                   return Array.prototype.reverse.call(this);
                 };
                 returned.reduce = function (...args) {
                   order.push(`reduce:${this === returned}:${this[0] === right}`);
                   return Array.prototype.reduce.apply(this, args);
                 };
                 let growArgument;
                 let growlistHits = 0;
                 receiver.shrinklist = function (argument) {
                   growArgument = argument;
                   return returned;
                 };
                 receiver.growlist = function () { growlistHits++; return []; };
                 phase = 'custom-grow';
                 const customGrow = receiver.grow(pair);
                 phase = 'custom-check';
                 check(growArgument.show() === '-1/2', 'grow pair coercion');
                 check(growArgument instanceof Fraction._original,
                   'grow did not pass a canonical Fraction');
                 check(growArgument !== pair, 'grow forwarded its pair Array');
                 check(growlistHits === 0, 'grow dispatched through growlist');
                 check(returned[0] === right && returned[1] === left,
                   'grow did not reverse the returned array in place');
                 check(order.join('|') === 'reverse|reduce:true:true',
                   `grow phase order: ${order}`);
                 check(customGrow._steps.show() === '5/1', 'custom grow steps');
                 globalThis.canonicalPairSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.canonicalPairSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("canonicalPairSurfaceError"), None);
    assert_eq!(runtime.get_number("canonicalPairSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "canonical pair surface");

    runtime
        .evaluate_score(
            "sequence('a', 'b', 'c', 'd').shrink([1, 2])",
            &TranspileOptions::default(),
        )
        .unwrap();
    assert!(
        !runtime.active_needs_host(),
        "a pure pair must finish as a host-free native graph"
    );
    assert_clean(&runtime, "canonical pair purity");
    runtime.clear_active();
}

#[test]
fn raw_take_drop_keep_direct_coercion_and_mutation_order() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawTake = Pattern.prototype._take;
                   const rawDrop = Pattern.prototype._drop;
                   const base = sequence('a', 'b', 'c', 'd');

                   phase = 'surface';
                   check(Pattern.prototype.s_add === Pattern.prototype.take
                     && Pattern.prototype.s_sub === Pattern.prototype.drop,
                     'raw install replaced a copied canonical method');
                   check(rawTake !== Pattern.prototype.take
                     && rawDrop !== Pattern.prototype.drop
                     && rawTake !== rawDrop, 'raw identities');
                   for (const [name, raw] of [
                     ['_take', rawTake], ['_drop', rawDrop]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} descriptor`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(raw, 'prototype'),
                       `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'take', '_take', 'drop', '_drop', 's_add', 's_sub'
                     ].includes(name)).join('|');
                   check(rawOrder === 'take|_take|drop|_drop|s_add|s_sub',
                     `raw take/drop order: ${rawOrder}`);
                   check(['_s_add', '_s_sub', '_pace'].every(name =>
                       !(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       )
                       && !(name in Pattern.prototype)
                     ), 'adjacent raw surface expanded');

                   phase = 'omission';
                   for (const [name, raw] of [
                     ['_take', rawTake], ['_drop', rawDrop]
                   ]) {
                     let omitted;
                     try { apply(raw, base, []); }
                     catch (error) { omitted = error; }
                     check(omitted instanceof TypeError,
                       `${name} omission did not leave pat undefined`);
                   }

                   phase = 'undefined-and-pair';
                   check(apply(rawTake, base, [undefined]) === nothing,
                     '_take(undefined) did not return lexical nothing');
                   check(apply(rawTake, base, [0]) === nothing
                     && apply(rawTake, base, [4]) === base,
                     '_take lost its direct-body return identities');
                   for (const [amount, steps] of [
                     [undefined, '4/1'], [0, '4/1'], [4, '0/1'],
                     [1, '3/1'], [-1, '3/1'], [6, '2/1'], [-6, '2/1']
                   ]) {
                     const result = apply(rawDrop, base, [amount]);
                     check(result !== base && result !== nothing
                       && result._steps.show() === steps,
                       `_drop(${amount}) finalizer identity/steps: ${
                         result === base
                       }:${result === nothing}:${result?._steps?.show?.()}`);
                   }
                   const zeroSource = gap(0);
                   const zeroDrop = apply(rawDrop, zeroSource, [1]);
                   check(zeroDrop !== zeroSource && zeroDrop !== nothing
                     && zeroDrop._steps.show() === '0/1',
                     '_drop zero-step source lost registered finalization');
                   check(apply(rawTake, base, [[1, 2]])._steps.show()
                     === '1/2', '_take pair was not Fraction 1/2');
                   check(apply(rawDrop, base, [[1, 2]])._steps.show()
                     === '7/2', '_drop pair was not Fraction 1/2');

                   phase = 'pattern-rejection';
                   const patterned = sequence(1, 2);
                   for (const [name, raw] of [
                     ['_take', rawTake], ['_drop', rawDrop]
                   ]) {
                     let rejected;
                     try { apply(raw, base, [patterned]); }
                     catch (error) { rejected = error; }
                     check(rejected?.name === 'Error'
                       && rejected.message === 'Invalid argument',
                       `${name} patternified its raw amount`);
                   }

                   phase = 'retarget';
                   const decoy = sequence('decoy');
                   Object.defineProperty(decoy, 'hasSteps', {
                     configurable: true,
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   const target = sequence('x', 'y', 'z');
                   check(apply(rawTake, decoy, [1, target])._steps.show()
                     === '1/1', '_take did not retarget to arg two');
                   check(apply(rawDrop, decoy, [1, target, 'ignored'])
                     ._steps.show() === '2/1',
                     '_drop did not ignore arguments after its target');
                   check(new rawTake(1) === nothing,
                     'constructed _take did not return lexical nothing');
                   check(new rawDrop(1, target)._steps.show() === '2/1',
                     'constructed _drop did not retarget');

                   phase = 'early-returns';
                   const noSteps = new Pattern(() => []);
                   const poisonPair = [];
                   for (const index of [0, 1]) {
                     Object.defineProperty(poisonPair, index, {
                       get() { throw new Error('no-step pair read'); },
                     });
                   }
                   check(apply(rawTake, noSteps, [poisonPair]) === nothing
                     && apply(rawDrop, noSteps, [poisonPair]) === nothing,
                     'no-step target did not bypass coercion');
                   check(apply(rawTake, gap(-1), [Symbol('ignored')])
                     === nothing,
                     '_take did not guard nonpositive steps before coercion');
                   let nonpositiveDrop;
                   try { apply(rawDrop, gap(-1), [Symbol('observed')]); }
                   catch (error) { nonpositiveDrop = error; }
                   check(nonpositiveDrop?.name === 'Error'
                     && nonpositiveDrop.message === 'Invalid argument',
                     '_drop incorrectly inherited the take guard');

                   phase = 'take-order';
                   const takeLog = [];
                   const takeSteps = Fraction(4);
                   const takePair = [-1, 2];
                   Object.defineProperty(takePair, 0, {
                     configurable: true,
                     get() { takeLog.push('pair[0]'); return -1; },
                   });
                   Object.defineProperty(takePair, 1, {
                     configurable: true,
                     get() { takeLog.push('pair[1]'); return 2; },
                   });
                   const takeToken = {};
                   const takeTarget = {
                     get hasSteps() { takeLog.push('hasSteps'); return true; },
                     get _steps() { takeLog.push('_steps'); return takeSteps; },
                     get zoom() {
                       takeLog.push('zoom.get');
                       return function (begin, end) {
                         takeLog.push(`zoom.call:${this === takeTarget}`
                           + `:${begin.show()}:${end}`);
                         return takeToken;
                       };
                     },
                   };
                   const fractionPrototype = Fraction._original.prototype;
                   const takeOriginals = {};
                   for (const name of ['lte', 'eq', 'abs', 'div', 'gte', 'sub']) {
                     takeOriginals[name] = fractionPrototype[name];
                     fractionPrototype[name] = function (...args) {
                       takeLog.push(`fraction.${name}`);
                       return apply(takeOriginals[name], this, args);
                     };
                   }
                   let takeOrdered;
                   try {
                     takeOrdered = apply(rawTake, null, [takePair, takeTarget]);
                   } finally {
                     for (const name of Object.keys(takeOriginals)) {
                       fractionPrototype[name] = takeOriginals[name];
                     }
                   }
                   check(takeOrdered === takeToken, '_take replaced zoom result');
                   check(takeLog.join('|') === [
                     'hasSteps', '_steps', 'fraction.lte',
                     'pair[0]', 'pair[1]', 'fraction.eq', 'fraction.abs',
                     '_steps', 'fraction.div', 'fraction.lte', 'fraction.gte',
                     'zoom.get', 'fraction.sub', 'zoom.call:true:7/8:1'
                   ].join('|'), `_take order: ${takeLog}`);

                   phase = 'drop-order';
                   const dropLog = [];
                   const dropSteps = Fraction(4);
                   const dropPair = [1, 2];
                   Object.defineProperty(dropPair, 0, {
                     configurable: true,
                     get() { dropLog.push('pair[0]'); return 1; },
                   });
                   Object.defineProperty(dropPair, 1, {
                     configurable: true,
                     get() { dropLog.push('pair[1]'); return 2; },
                   });
                   const dropToken = {};
                   const dropTarget = {
                     get hasSteps() { dropLog.push('hasSteps'); return true; },
                     get _steps() { dropLog.push('_steps'); return dropSteps; },
                     get take() {
                       dropLog.push('take.get');
                       return function (amount) {
                         dropLog.push(`take.call:${this === dropTarget}`
                           + `:${amount.show()}`);
                         return dropToken;
                       };
                     },
                   };
                   const dropOriginals = {};
                   for (const name of ['lt', 'add', 'sub']) {
                     dropOriginals[name] = fractionPrototype[name];
                     fractionPrototype[name] = function (...args) {
                       dropLog.push(`fraction.${name}`);
                       return apply(dropOriginals[name], this, args);
                     };
                   }
                   let dropOrdered;
                   try {
                     dropOrdered = apply(rawDrop, null, [dropPair, dropTarget]);
                   } finally {
                     for (const name of Object.keys(dropOriginals)) {
                       fractionPrototype[name] = dropOriginals[name];
                     }
                   }
                   check(dropOrdered === dropToken, '_drop replaced take result');
                   check(dropLog.join('|') === [
                     'hasSteps', 'pair[0]', 'pair[1]', 'fraction.lt',
                     'take.get', '_steps', 'fraction.sub', 'fraction.sub',
                     'take.call:true:-7/2'
                   ].join('|'), `_drop order: ${dropLog}`);

                   phase = 'dynamic-public-take';
                   const savedTakeMethod = Pattern.prototype.take;
                   const savedAddAlias = Pattern.prototype.s_add;
                   let dynamicTakeHits = 0;
                   let dynamicDrop;
                   Pattern.prototype.take = function (amount) {
                     dynamicTakeHits++;
                     check(this === base && amount.show() === '-3/1',
                       'dynamic take receiver or amount');
                     return gap(2);
                   };
                   Pattern.prototype.s_add = function () {
                     throw new Error('s_add decoy reached');
                   };
                   try {
                     dynamicDrop = apply(rawDrop, base, [1]);
                   } finally {
                     Pattern.prototype.take = savedTakeMethod;
                     Pattern.prototype.s_add = savedAddAlias;
                   }
                   check(dynamicTakeHits === 1
                     && dynamicDrop._steps.show() === '2/1',
                     '_drop did not use the current public take once');

                   phase = 'lexical-poison';
                   const savedGlobals = {
                     Fraction: globalThis.Fraction,
                     nothing: globalThis.nothing,
                     take: globalThis.take,
                     drop: globalThis.drop,
                   };
                   const savedScope = {
                     take: rustelScope.take,
                     drop: rustelScope.drop,
                   };
                   const lexicalNothing = nothing;
                   const poison = function () {
                     throw new Error('public lexical decoy reached');
                   };
                   let lexicalTake;
                   let lexicalDrop;
                   let lexicalNoSteps;
                   globalThis.Fraction = poison;
                   globalThis.nothing = poison;
                   globalThis.take = poison;
                   globalThis.drop = poison;
                   rustelScope.take = poison;
                   rustelScope.drop = poison;
                   try {
                     lexicalTake = apply(rawTake, base, [1]);
                     lexicalDrop = apply(rawDrop, base, [1]);
                     lexicalNoSteps = apply(rawTake, noSteps, [Symbol('x')]);
                   } finally {
                     Object.assign(globalThis, savedGlobals);
                     Object.assign(rustelScope, savedScope);
                   }
                   check(lexicalTake._steps.show() === '1/1'
                     && lexicalDrop._steps.show() === '3/1'
                     && lexicalNoSteps === lexicalNothing,
                     'raw take/drop lost lexical Fraction or nothing');
                   globalThis.rawTakeDropRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawTakeDropRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawTakeDropRoutingError"), None);
    assert_eq!(runtime.get_number("rawTakeDropRoutingOkay"), Some(1.0));
    assert_clean(&runtime, "raw take/drop routing");
}

#[test]
fn raw_take_drop_are_eager_and_preserve_only_returned_ownership() {
    for (expression, expected) in [
        ("_take(1)", vec!["a"]),
        ("_take(-1)", vec!["d"]),
        ("_drop(1)", vec!["b", "c", "d"]),
        ("_drop(-1)", vec!["a", "b", "c"]),
    ] {
        let runtime = semantic_runtime(&format!("sequence('a', 'b', 'c', 'd').{expression}"));
        assert!(
            !runtime.active_needs_host(),
            "{expression} retained JavaScript on a scalar native path"
        );
        let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>();
        assert_eq!(values, expected, "{expression} selected the wrong steps");
        assert_clean(&runtime, expression);
        runtime.clear_active();
    }

    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawTakeEagerHits = 0;
             globalThis.rawTakeLateHits = 0;
             globalThis.rawDropEagerHits = 0;
             globalThis.rawDropLateHits = 0;
             globalThis.rawEagerMismatch = 0;

             const takeSource = sequence('a', 'b', 'c', 'd');
             takeSource.zoom = function (begin, end) {
               globalThis.rawTakeEagerHits++;
               if (this !== takeSource || begin !== 0
                   || !(end instanceof Fraction._original)
                   || end.show() !== '1/4') {
                 globalThis.rawEagerMismatch++;
               }
               return pure('raw-take-eager').setSteps(2);
             };
             const taken = takeSource._take(1);
             takeSource.zoom = function () {
               globalThis.rawTakeLateHits++;
               return pure('raw-take-late').setSteps(9);
             };

             const dropSource = sequence('a', 'b', 'c', 'd');
             dropSource.take = function (amount) {
               globalThis.rawDropEagerHits++;
               if (this !== dropSource
                   || !(amount instanceof Fraction._original)
                   || amount.show() !== '-3/1') {
                 globalThis.rawEagerMismatch++;
               }
               return pure('raw-drop-eager').setSteps(3);
             };
             const dropped = dropSource._drop(1);
             dropSource.take = function () {
               globalThis.rawDropLateHits++;
               return pure('raw-drop-late').setSteps(9);
             };
             return stack(taken, dropped);
           })()"#,
    );
    assert!(
        !eager.active_needs_host(),
        "eager raw overrides returning native graphs became query callbacks"
    );
    assert_eq!(eager.get_number("rawTakeEagerHits"), Some(1.0));
    assert_eq!(eager.get_number("rawDropEagerHits"), Some(1.0));
    assert_eq!(eager.get_number("rawEagerMismatch"), Some(0.0));
    let eager_values = query_active(&eager, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(eager_values.iter().any(|value| value == "raw-take-eager"));
    assert!(eager_values.iter().any(|value| value == "raw-drop-eager"));
    assert_eq!(eager.get_number("rawTakeLateHits"), Some(0.0));
    assert_eq!(eager.get_number("rawDropLateHits"), Some(0.0));
    assert_clean(&eager, "raw take/drop eager overrides");
    eager.clear_active();

    // Canonical public take finalization is load-bearing for ownership. An
    // equal-count drop wraps lexical nothing and prunes the source callback;
    // a zero drop wraps the full source and must retain it.
    let pruned = semantic_runtime(
        r#"(() => {
             globalThis.rawDropPrunedHits = 0;
             const source = new Pattern(state => {
               globalThis.rawDropPrunedHits++;
               return pure('raw-drop-pruned').query(state);
             }, 4);
             return source._drop(4);
           })()"#,
    );
    assert!(
        !pruned.active_needs_host(),
        "equal-count raw drop retained an unreachable receiver callback"
    );
    assert!(
        query_active(&pruned, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(pruned.get_number("rawDropPrunedHits"), Some(0.0));
    assert_clean(&pruned, "raw drop pruned ownership");
    pruned.clear_active();

    let retained = semantic_runtime(
        r#"(() => {
             globalThis.rawDropRetainedHits = 0;
             const source = new Pattern(state => {
               globalThis.rawDropRetainedHits++;
               return pure('raw-drop-retained').query(state);
             }, 4);
             return source._drop(0);
           })()"#,
    );
    assert!(
        retained.active_needs_host(),
        "zero raw drop lost the retained receiver callback"
    );
    retained.run_gc();
    retained.run_gc();
    retained.run_gc();
    let retained_values = query_active(&retained, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(retained_values, ["raw-drop-retained"]);
    assert_eq!(retained.get_number("rawDropRetainedHits"), Some(1.0));
    assert_clean(&retained, "raw drop retained ownership");
    retained.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let takeMarker = { marker: 'raw-take-owned' };
             let dropMarker = { marker: 'raw-drop-owned' };
             globalThis.rawTakeOwnedRef = new WeakRef(takeMarker);
             globalThis.rawDropOwnedRef = new WeakRef(dropMarker);
             let takeSource = sequence('a', 'b', 'c', 'd');
             let dropSource = sequence('a', 'b', 'c', 'd');
             takeSource.zoom = () => pure(takeMarker).setSteps(1);
             dropSource.take = () => pure(dropMarker).setSteps(1);
             const result = stack(
               takeSource._take(1),
               dropSource._drop(1)
             );
             takeMarker = null;
             dropMarker = null;
             takeSource = null;
             dropSource = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw override results lost their JS-owned values"
    );
    owned.run_gc();
    owned.run_gc();
    owned.run_gc();
    owned
        .eval(
            r#"globalThis.rawTakeOwnedAlive = Number(
                 rawTakeOwnedRef.deref()?.marker === 'raw-take-owned'
               );
               globalThis.rawDropOwnedAlive = Number(
                 rawDropOwnedRef.deref()?.marker === 'raw-drop-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawTakeOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawDropOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-take-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-drop-owned"))
    );
    assert_clean(&owned, "raw take/drop returned ownership");
    owned.clear_active();
}

#[test]
fn raw_take_drop_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b', 'c', 'd')._take([1, 2]),
             sequence('a', 'b', 'c', 'd')._drop([1, 2])
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw take/drop charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw take/drop charged query-time stepwise work"
    );
    assert_clean(&direct, "raw take/drop pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return sequence('a', 'b', 'c', 'd')._take([1, 2]);
             }}
             if (value === 1) {{
               return sequence('a', 'b', 'c', 'd')._drop([1, 2]);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw take/drop consumed any of the shared stepwise allowance"
    );
    assert_clean(&shared, "raw take/drop shared-pool neutrality");
    shared.clear_active();
}

#[test]
fn raw_expand_contract_and_with_steps_keep_surface_and_phase_order() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const withSteps = Pattern.prototype.withSteps;
                   const rawExpand = Pattern.prototype._expand;
                   const rawContract = Pattern.prototype._contract;

                   phase = 'withSteps-reflection';
                   const withStepsDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'withSteps'
                   );
                   check(withStepsDescriptor.value === withSteps
                     && withStepsDescriptor.writable
                     && !withStepsDescriptor.enumerable
                     && withStepsDescriptor.configurable,
                     'withSteps descriptor');
                   check(withSteps.name === 'withSteps'
                     && withSteps.length === 1
                     && !Object.prototype.hasOwnProperty.call(
                       withSteps, 'prototype'
                     ), 'withSteps reflection');
                   let withStepsConstruction;
                   try { new withSteps(() => 1); }
                   catch (error) { withStepsConstruction = error; }
                   check(withStepsConstruction instanceof TypeError,
                     'withSteps became constructible');
                   const elementalOrder = Reflect.ownKeys(Pattern.prototype)
                     .filter(name => [
                       'constructor', '_steps', 'setSteps', 'withSteps',
                       'hasSteps'
                     ].includes(name)).join('|');
                   check(elementalOrder
                     === 'constructor|_steps|setSteps|withSteps|hasSteps',
                     `elemental order: ${elementalOrder}`);

                   phase = 'raw-reflection';
                   check(Pattern.prototype.s_expand === Pattern.prototype.expand
                     && Pattern.prototype.s_contract
                       === Pattern.prototype.contract
                     && globalThis.s_expand === globalThis.expand
                     && globalThis.s_contract === globalThis.contract,
                     'raw install replaced copied aliases');
                   check(rawExpand !== Pattern.prototype.expand
                     && rawContract !== Pattern.prototype.contract
                     && rawExpand !== rawContract, 'raw identities');
                   for (const [name, raw] of [
                     ['_expand', rawExpand], ['_contract', rawContract]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} prototype descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${name} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(
                         raw, 'prototype'
                       ), `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'expand', '_expand', 'contract', '_contract',
                       's_expand', 's_contract'
                     ].includes(name)).join('|');
                   check(rawOrder
                     === 'expand|_expand|contract|_contract|s_expand|s_contract',
                     `raw expand/contract order: ${rawOrder}`);
                   check(['_s_expand', '_s_contract'].every(name =>
                     !(name in Pattern.prototype)
                     && !(name in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, name
                     )), 'raw aliases leaked');

                   phase = 'withSteps-order';
                   const source = pure('source').setSteps(2);
                   const sourceQuery = source.query;
                   const firstSteps = Fraction(2);
                   const secondSteps = Fraction(3);
                   const order = [];
                   let stepReads = 0;
                   Object.defineProperty(source, 'query', {
                     configurable: true,
                     get() {
                       order.push('query');
                       return sourceQuery;
                     },
                   });
                   Object.defineProperty(source, '_steps', {
                     configurable: true,
                     get() {
                       order.push(`steps:${stepReads}`);
                       return stepReads++ === 0 ? firstSteps : secondSteps;
                     },
                   });
                   const result = apply(withSteps, source, [steps => {
                     order.push(`callback:${steps === secondSteps}`);
                     return steps;
                   }]);
                   check(order.join('|')
                     === 'query|steps:0|steps:1|callback:true',
                     `withSteps order: ${order}`);
                   check(result !== source && result.query === sourceQuery,
                     'withSteps identity/query identity');
                   check(result._steps.show() === '3/1',
                     `withSteps final steps: ${result._steps?.show?.()}`);
                   check(!Object.prototype.hasOwnProperty.call(result, '__pure')
                     && !Object.prototype.hasOwnProperty.call(
                       result, '__pure_loc'
                     ), 'withSteps retained register fast-path tags');

                   phase = 'undefined-steps';
                   const noSteps = new Pattern(() => [], undefined);
                   const noStepsQuery = noSteps.query;
                   let callbackHits = 0;
                   const noStepsResult = noSteps.withSteps(() => {
                     callbackHits++;
                     return Symbol('must not convert');
                   });
                   const noStepsContract = noSteps._contract(0);
                   check(noStepsResult !== noSteps
                     && noStepsResult.query === noStepsQuery
                     && noStepsResult._steps === undefined
                     && callbackHits === 0,
                     'undefined steps called or converted its callback');
                   check(noStepsContract !== noSteps
                     && noStepsContract.query === noStepsQuery
                     && noStepsContract._steps === undefined,
                     'no-step contract zero reached division/conversion');
                   globalThis.rawExpandContractSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawExpandContractSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawExpandContractSurfaceError"), None);
    assert_eq!(
        runtime.get_number("rawExpandContractSurfaceOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "raw expand/contract surface");
}

#[test]
fn raw_expand_contract_delay_fraction_work_and_keep_throw_cutoffs() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawExpand = Pattern.prototype._expand;
                   const rawContract = Pattern.prototype._contract;

                   phase = 'delayed-expand';
                   const log = [];
                   const factor = new Proxy({ n: 2, d: 1 }, {
                     has(target, key) {
                       log.push(`factor.has:${String(key)}`);
                       return Reflect.has(target, key);
                     },
                     get(target, key, receiver) {
                       log.push(`factor.get:${String(key)}`);
                       return Reflect.get(target, key, receiver);
                     },
                   });
                   let delayed;
                   const terminal = { marker: 'expand-terminal' };
                   const target = {};
                   Object.defineProperty(target, 'withSteps', {
                     configurable: true,
                     get() {
                       log.push('withSteps.get');
                       return function (callback) {
                         log.push(`withSteps.call:${this === target}`);
                         delayed = callback;
                         return terminal;
                       };
                     },
                   });
                   const decoy = {};
                   Object.defineProperty(decoy, 'withSteps', {
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   check(apply(rawExpand, decoy, [
                     factor, target, 'ignored'
                   ]) === terminal, '_expand replaced override result');
                   check(log.join('|') === 'withSteps.get|withSteps.call:true',
                     `factor converted before callback: ${log}`);

                   const multiplied = { marker: 'multiplied' };
                   const steps = {};
                   Object.defineProperty(steps, 'mul', {
                     configurable: true,
                     get() {
                       log.push('mul.get');
                       return function (received) {
                         log.push(`mul.call:${this === steps}`
                           + `:${received.show()}`);
                         return multiplied;
                       };
                     },
                   });
                   check(delayed(steps) === multiplied,
                     'delayed expand callback result');
                   const mulGet = log.indexOf('mul.get');
                   const factorRead = log.findIndex(entry =>
                     entry.startsWith('factor.')
                   );
                   const mulCall = log.findIndex(entry =>
                     entry.startsWith('mul.call:')
                   );
                   check(mulGet >= 0 && factorRead > mulGet
                     && mulCall > factorRead
                     && log[mulCall] === 'mul.call:true:2/1',
                     `expand callback order: ${log}`);

                   phase = 'retarget-and-constructor';
                   const receiverTarget = {
                     withSteps(callback) {
                       check(this === receiverTarget,
                         'one-argument receiver changed');
                       return callback(Fraction(3));
                     },
                   };
                   check(apply(rawExpand, receiverTarget, [2]).show()
                     === '6/1', 'one explicit argument did not use receiver');
                   let omitted;
                   try { apply(rawExpand, receiverTarget, []); }
                   catch (error) { omitted = error; }
                   check(omitted instanceof TypeError,
                     'omission did not leave pat undefined');

                   const constructorObject = { marker: 'constructor-object' };
                   const constructorTarget = {
                     withSteps(callback) {
                       check(this === constructorTarget
                         && typeof callback === 'function',
                         'constructor target/callback');
                       return constructorObject;
                     },
                   };
                   check(Reflect.construct(
                     rawExpand, [factor, constructorTarget]
                   ) === constructorObject,
                   'constructor did not return object body result');
                   const primitiveTarget = { withSteps() { return 7; } };
                   const primitiveConstruction = Reflect.construct(
                     rawContract, [2, primitiveTarget]
                   );
                   check(primitiveConstruction instanceof rawContract,
                     'constructor returned primitive body result');

                   phase = 'withSteps-throw-cutoff';
                   let getterFactorHits = 0;
                   const getterFactor = new Proxy({ n: 2, d: 1 }, {
                     has(target, key) {
                       getterFactorHits++;
                       return Reflect.has(target, key);
                     },
                     get(target, key, receiver) {
                       getterFactorHits++;
                       return Reflect.get(target, key, receiver);
                     },
                   });
                   const getterStop = {};
                   Object.defineProperty(getterStop, 'withSteps', {
                     get() { throw new Error('withSteps-stop'); },
                   });
                   let getterError;
                   try { apply(rawExpand, null, [getterFactor, getterStop]); }
                   catch (error) { getterError = error; }
                   check(getterError?.message === 'withSteps-stop'
                     && getterFactorHits === 0,
                     'withSteps getter throw did not cut off conversion');

                   phase = 'div-throw-cutoff';
                   let divFactorHits = 0;
                   const divFactor = new Proxy({ n: 2, d: 1 }, {
                     has(target, key) {
                       divFactorHits++;
                       return Reflect.has(target, key);
                     },
                     get(target, key, receiver) {
                       divFactorHits++;
                       return Reflect.get(target, key, receiver);
                     },
                   });
                   const divSteps = {};
                   Object.defineProperty(divSteps, 'div', {
                     get() { throw new Error('div-stop'); },
                   });
                   let divError;
                   try {
                     apply(rawContract, null, [divFactor, {
                       withSteps(callback) { return callback(divSteps); },
                     }]);
                   } catch (error) { divError = error; }
                   check(divError?.message === 'div-stop'
                     && divFactorHits === 0,
                     'div getter throw did not cut off conversion');

                   phase = 'conversion-throw-cutoff';
                   let mulGets = 0;
                   let mulCalls = 0;
                   const conversionSteps = {};
                   Object.defineProperty(conversionSteps, 'mul', {
                     get() {
                       mulGets++;
                       return function () { mulCalls++; };
                     },
                   });
                   let conversionError;
                   try {
                     apply(rawExpand, null, [Symbol('bad-factor'), {
                       withSteps(callback) {
                         return callback(conversionSteps);
                       },
                     }]);
                   } catch (error) { conversionError = error; }
                   check(conversionError instanceof Error
                     && mulGets === 1 && mulCalls === 0,
                     'factor throw did not follow mul getter');
                   globalThis.rawExpandContractRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawExpandContractRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawExpandContractRoutingError"), None);
    assert_eq!(
        runtime.get_number("rawExpandContractRoutingOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "raw expand/contract routing");
}

#[test]
fn with_steps_tag_loss_forces_public_step_register_routing() {
    for method in ["expand", "contract"] {
        let runtime = semantic_runtime(&format!(
            r#"(() => {{
                 globalThis.withStepsPatternedHits = 0;
                 globalThis.withStepsDirectHits = 0;

                 const patternedSource = pure(2);
                 const patternedQuery = patternedSource.query;
                 patternedSource.query = state => {{
                   globalThis.withStepsPatternedHits++;
                   return Reflect.apply(
                     patternedQuery, patternedSource, [state]
                   );
                 }};
                 const patternedFactor = patternedSource.withSteps(
                   steps => steps
                 );

                 const directFactor = pure(2);
                 const directQuery = directFactor.query;
                 directFactor.query = state => {{
                   globalThis.withStepsDirectHits++;
                   return Reflect.apply(directQuery, directFactor, [state]);
                 }};

                 const patterned = pure('patterned').{method}(
                   patternedFactor
                 );
                 const direct = pure('direct').{method}(directFactor);
                 globalThis.withStepsPatternedConstructionHits =
                   globalThis.withStepsPatternedHits;
                 globalThis.withStepsDirectConstructionHits =
                   globalThis.withStepsDirectHits;
                 return stack(patterned, direct);
               }})()"#
        ));
        assert_eq!(
            runtime.get_number("withStepsPatternedConstructionHits"),
            Some(1.0),
            "public {method} did not query the stripped factor during stepJoin"
        );
        assert_eq!(
            runtime.get_number("withStepsDirectConstructionHits"),
            Some(0.0),
            "public {method} queried a direct pure factor during construction"
        );
        query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            runtime.get_number("withStepsPatternedHits"),
            Some(2.0),
            "public {method} did not keep the patterned factor route at query"
        );
        assert_eq!(
            runtime.get_number("withStepsDirectHits"),
            Some(0.0),
            "public {method} queried a direct pure factor"
        );
        assert_clean(&runtime, &format!("withSteps public {method} routing"));
        runtime.clear_active();
    }
}

#[test]
fn with_steps_tracks_native_query_identity_across_reassignment_and_repetition() {
    let runtime = semantic_runtime(
        r#"(() => {
             const a = pure('a');
             const b = pure('b');
             const copiedQuery = b.query;
             a.query = copiedQuery;

             const first = a.withSteps(steps => steps);
             const second = first.withSteps(steps => steps.mul(2));
             globalThis.withStepsCopiedFirst = Number(
               first !== a
               && first.query === copiedQuery
               && first._steps.show() === '1/1'
               && !Object.prototype.hasOwnProperty.call(first, '__pure')
             );
             globalThis.withStepsCopiedSecond = Number(
               second !== first
               && second.query === copiedQuery
               && second._steps.show() === '2/1'
               && !Object.prototype.hasOwnProperty.call(second, '__pure')
             );
             return second;
           })()"#,
    );
    assert_eq!(runtime.get_number("withStepsCopiedFirst"), Some(1.0));
    assert_eq!(runtime.get_number("withStepsCopiedSecond"), Some(1.0));
    assert!(
        !runtime.active_needs_host(),
        "native query reassignment or repeated withSteps fell into JS dispatch"
    );
    let pattern = runtime.active_pattern().expect("copied-query result");
    assert_eq!(pattern.steps, Some(Fraction::int(2)));
    assert!(pattern.is_pure(), "copied native query lost native purity");
    assert!(
        pattern.as_pure().is_none(),
        "repeated withSteps restored structural pure fast-path metadata"
    );
    assert_eq!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "b",
        "withSteps cloned the reassignment target instead of copied query"
    );
    assert_clean(&runtime, "withSteps native query reassignment");
    runtime.clear_active();
}

#[test]
fn raw_expand_contract_keep_timing_purity_ownership_and_pool_neutrality() {
    let with_steps = semantic_runtime("pure('with-steps').withSteps(t => t.mul(2))");
    let with_steps_pattern = with_steps
        .active_pattern()
        .expect("active withSteps result");
    assert_eq!(with_steps_pattern.steps, Some(Fraction::int(2)));
    assert!(
        with_steps_pattern.is_pure(),
        "metadata-only withSteps lost native purity"
    );
    assert!(
        with_steps_pattern.as_pure().is_none(),
        "withSteps retained the structural register fast path"
    );
    assert!(
        !with_steps.active_needs_host(),
        "native withSteps retained an eager callback"
    );
    assert_eq!(
        query_active(&with_steps, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "with-steps"
    );
    assert_clean(&with_steps, "host-free withSteps");
    with_steps.clear_active();

    for (expression, expected_steps) in [
        ("sequence('a', 'b')._expand(2)", Some(Fraction::int(4))),
        ("sequence('a', 'b')._expand(.5)", Some(Fraction::ONE)),
        ("sequence('a', 'b')._expand(-1)", Some(Fraction::int(-2))),
        ("sequence('a', 'b')._expand(0)", Some(Fraction::ZERO)),
        ("sequence('a', 'b')._contract(2)", Some(Fraction::ONE)),
        ("sequence('a', 'b')._contract(.5)", Some(Fraction::int(4))),
        ("sequence('a', 'b')._contract(-1)", Some(Fraction::int(-2))),
    ] {
        let runtime = semantic_runtime(expression);
        assert_eq!(
            runtime
                .active_pattern()
                .expect("active scalar result")
                .steps,
            expected_steps,
            "{expression}: steps"
        );
        assert!(
            !runtime.active_needs_host(),
            "{expression}: scalar metadata path retained JavaScript"
        );
        let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            haps.iter()
                .map(|hap| (
                    hap.value.show(),
                    hap.part.begin,
                    hap.part.end,
                    hap.whole.expect("sequence hap whole").begin,
                    hap.whole.expect("sequence hap whole").end,
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "a".to_string(),
                    Fraction::ZERO,
                    Fraction::new(1, 2),
                    Fraction::ZERO,
                    Fraction::new(1, 2),
                ),
                (
                    "b".to_string(),
                    Fraction::new(1, 2),
                    Fraction::ONE,
                    Fraction::new(1, 2),
                    Fraction::ONE,
                ),
            ],
            "{expression}: changing steps changed query timing"
        );
        assert_clean(&runtime, expression);
        runtime.clear_active();
    }

    let zero_contract = JsRuntime::new().unwrap();
    zero_contract.install_semantic_bindings().unwrap();
    let zero_error = zero_contract
        .evaluate_score(
            "sequence('a', 'b')._contract(0)",
            &TranspileOptions::default(),
        )
        .expect_err("raw contract zero must throw during construction");
    assert!(
        zero_error.contains("Division by Zero"),
        "unexpected raw contract zero error: {zero_error}"
    );
    assert_clean(&zero_contract, "raw contract zero");

    let lexical = semantic_runtime(
        r#"(() => {
             const left = sequence('a', 'b');
             const right = sequence('c', 'd');
             globalThis.rawExpandContractParserHits = 0;
             setStringParser(value => {
               globalThis.rawExpandContractParserHits++;
               return pure(value);
             });
             const savedGlobal = globalThis.Fraction;
             const savedScope = rustelScope.Fraction;
             const poison = () => {
               throw new Error('mutable Fraction surface reached');
             };
             globalThis.Fraction = poison;
             rustelScope.Fraction = poison;
             try {
               const expanded = left._expand('2');
               const contracted = right._contract('2');
               globalThis.rawExpandStringSteps = Number(expanded._steps);
               globalThis.rawContractStringSteps = Number(contracted._steps);
               return stack(expanded, contracted);
             } finally {
               globalThis.Fraction = savedGlobal;
               rustelScope.Fraction = savedScope;
             }
           })()"#,
    );
    assert_eq!(lexical.get_number("rawExpandContractParserHits"), Some(0.0));
    assert_eq!(lexical.get_number("rawExpandStringSteps"), Some(4.0));
    assert_eq!(lexical.get_number("rawContractStringSteps"), Some(1.0));
    assert!(
        !lexical.active_needs_host(),
        "lexical string factors retained parser JavaScript"
    );
    assert_clean(&lexical, "raw expand/contract lexical strings");
    lexical.clear_active();

    let no_steps =
        semantic_runtime("new Pattern(() => [], undefined)._expand(Symbol('unused-factor'))");
    assert_eq!(
        no_steps.active_pattern().expect("no-step raw expand").steps,
        None
    );
    assert!(
        query_active(&no_steps, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty(),
        "no-step custom query changed"
    );
    assert_clean(&no_steps, "raw expand undefined-step cutoff");
    no_steps.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let pureMarker = { marker: 'raw-expand-owned' };
             let customMarker = { marker: 'raw-contract-owned' };
             globalThis.rawExpandOwnedRef = new WeakRef(pureMarker);
             globalThis.rawContractOwnedRef = new WeakRef(customMarker);
             let pureSource = pure(pureMarker).setSteps(2);
             const customCallback = (ownedMarker => state => [{
               begin: state.span.begin,
               end: state.span.end,
               value: ownedMarker,
             }])(customMarker);
             let customSource = new Pattern(customCallback, 2);
             const pureQuery = pureSource.query;
             const customQuery = customSource.query;
             const expanded = pureSource._expand(2);
             const contracted = customSource._contract(2);
             globalThis.rawExpandOwnedQueryIdentity = Number(
               expanded.query === pureQuery
             );
             globalThis.rawContractOwnedQueryIdentity = Number(
               contracted.query === customQuery
             );
             pureMarker = null;
             customMarker = null;
             pureSource = null;
             customSource = null;
             return stack(expanded, contracted);
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "owned results lost JS dependencies"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawExpandOwnedAlive = Number(
                 rawExpandOwnedRef.deref()?.marker === 'raw-expand-owned'
               );
               globalThis.rawContractOwnedAlive = Number(
                 rawContractOwnedRef.deref()?.marker === 'raw-contract-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawExpandOwnedQueryIdentity"), Some(1.0));
    assert_eq!(owned.get_number("rawContractOwnedQueryIdentity"), Some(1.0));
    assert_eq!(owned.get_number("rawExpandOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawContractOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-expand-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-contract-owned"))
    );
    assert_clean(&owned, "raw expand/contract ownership");
    owned.clear_active();

    let pruned = semantic_runtime(
        r#"(() => {
             let dropped = { marker: 'raw-expand-dropped' };
             globalThis.rawExpandDroppedRef = new WeakRef(dropped);
             pure(dropped);
             dropped = null;
             return sequence('a', 'b')._expand(2);
           })()"#,
    );
    for _ in 0..3 {
        pruned.run_gc();
    }
    pruned
        .eval(
            "globalThis.rawExpandDroppedCollected = Number(\
               rawExpandDroppedRef.deref() === undefined);",
        )
        .unwrap();
    assert_eq!(pruned.get_number("rawExpandDroppedCollected"), Some(1.0));
    assert_clean(&pruned, "raw expand unrelated-owner pruning");
    pruned.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b')._expand(2),
             sequence('c', 'd')._contract(2)
           )"#,
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw expand/contract charged stepwise work"
    );
    assert_clean(&direct, "raw expand/contract direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) return sequence('a', 'b')._expand(2);
             if (value === 1) return sequence('c', 'd')._contract(2);
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw expand/contract consumed shared stepwise allowance"
    );
    assert_clean(&shared, "raw expand/contract shared-pool neutrality");
    shared.clear_active();
}

#[test]
fn raw_extend_replicate_keep_surface_and_dynamic_chain_routing() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawExtend = Pattern.prototype._extend;
                   const rawReplicate = Pattern.prototype._replicate;

                   phase = 'surface';
                   check(Pattern.prototype.s_extend === Pattern.prototype.extend
                     && globalThis.s_extend === globalThis.extend
                     && rustelScope.s_extend === rustelScope.extend,
                     'raw install replaced the copied extend alias');
                   check(rawExtend !== Pattern.prototype.extend
                     && rawReplicate !== Pattern.prototype.replicate
                     && rawExtend !== rawReplicate, 'raw identities');
                   for (const [name, raw] of [
                     ['_extend', rawExtend], ['_replicate', rawReplicate]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} prototype descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${name} own keys`);
                     const length = Object.getOwnPropertyDescriptor(raw, 'length');
                     const functionName = Object.getOwnPropertyDescriptor(raw, 'name');
                     const functionPrototype = Object.getOwnPropertyDescriptor(
                       raw, 'prototype'
                     );
                     const constructor = Object.getOwnPropertyDescriptor(
                       raw.prototype, 'constructor'
                     );
                     check(length.value === 0 && !length.writable
                       && !length.enumerable && length.configurable,
                       `${name} length descriptor`);
                     check(functionName.value === '' && !functionName.writable
                       && !functionName.enumerable && functionName.configurable,
                       `${name} name descriptor`);
                     check(functionPrototype.writable
                       && !functionPrototype.enumerable
                       && !functionPrototype.configurable
                       && constructor.value === raw
                       && constructor.writable
                       && !constructor.enumerable
                       && constructor.configurable,
                       `${name} function prototype`);
                     check(Object.getPrototypeOf(raw) === Function.prototype
                       && Object.getPrototypeOf(raw.prototype) === Object.prototype,
                       `${name} prototype chains`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'extend', '_extend', 'replicate', '_replicate',
                       's_extend'
                     ].includes(name)).join('|');
                   check(rawOrder
                     === 'extend|_extend|replicate|_replicate|s_extend',
                     `raw extend/replicate order: ${rawOrder}`);
                   check(['_pace', '_s_extend',
                     '_s_replicate', 's_replicate'].every(name =>
                       !(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       )
                       && !(name in Pattern.prototype)
                     ), 'adjacent raw or alias surface expanded');

                   phase = 'omission';
                   for (const [name, raw] of [
                     ['_extend', rawExtend], ['_replicate', rawReplicate]
                   ]) {
                     let omitted;
                     try { apply(raw, {}, []); }
                     catch (error) { omitted = error; }
                     check(omitted instanceof TypeError,
                       `${name} omission did not leave pat undefined`);
                     let constructed;
                     try { new raw({ marker: name }); }
                     catch (error) { constructed = error; }
                     check(constructed instanceof TypeError,
                       `${name} constructor unexpectedly used its wrapper target`);
                   }

                   phase = 'extend-chain';
                   const factor = { marker: 'same-factor' };
                   const extendLog = [];
                   const extendFinal = { marker: 'extend-final' };
                   const extendMiddle = {};
                   Object.defineProperty(extendMiddle, 'expand', {
                     configurable: true,
                     get() {
                       extendLog.push('expand.get');
                       return function (received) {
                         extendLog.push(`expand.call:${this === extendMiddle}`
                           + `:${received === factor}`);
                         return extendFinal;
                       };
                     },
                   });
                   const extendTarget = {};
                   Object.defineProperty(extendTarget, 'fast', {
                     configurable: true,
                     get() {
                       extendLog.push('fast.get');
                       return function (received) {
                         extendLog.push(`fast.call:${this === extendTarget}`
                           + `:${received === factor}`);
                         return extendMiddle;
                       };
                     },
                   });
                   const decoy = {};
                   Object.defineProperty(decoy, 'fast', {
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   check(apply(rawExtend, decoy, [
                     factor, extendTarget, 'ignored'
                   ]) === extendFinal, '_extend replaced the terminal result');
                   check(extendLog.join('|') === [
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_extend chain order: ${extendLog}`);
                   extendLog.length = 0;
                   check(Reflect.construct(
                     rawExtend, [factor, extendTarget]
                   ) === extendFinal, '_extend constructor did not retarget');
                   check(extendLog.join('|') === [
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_extend constructor order: ${extendLog}`);

                   phase = 'replicate-chain';
                   const replicateLog = [];
                   const replicateFinal = { marker: 'replicate-final' };
                   const afterRepeat = {};
                   const afterFast = {};
                   Object.defineProperty(afterFast, 'expand', {
                     configurable: true,
                     get() {
                       replicateLog.push('expand.get');
                       return function (received) {
                         replicateLog.push(`expand.call:${this === afterFast}`
                           + `:${received === factor}`);
                         return replicateFinal;
                       };
                     },
                   });
                   Object.defineProperty(afterRepeat, 'fast', {
                     configurable: true,
                     get() {
                       replicateLog.push('fast.get');
                       return function (received) {
                         replicateLog.push(`fast.call:${this === afterRepeat}`
                           + `:${received === factor}`);
                         return afterFast;
                       };
                     },
                   });
                   const replicateTarget = {};
                   Object.defineProperty(replicateTarget, 'repeatCycles', {
                     configurable: true,
                     get() {
                       replicateLog.push('repeatCycles.get');
                       return function (received) {
                         replicateLog.push(
                           `repeatCycles.call:${this === replicateTarget}`
                           + `:${received === factor}`
                         );
                         return afterRepeat;
                       };
                     },
                   });
                   Object.defineProperty(decoy, 'repeatCycles', {
                     get() { throw new Error('replicate wrapper receiver reached'); },
                   });
                   check(apply(rawReplicate, decoy, [
                     factor, replicateTarget, 'ignored', 'ignored again'
                   ]) === replicateFinal,
                   '_replicate replaced the terminal result');
                   check(replicateLog.join('|') === [
                     'repeatCycles.get', 'repeatCycles.call:true:true',
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_replicate chain order: ${replicateLog}`);
                   replicateLog.length = 0;
                   check(Reflect.construct(
                     rawReplicate, [factor, replicateTarget]
                   ) === replicateFinal,
                   '_replicate constructor did not retarget');
                   check(replicateLog.join('|') === [
                     'repeatCycles.get', 'repeatCycles.call:true:true',
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_replicate constructor order: ${replicateLog}`);

                   phase = 'primitive-constructor-results';
                   const primitiveExtendTarget = {
                     fast(received) {
                       check(this === primitiveExtendTarget
                         && received === factor,
                         'primitive extend fast receiver or factor');
                       return {
                         expand(next) {
                           check(next === factor,
                             'primitive extend repeated factor');
                           return 17;
                         },
                       };
                     },
                   };
                   check(apply(rawExtend, null, [
                     factor, primitiveExtendTarget
                   ]) === 17, '_extend call replaced a primitive result');
                   const constructedExtend = Reflect.construct(
                     rawExtend, [factor, primitiveExtendTarget]
                   );
                   check(constructedExtend instanceof rawExtend,
                     '_extend constructor returned its primitive body result');

                   const primitiveReplicateTarget = {
                     repeatCycles(received) {
                       check(this === primitiveReplicateTarget
                         && received === factor,
                         'primitive replicate receiver or factor');
                       return {
                         fast(next) {
                           check(next === factor,
                             'primitive replicate fast factor');
                           return {
                             expand(last) {
                               check(last === factor,
                                 'primitive replicate expand factor');
                               return 'replicate-terminal';
                             },
                           };
                         },
                       };
                     },
                   };
                   check(apply(rawReplicate, null, [
                     factor, primitiveReplicateTarget
                   ]) === 'replicate-terminal',
                   '_replicate call replaced a primitive result');
                   const constructedReplicate = Reflect.construct(
                     rawReplicate, [factor, primitiveReplicateTarget]
                   );
                   check(constructedReplicate instanceof rawReplicate,
                     '_replicate constructor returned its primitive body result');

                   phase = 'dynamic-prototype';
                   const base = sequence('a', 'b');
                   const saved = {
                     fast: Pattern.prototype.fast,
                     expand: Pattern.prototype.expand,
                     repeatCycles: Pattern.prototype.repeatCycles,
                   };
                   const dynamicLog = [];
                   const extended = pure('dynamic-extend').setSteps(2);
                   const replicated = pure('dynamic-replicate').setSteps(3);
                   let mode = 'extend';
                   Pattern.prototype.repeatCycles = function (received) {
                     dynamicLog.push(`repeat:${this === base}:${received === factor}`);
                     return this;
                   };
                   Pattern.prototype.fast = function (received) {
                     dynamicLog.push(`fast:${this === base}:${received === factor}`);
                     return this;
                   };
                   Pattern.prototype.expand = function (received) {
                     dynamicLog.push(`expand:${this === base}:${received === factor}`);
                     return mode === 'extend' ? extended : replicated;
                   };
                   let dynamicExtend;
                   let dynamicReplicate;
                   try {
                     dynamicExtend = apply(rawExtend, base, [factor]);
                     mode = 'replicate';
                     dynamicReplicate = apply(rawReplicate, base, [factor]);
                   } finally {
                     Object.assign(Pattern.prototype, saved);
                   }
                   check(dynamicExtend === extended
                     && dynamicReplicate === replicated,
                     'saved raws did not use the current public chain');
                   check(dynamicLog.join('|') === [
                     'fast:true:true', 'expand:true:true',
                     'repeat:true:true', 'fast:true:true', 'expand:true:true'
                   ].join('|'), `dynamic prototype chain: ${dynamicLog}`);

                   phase = 'global-poison';
                   const savedGlobals = {
                     fast: globalThis.fast,
                     expand: globalThis.expand,
                     repeatCycles: globalThis.repeatCycles,
                   };
                   const savedScope = {
                     fast: rustelScope.fast,
                     expand: rustelScope.expand,
                     repeatCycles: rustelScope.repeatCycles,
                   };
                   const poison = () => {
                     throw new Error('global chain decoy reached');
                   };
                   globalThis.fast = poison;
                   globalThis.expand = poison;
                   globalThis.repeatCycles = poison;
                   rustelScope.fast = poison;
                   rustelScope.expand = poison;
                   rustelScope.repeatCycles = poison;
                   let lexicalExtend;
                   let lexicalReplicate;
                   try {
                     lexicalExtend = apply(rawExtend, null, [
                       factor, extendTarget
                     ]);
                     lexicalReplicate = apply(rawReplicate, null, [
                       factor, replicateTarget
                     ]);
                   } finally {
                     Object.assign(globalThis, savedGlobals);
                     Object.assign(rustelScope, savedScope);
                   }
                   check(lexicalExtend === extendFinal
                     && lexicalReplicate === replicateFinal,
                     'raw bodies resolved a global or scope method');
                   globalThis.rawExtendReplicateRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawExtendReplicateRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawExtendReplicateRoutingError"), None);
    assert_eq!(
        runtime.get_number("rawExtendReplicateRoutingOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "raw extend/replicate routing");
}

#[test]
fn raw_extend_replicate_are_eager_and_preserve_returned_ownership() {
    let extend = semantic_runtime("slowcat(pure('a'), pure('b')).setSteps(2)._extend(2)");
    assert!(
        !extend.active_needs_host(),
        "scalar raw extend retained JavaScript"
    );
    assert_eq!(
        extend.active_pattern().expect("active raw extend").steps,
        Some(Fraction::int(4))
    );
    let extend_values = query_active(&extend, Fraction::ZERO, Fraction::int(2))
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(extend_values, ["a", "b", "a", "b"]);
    assert_clean(&extend, "scalar raw extend");
    extend.clear_active();

    let replicate = semantic_runtime("slowcat(pure('a'), pure('b')).setSteps(2)._replicate(2)");
    assert!(
        !replicate.active_needs_host(),
        "scalar raw replicate retained JavaScript"
    );
    assert_eq!(
        replicate
            .active_pattern()
            .expect("active raw replicate")
            .steps,
        Some(Fraction::int(4))
    );
    let replicate_values = query_active(&replicate, Fraction::ZERO, Fraction::int(2))
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(replicate_values, ["a", "a", "b", "b"]);
    assert_clean(&replicate, "scalar raw replicate");
    replicate.clear_active();

    for (method, factor, expected_steps, expected_values) in [
        ("_extend", "1/2", Fraction::ONE, vec!["a"]),
        ("_replicate", "1/2", Fraction::ONE, vec!["a"]),
        ("_extend", "0", Fraction::ZERO, Vec::new()),
        ("_replicate", "0", Fraction::ZERO, Vec::new()),
    ] {
        let runtime = semantic_runtime(&format!("sequence('a', 'b').{method}({factor})"));
        assert_eq!(
            runtime
                .active_pattern()
                .expect("active raw numeric case")
                .steps,
            Some(expected_steps),
            "{method}({factor}) step metadata"
        );
        assert!(
            !runtime.active_needs_host(),
            "{method}({factor}) retained JavaScript"
        );
        let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>();
        assert_eq!(values, expected_values, "{method}({factor}) values");
        assert_clean(&runtime, &format!("{method}({factor})"));
        runtime.clear_active();
    }

    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawExtendEagerHits = 0;
             globalThis.rawReplicateEagerHits = 0;
             globalThis.rawExtendReplicateLateHits = 0;
             globalThis.rawExtendReplicateMismatch = 0;
             const factor = { marker: 'raw-factor' };

             const extendSource = sequence('a', 'b');
             const extendMiddle = {};
             extendSource.fast = function (received) {
               globalThis.rawExtendEagerHits++;
               if (this !== extendSource || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return extendMiddle;
             };
             extendMiddle.expand = function (received) {
               globalThis.rawExtendEagerHits++;
               if (this !== extendMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return pure('raw-extend-eager').setSteps(2);
             };
             const extended = extendSource._extend(factor);
             extendSource.fast = extendMiddle.expand = function () {
               globalThis.rawExtendReplicateLateHits++;
               return pure('raw-extend-late');
             };

             const replicateSource = sequence('c', 'd');
             const repeatMiddle = {};
             const fastMiddle = {};
             replicateSource.repeatCycles = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== replicateSource || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return repeatMiddle;
             };
             repeatMiddle.fast = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== repeatMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return fastMiddle;
             };
             fastMiddle.expand = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== fastMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return pure('raw-replicate-eager').setSteps(3);
             };
             const replicated = replicateSource._replicate(factor);
             replicateSource.repeatCycles = repeatMiddle.fast
               = fastMiddle.expand = function () {
                 globalThis.rawExtendReplicateLateHits++;
                 return pure('raw-replicate-late');
               };
             return stack(extended, replicated);
           })()"#,
    );
    assert!(
        !eager.active_needs_host(),
        "eager raw chains returning native graphs retained JavaScript"
    );
    assert_eq!(eager.get_number("rawExtendEagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawReplicateEagerHits"), Some(3.0));
    assert_eq!(eager.get_number("rawExtendReplicateMismatch"), Some(0.0));
    let eager_values = query_active(&eager, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(eager_values.iter().any(|value| value == "raw-extend-eager"));
    assert!(
        eager_values
            .iter()
            .any(|value| value == "raw-replicate-eager")
    );
    assert_eq!(eager.get_number("rawExtendReplicateLateHits"), Some(0.0));
    assert_clean(&eager, "raw extend/replicate eager overrides");
    eager.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let extendMarker = { marker: 'raw-extend-owned' };
             let replicateMarker = { marker: 'raw-replicate-owned' };
             globalThis.rawExtendOwnedRef = new WeakRef(extendMarker);
             globalThis.rawReplicateOwnedRef = new WeakRef(replicateMarker);
             let extendSource = sequence('a', 'b');
             let replicateSource = sequence('c', 'd');
             extendSource.fast = () => ({
               expand: () => pure(extendMarker).setSteps(1),
             });
             replicateSource.repeatCycles = () => ({
               fast: () => ({
                 expand: () => pure(replicateMarker).setSteps(1),
               }),
             });
             const result = stack(
               extendSource._extend(2),
               replicateSource._replicate(2)
             );
             extendMarker = null;
             replicateMarker = null;
             extendSource = null;
             replicateSource = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw chain results lost their JavaScript-owned values"
    );
    owned.run_gc();
    owned.run_gc();
    owned.run_gc();
    owned
        .eval(
            r#"globalThis.rawExtendOwnedAlive = Number(
                 rawExtendOwnedRef.deref()?.marker === 'raw-extend-owned'
               );
               globalThis.rawReplicateOwnedAlive = Number(
                 rawReplicateOwnedRef.deref()?.marker === 'raw-replicate-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawExtendOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawReplicateOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-extend-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-replicate-owned"))
    );
    assert_clean(&owned, "raw extend/replicate returned ownership");
    owned.clear_active();
}

#[test]
fn raw_extend_replicate_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b')._extend(2),
             slowcat(pure('c'), pure('d')).setSteps(2)._replicate(2)
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw extend/replicate charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw extend/replicate charged query-time stepwise work"
    );
    assert_clean(&direct, "raw extend/replicate pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return sequence('a', 'b')._extend(2);
             }}
             if (value === 1) {{
               return slowcat(pure('c'), pure('d'))
                 .setSteps(2)._replicate(2);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw extend/replicate consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw extend/replicate shared-pool neutrality");
    shared.clear_active();
}

#[test]
fn public_swing_preserves_scalar_steps_and_keeps_zero_no_step_and_patterned_boundaries() {
    let cases = [
        (
            "scalar swing",
            "pure(0).swing(4)",
            Some(Fraction::ONE),
            8usize,
        ),
        (
            "scalar swingBy",
            "pure(0).setSteps(8).swingBy(1/3, 4)",
            Some(Fraction::int(8)),
            8,
        ),
        (
            "fractional swing",
            "pure(0).setSteps(8).swing(.5)",
            Some(Fraction::int(8)),
            1,
        ),
        (
            "negative swing",
            "pure(0).setSteps(8).swing(-2)",
            Some(Fraction::int(8)),
            0,
        ),
        (
            "zero swing",
            "pure(0).setSteps(8).swing(0)",
            Some(Fraction::ONE),
            0,
        ),
        (
            "no-step swing",
            "pure(0).setSteps(undefined).swing(4)",
            None,
            8,
        ),
        (
            "patterned swing boundary",
            "pure(0).setSteps(8).swing(fastcat(2, 4))",
            None,
            6,
        ),
    ];

    for (label, source, expected_steps, expected_haps) in cases {
        let runtime = semantic_runtime(source);
        let pattern = runtime.active_pattern().expect("active public swing");
        assert_eq!(pattern.steps, expected_steps, "{label}: steps");
        assert!(pattern.is_pure(), "{label}: native graph lost purity");
        assert!(
            !runtime.active_needs_host(),
            "{label}: native graph retained JavaScript"
        );
        let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(haps.len(), expected_haps, "{label}: hap count");
        if label == "patterned swing boundary" {
            assert_eq!(
                haps.iter()
                    .map(|hap| (hap.part.begin, hap.part.end))
                    .collect::<Vec<_>>(),
                [
                    (Fraction::ZERO, Fraction::new(1, 4)),
                    (Fraction::new(1, 4), Fraction::new(1, 2)),
                    (Fraction::new(1, 2), Fraction::new(5, 8)),
                    (Fraction::new(5, 8), Fraction::new(3, 4)),
                    (Fraction::new(3, 4), Fraction::new(7, 8)),
                    (Fraction::new(7, 8), Fraction::ONE),
                ],
                "patterned public swing timing changed"
            );
        }
        assert_clean(&runtime, label);
        runtime.clear_active();
    }
}

#[test]
fn raw_swing_keeps_surface_strict_forwarding_dynamic_dispatch_parser_scalars_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const hasOwn = Object.prototype.hasOwnProperty;
                   const raw = Pattern.prototype._swing;

                   phase = 'surface';
                   check(typeof raw === 'function'
                     && raw !== Pattern.prototype.swing
                     && raw !== Pattern.prototype.swingBy,
                     'raw identity');
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_swing'
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(Reflect.ownKeys(raw).join('|')
                     === 'length|name|prototype', 'raw own keys');
                   check(raw.name === '' && raw.length === 0
                     && hasOwn.call(raw, 'prototype'), 'raw reflection');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw constructibility');
                   check(!hasOwn.call(globalThis, '_swing')
                     && !hasOwn.call(rustelScope, '_swing')
                     && hasOwn.call(Pattern.prototype, '_swing'),
                     'raw publication');
                   check(!hasOwn.call(Pattern.prototype, '_swingBy')
                     && !hasOwn.call(globalThis, '_swingBy')
                     && !hasOwn.call(rustelScope, '_swingBy'),
                     'raw swingBy residual leaked');
                   const projected = [
                     'swingBy', '_swingBy', 'swing', '_swing'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === 'swingBy|swing|_swing',
                     'filtered own-key order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === 'swingBy|swing|_swing',
                     'filtered enumerable order');

                   phase = 'strict-forwarding';
                   const factor = new Proxy(Object.create(null), {
                     get() { throw new Error('raw swing inspected factor'); },
                     set() { throw new Error('raw swing wrote factor'); },
                     ownKeys() { throw new Error('raw swing listed factor'); },
                   });
                   let zeroHits = 0;
                   const zeroReceiver = {
                     swingBy() { zeroHits++; return null; },
                   };
                   let zeroError;
                   try { apply(raw, zeroReceiver, []); }
                   catch (error) { zeroError = error; }
                   check(zeroError instanceof TypeError && zeroHits === 0,
                     'zero-argument shift');
                   for (const receiver of [null, undefined]) {
                     let error;
                     try { apply(raw, receiver, [factor]); }
                     catch (caught) { error = caught; }
                     check(error instanceof TypeError,
                       'nullish strict receiver did not throw');
                   }
                   for (const [receiver, prototype] of [
                     [7, Number.prototype],
                     ['x', String.prototype],
                     [true, Boolean.prototype],
                     [1n, BigInt.prototype],
                     [Symbol('receiver'), Symbol.prototype],
                   ]) {
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'swingBy'
                     );
                     Object.defineProperty(prototype, 'swingBy', {
                       configurable: true,
                       value: function (swing, received) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         check(swing === 1 / 3 && received === factor,
                           'primitive forwarding changed');
                         return receiver;
                       },
                     });
                     try {
                       check(Object.is(
                         apply(raw, receiver, [factor]), receiver
                       ), 'primitive terminal changed');
                     } finally {
                       if (prior === undefined) {
                         delete prototype.swingBy;
                       } else {
                         Object.defineProperty(
                           prototype, 'swingBy', prior
                         );
                       }
                     }
                   }

                   phase = 'dynamic-dispatch';
                   const target = {};
                   const terminal = { marker: 'terminal' };
                   let getterHits = 0;
                   let callHits = 0;
                   const callable = new Proxy(function () {}, {
                     apply(_function, thisArg, args) {
                       callHits++;
                       check(thisArg === target,
                         'dynamic method receiver changed');
                       check(args.length === 2
                         && args[0] === 1 / 3
                         && args[1] === factor,
                         'dynamic arguments changed');
                       return terminal;
                     },
                   });
                   Object.defineProperty(target, 'swingBy', {
                     configurable: true,
                     get() { getterHits++; return callable; },
                   });
                   check(apply(raw, target, [factor]) === terminal,
                     'one-argument dispatch');
                   const outerReceivers = [
                     null, undefined, 7, 'outer', 1n, Symbol('outer')
                   ];
                   for (const receiver of outerReceivers) {
                     check(apply(raw, receiver, [factor, target])
                       === terminal, 'explicit target dispatch');
                   }
                   const ignored = new Proxy({}, {
                     get() { throw new Error('read ignored extra'); },
                   });
                   check(apply(raw, null, [factor, target, ignored, 9])
                     === terminal, 'ignored extras changed dispatch');
                   check(getterHits === outerReceivers.length + 2
                     && callHits === getterHits,
                     'dynamic getter/call count');

                   const getterSentinel = { marker: 'getter-stop' };
                   const cutoffTarget = {};
                   Object.defineProperty(cutoffTarget, 'swingBy', {
                     get() { throw getterSentinel; },
                   });
                   let getterCaught;
                   try { apply(raw, null, [factor, cutoffTarget]); }
                   catch (error) { getterCaught = error; }
                   check(getterCaught === getterSentinel,
                     'getter throw identity');
                   const callSentinel = { marker: 'call-stop' };
                   let callCaught;
                   try {
                     apply(raw, null, [factor, {
                       swingBy() { throw callSentinel; },
                     }]);
                   } catch (error) { callCaught = error; }
                   check(callCaught === callSentinel,
                     'call throw identity');

                   phase = 'parser-and-scalars';
                   const parserLog = [];
                   setStringParser(value => {
                     parserLog.push(value);
                     return pure(Number(value));
                   });
                   const parsed = apply(raw, pure(0), ['4']);
                   check(parserLog.join('|') === '4'
                     && Number(parsed._steps) === 1,
                     `configured parser phase: ${parserLog}`);
                   const state = {
                     span: { begin: 0, end: 1 }, controls: {},
                   };
                   check(parsed.query(state).length === 8
                     && parserLog.join('|') === '4',
                     'configured parser reread');
                   const parserGetterSentinel = {
                     marker: 'parser-getter-stop',
                   };
                   const parserGetterTarget = {};
                   Object.defineProperty(parserGetterTarget, 'swingBy', {
                     get() { throw parserGetterSentinel; },
                   });
                   let parserGetterCaught;
                   try {
                     apply(raw, null, ['5', parserGetterTarget]);
                   } catch (error) { parserGetterCaught = error; }
                   check(parserGetterCaught === parserGetterSentinel
                     && parserLog.join('|') === '4',
                     'dynamic getter did not cut off parser');
                   const customTerminal = {};
                   check(apply(raw, null, ['custom', {
                     swingBy(swing, received) {
                       check(this !== undefined
                         && swing === 1 / 3
                         && received === 'custom',
                         'custom string dispatch');
                       return customTerminal;
                     },
                   }]) === customTerminal
                     && parserLog.join('|') === '4',
                     'custom dispatch entered parser');
                   for (const [n, steps, count] of [
                     [4, 8, 8], [.5, 8, 1], [-2, 8, 0], [0, 1, 0],
                   ]) {
                     const result = apply(
                       raw, pure(0).setSteps(8), [n]
                     );
                     check(Number(result._steps) === steps
                       && result.query(state).length === count,
                       `scalar ${n} changed`);
                   }
                   check(parserLog.join('|') === '4',
                     'numeric scalar entered parser');
                   setStringParser(undefined);

                   phase = 'saved-and-constructors';
                   const originalSlot = Pattern.prototype._swing;
                   const originalSwing = Pattern.prototype.swing;
                   const originalGlobalSwing = globalThis.swing;
                   const originalScopeSwing = rustelScope.swing;
                   Pattern.prototype._swing = () => {
                     throw new Error('poisoned raw slot');
                   };
                   Pattern.prototype.swing = () => {
                     throw new Error('poisoned public swing');
                   };
                   globalThis.swing = () => {
                     throw new Error('poisoned global swing');
                   };
                   rustelScope.swing = () => {
                     throw new Error('poisoned scope swing');
                   };
                   const savedTerminal = {};
                   const savedTarget = {
                     swingBy(swing, received) {
                       check(this === savedTarget
                         && swing === 1 / 3
                         && received === factor,
                         'saved raw dispatch changed');
                       return savedTerminal;
                     },
                   };
                   check(apply(raw, null, [factor, savedTarget])
                     === savedTerminal, 'saved raw followed poison');
                   Pattern.prototype._swing = originalSlot;
                   Pattern.prototype.swing = originalSwing;
                   globalThis.swing = originalGlobalSwing;
                   rustelScope.swing = originalScopeSwing;

                   let oneArgumentConstruction;
                   try { new raw(4); }
                   catch (error) { oneArgumentConstruction = error; }
                   check(oneArgumentConstruction instanceof TypeError,
                     'one-argument construction did not dispatch to instance');
                   const objectTerminal = {};
                   const constructorTarget = {
                     mode: 'object',
                     swingBy(swing, received) {
                       check(this === constructorTarget
                         && swing === 1 / 3 && received === 4,
                         'constructor forwarding changed');
                       return this.mode === 'object' ? objectTerminal : 7;
                     },
                   };
                   check(new raw(4, constructorTarget) === objectTerminal,
                     'object constructor terminal');
                   constructorTarget.mode = 'primitive';
                   const primitiveConstruction = new raw(
                     4, constructorTarget
                   );
                   check(primitiveConstruction instanceof raw,
                     'primitive constructor fallback');
                   function Redirect() {}
                   const redirected = Reflect.construct(
                     raw, [4, constructorTarget], Redirect
                   );
                   check(redirected instanceof Redirect
                     && !(redirected instanceof raw),
                     'redirected primitive constructor fallback');
                   constructorTarget.mode = 'object';
                   check(Reflect.construct(
                     raw, [4, constructorTarget], Redirect
                   ) === objectTerminal,
                   'redirected object constructor terminal');

                   globalThis.rawSwingSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawSwingSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSwingSurfaceError"), None);
    assert_eq!(runtime.get_number("rawSwingSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw swing surface and dispatch");
}

#[test]
fn raw_swing_preserves_selected_ownership_and_existing_resource_accounting() {
    let native = semantic_runtime(
        r#"(() => {
             const source = pure('native-swing').setSteps(8);
             source.__pure_loc = { start: 10, end: 11 };
             const result = source._swing(4);
             globalThis.rawSwingTagLoss = Number(
               Object.prototype.hasOwnProperty.call(source, '__pure')
               && Object.prototype.hasOwnProperty.call(
                 source, '__pure_loc'
               )
               && !Object.prototype.hasOwnProperty.call(result, '__pure')
               && !Object.prototype.hasOwnProperty.call(
                 result, '__pure_loc'
               )
             );
             return result;
           })()"#,
    );
    assert_eq!(native.get_number("rawSwingTagLoss"), Some(1.0));
    let native_pattern = native.active_pattern().expect("active raw swing");
    assert_eq!(native_pattern.steps, Some(Fraction::int(8)));
    assert!(native_pattern.is_pure(), "raw swing lost native purity");
    assert!(
        native_pattern.as_pure().is_none(),
        "raw swing retained the structural pure fast path"
    );
    assert!(
        !native.active_needs_host(),
        "raw native swing retained JavaScript"
    );
    assert_eq!(
        query_active(&native, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .len(),
        8
    );
    assert_clean(&native, "raw native swing ownership");
    native.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawSwingDispatchHits = 0;
             globalThis.rawSwingQueryHits = 0;
             globalThis.rawSwingLateHits = 0;
             globalThis.rawSwingOwnershipMismatch = 0;
             let kept = { marker: 'raw-swing-owned' };
             let factor = { marker: 'raw-swing-factor' };
             let target = {};
             let method = function (swing, received) {
               globalThis.rawSwingDispatchHits++;
               if (this !== target || swing !== 1 / 3
                   || received !== factor) {
                 globalThis.rawSwingOwnershipMismatch++;
               }
               const retained = kept;
               return new Pattern(state => {
                 globalThis.rawSwingQueryHits++;
                 return pure(retained).query(state);
               }, 1);
             };
             target.swingBy = method;
             globalThis.rawSwingKeptRef = new WeakRef(kept);
             globalThis.rawSwingDroppedRefs = [
               new WeakRef(factor), new WeakRef(target),
               new WeakRef(method)
             ];
             const result = Reflect.apply(
               Pattern.prototype._swing, null, [factor, target]
             );
             target.swingBy = function () {
               globalThis.rawSwingLateHits++;
               return pure('late');
             };
             kept = factor = target = method = null;
             return result;
           })()"#,
    );
    assert_eq!(owned.get_number("rawSwingDispatchHits"), Some(1.0));
    assert_eq!(owned.get_number("rawSwingQueryHits"), Some(0.0));
    assert_eq!(owned.get_number("rawSwingOwnershipMismatch"), Some(0.0));
    assert!(
        owned.active_needs_host(),
        "returned custom Pattern lost its query owner"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawSwingKeptAlive = Number(
                 rawSwingKeptRef.deref()?.marker === 'raw-swing-owned'
               );
               globalThis.rawSwingTransientsPruned = Number(
                 rawSwingDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawSwingKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSwingTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(values.iter().any(|value| value.contains("raw-swing-owned")));
    assert_eq!(owned.get_number("rawSwingQueryHits"), Some(1.0));
    assert_eq!(owned.get_number("rawSwingLateHits"), Some(0.0));
    assert_clean(&owned, "raw swing selected ownership");
    owned.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(0).setSteps(8)._swing(4)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw swing charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw swing charged query-time stepwise work"
    );
    assert_clean(&direct, "raw swing pool-neutral direct path");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._swing(4);
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw swing consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw swing shared-pool neutrality");
    shared.clear_active();

    let over = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._swing(4);
             return gap({}).shrink(0);
           }})"#,
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw swing nested +1 refusal");
    over.clear_active();

    // Canonical own-query replacement, mutable `inside`, patterned/extreme
    // fractions, and general graph/allocation bounds remain explicit
    // residuals; this raw dispatch adds no resource operation of its own.
}

#[test]
fn raw_signal_quartet_keeps_surface_strict_forwarding_dynamic_dispatch_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const hasOwn = Object.prototype.hasOwnProperty;
                   const entries = [
                     ['often', '_often', 0.75],
                     ['rarely', '_rarely', 0.25],
                     ['almostNever', '_almostNever', 0.1],
                     ['almostAlways', '_almostAlways', 0.9],
                   ];
                   const raws = entries.map(([, rawName]) =>
                     Pattern.prototype[rawName]
                   );

                   phase = 'surface';
                   check(new Set(raws).size === entries.length,
                     'raw identities collapsed');
                   for (const [name, rawName] of entries) {
                     const raw = Pattern.prototype[rawName];
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, rawName
                     );
                     check(typeof raw === 'function'
                       && raw !== Pattern.prototype[name],
                       `${rawName} identity`);
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${rawName} descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${rawName} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && hasOwn.call(raw, 'prototype'),
                       `${rawName} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${rawName} constructibility`);
                     check(hasOwn.call(Pattern.prototype, rawName)
                       && !hasOwn.call(globalThis, rawName)
                       && !hasOwn.call(rustelScope, rawName),
                       `${rawName} publication`);
                   }
                   const projected = [
                     'often', '_often', 'rarely', '_rarely',
                     'almostNever', '_almostNever',
                     'almostAlways', '_almostAlways',
                     'never', '_never', 'always', '_always',
                   ];
                   const expectedOrder = projected.join('|');
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === expectedOrder, 'filtered own-key order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === expectedOrder, 'filtered enumerable order');

                   phase = 'strict-forwarding';
                   const callback = function callback() {};
                   let zeroReceiverHits = 0;
                   const zeroReceiver = new Proxy(callback, {
                     get() { zeroReceiverHits++; throw new Error('read receiver'); },
                   });
                   let zeroError;
                   try { apply(raws[0], zeroReceiver, []); }
                   catch (error) { zeroError = error; }
                   check(zeroError instanceof TypeError
                     && zeroReceiverHits === 0, 'zero-argument shift');
                   for (const receiver of [null, undefined]) {
                     let error;
                     try { apply(raws[0], receiver, [callback]); }
                     catch (caught) { error = caught; }
                     check(error instanceof TypeError,
                       'nullish strict receiver did not throw');
                   }
                   for (const [receiver, prototype] of [
                     [7, Number.prototype],
                     ['x', String.prototype],
                     [true, Boolean.prototype],
                     [1n, BigInt.prototype],
                     [Symbol('receiver'), Symbol.prototype],
                   ]) {
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'sometimesBy'
                     );
                     Object.defineProperty(prototype, 'sometimesBy', {
                       configurable: true,
                       value: function (threshold, received) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         check(threshold === 0.75 && received === callback,
                           'primitive arguments changed');
                         return receiver;
                       },
                     });
                     try {
                       check(Object.is(
                         apply(raws[0], receiver, [callback]), receiver
                       ), 'primitive terminal changed');
                     } finally {
                       if (prior === undefined) {
                         delete prototype.sometimesBy;
                       } else {
                         Object.defineProperty(
                           prototype, 'sometimesBy', prior
                         );
                       }
                     }
                   }

                   phase = 'dynamic-dispatch';
                   for (const [index, [name, rawName, threshold]]
                        of entries.entries()) {
                     const raw = raws[index];
                     const terminal = { rawName };
                     const target = {};
                     let getterHits = 0;
                     let callHits = 0;
                     const method = new Proxy(function () {}, {
                       apply(_function, thisArg, args) {
                         callHits++;
                         check(thisArg === target,
                           `${rawName} method receiver`);
                         check(args.length === 2
                           && Object.is(args[0], threshold)
                           && args[1] === callback,
                           `${rawName} arguments`);
                         return terminal;
                       },
                     });
                     Object.defineProperty(target, 'sometimesBy', {
                       configurable: true,
                       get() { getterHits++; return method; },
                     });
                     check(apply(raw, target, [callback]) === terminal,
                       `${rawName} one-argument target`);
                     const outer = new Proxy({}, {
                       get() { throw new Error('read outer receiver'); },
                       ownKeys() { throw new Error('listed outer receiver'); },
                     });
                     const ignored = new Proxy({}, {
                       get() { throw new Error('read ignored extra'); },
                     });
                     check(apply(raw, outer, [callback, target, ignored, 9])
                       === terminal, `${rawName} explicit target`);
                     check(getterHits === 2 && callHits === 2,
                       `${rawName} getter/call count`);

                     const getterSentinel = { rawName, phase: 'getter' };
                     const cutoffTarget = {};
                     Object.defineProperty(cutoffTarget, 'sometimesBy', {
                       get() { throw getterSentinel; },
                     });
                     let getterCaught;
                     try { apply(raw, null, [callback, cutoffTarget]); }
                     catch (error) { getterCaught = error; }
                     check(getterCaught === getterSentinel,
                       `${rawName} getter throw identity`);
                     const callSentinel = { rawName, phase: 'call' };
                     let callCaught;
                     try {
                       apply(raw, null, [callback, {
                         sometimesBy() { throw callSentinel; },
                       }]);
                     } catch (error) { callCaught = error; }
                     check(callCaught === callSentinel,
                       `${rawName} call throw identity`);

                     const originalRaw = Pattern.prototype[rawName];
                     const originalPublic = Pattern.prototype[name];
                     const originalGlobal = globalThis[name];
                     const scopeOwned = hasOwn.call(rustelScope, name);
                     const originalScope = rustelScope[name];
                     Pattern.prototype[rawName] = () => {
                       throw new Error('poisoned raw');
                     };
                     Pattern.prototype[name] = () => {
                       throw new Error('poisoned public');
                     };
                     globalThis[name] = () => {
                       throw new Error('poisoned global');
                     };
                     rustelScope[name] = () => {
                       throw new Error('poisoned scope');
                     };
                     check(apply(raw, null, [callback, target]) === terminal,
                       `${rawName} saved dispatch followed poison`);
                     Pattern.prototype[rawName] = originalRaw;
                     Pattern.prototype[name] = originalPublic;
                     globalThis[name] = originalGlobal;
                     if (scopeOwned) rustelScope[name] = originalScope;
                     else delete rustelScope[name];

                     let oneArgumentConstruction;
                     try { new raw(callback); }
                     catch (error) { oneArgumentConstruction = error; }
                     check(oneArgumentConstruction instanceof TypeError,
                       `${rawName} one-argument construction`);
                     const objectTerminal = {};
                     const constructorTarget = {
                       object: true,
                       sometimesBy(receivedThreshold, receivedCallback) {
                         check(this === constructorTarget
                           && Object.is(receivedThreshold, threshold)
                           && receivedCallback === callback,
                           `${rawName} constructor forwarding`);
                         return this.object ? objectTerminal : 7;
                       },
                     };
                     check(new raw(callback, constructorTarget)
                       === objectTerminal,
                       `${rawName} object constructor terminal`);
                     constructorTarget.object = false;
                     const primitiveConstruction = new raw(
                       callback, constructorTarget
                     );
                     check(primitiveConstruction instanceof raw,
                       `${rawName} primitive constructor fallback`);
                   }

                   phase = 'parser-cutoffs';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`unexpected parser call: ${value}`);
                   });
                   check(pure('ordinary')._often(callback) instanceof Pattern
                     && parserCalls === 0,
                     'ordinary callable entered parser');
                   const customStringTarget = {
                     sometimesBy(threshold, received) {
                       check(threshold === 0.75 && received === 'raw-string',
                         'custom string identity changed');
                       return 3;
                     },
                   };
                   check(apply(raws[0], null, [
                     'raw-string', customStringTarget
                   ]) === 3 && parserCalls === 0,
                     'custom dispatch entered parser');
                   const parserCutoff = {};
                   Object.defineProperty(parserCutoff, 'sometimesBy', {
                     get() { throw new Error('parser cutoff'); },
                   });
                   try {
                     apply(raws[0], null, ['raw-string', parserCutoff]);
                   } catch (error) {
                     check(error.message === 'parser cutoff',
                       'parser cutoff error changed');
                   }
                   check(parserCalls === 0,
                     'getter cutoff entered parser');
                   setStringParser(undefined);

                   globalThis.rawSignalSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawSignalSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSignalSurfaceError"), None);
    assert_eq!(runtime.get_number("rawSignalSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw signal surface and dispatch");
}

#[test]
fn raw_signal_quartet_keeps_rng_lazy_callbacks_steps_tags_and_throw_cutoff() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const entries = [
                     ['_often', 55],
                     ['_rarely', 18],
                     ['_almostNever', 7],
                     ['_almostAlways', 63],
                   ];
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   for (const [rawName, transformed] of entries) {
                     phase = `${rawName}-rng`;
                     let calls = 0;
                     const result = pure(0).fast(64)[rawName](pat => {
                       calls++;
                       return pat.add(1);
                     });
                     check(calls === 0, `${rawName} callback was eager`);
                     const first = result.query(state(0, 1));
                     check(calls === 1 && first.length === 64
                       && first.filter(hap => hap.value === 1).length
                         === transformed,
                       `${rawName} first-cycle discriminator`);
                     const second = result.query(state(1, 2));
                     check(calls === 2 && second.length === 64,
                       `${rawName} crossing callback count`);

                     const source = pure(rawName).setSteps(11);
                     source.__pure_loc = { start: 10, end: 11 };
                     check(Object.prototype.hasOwnProperty.call(
                       source, '__pure'
                     ) && Object.prototype.hasOwnProperty.call(
                       source, '__pure_loc'
                     ), `${rawName} source tag precondition`);
                     const identity = source[rawName](pat => pat);
                     check(identity !== source
                       && identity.query !== source.query
                       && identity._steps === undefined
                       && !Object.prototype.hasOwnProperty.call(
                         identity, '__pure'
                       )
                       && !Object.prototype.hasOwnProperty.call(
                         identity, '__pure_loc'
                       ), `${rawName} steps/tag loss`);
                   }
                   globalThis.rawSignalSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawSignalSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSignalSemanticError"), None);
    assert_eq!(runtime.get_number("rawSignalSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw signal scalar semantics");

    let throwing = semantic_runtime(
        r#"(() => {
             globalThis.rawSignalThrowCalls = 0;
             return pure('throw')._often(() => {
               globalThis.rawSignalThrowCalls++;
               throw new Error('raw signal callback sentinel');
             });
           })()"#,
    );
    let thrown = query_active(&throwing, Fraction::ZERO, Fraction::int(2)).unwrap();
    assert!(
        thrown.is_empty(),
        "query-time raw signal throw was not converted to silence"
    );
    assert_eq!(throwing.get_number("rawSignalThrowCalls"), Some(1.0));
    assert_clean(&throwing, "raw signal first-throw cutoff");
    throwing.clear_active();

    // Explicitly pin, rather than hide, the remaining public-helper gap:
    // pinned Node constructs lazily for `_often(17)` and queryArc yields zero
    // after `func is not a function`; native `sometimesBy` currently maps a
    // non-callable FunctionRef to the identity and therefore emits the source.
    let invalid = semantic_runtime("pure('invalid').fast(4)._often(17)");
    let invalid_haps = query_active(&invalid, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(invalid_haps.len(), 4, "invalid-callback residual drifted");
    assert!(
        invalid_haps.iter().all(|hap| hap.value.show() == "invalid"),
        "invalid-callback residual stopped selecting the source"
    );
    assert_clean(&invalid, "raw signal invalid-callback residual");
    invalid.clear_active();
}

#[test]
fn raw_signal_quartet_preserves_owners_prunes_transients_and_keeps_nested_accounting() {
    let tagged = semantic_runtime("pure('tagged')._often(rev)");
    let tagged_pattern = tagged.active_pattern().expect("tagged raw signal");
    assert!(
        tagged_pattern.is_pure(),
        "tagged raw signal lost native purity"
    );
    assert!(
        !tagged.active_needs_host(),
        "tagged raw signal retained a JavaScript query owner"
    );
    let detached = tagged_pattern.clone();
    tagged.clear_active();
    drop(tagged);
    let detached_values = detached
        .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(detached_values, ["tagged"]);

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawSignalCallbackCalls = 0;
             let callbackMarker = { marker: 'raw-signal-callback' };
             let valueMarker = { marker: 'raw-signal-value' };
             let outer = { marker: 'raw-signal-outer' };
             let extra = { marker: 'raw-signal-extra' };
             let callback = ((ownedCallbackMarker, ownedValueMarker) =>
               pat => {
                 globalThis.rawSignalCallbackCalls++;
                 void ownedCallbackMarker;
                 return pat.withValue(() => ownedValueMarker);
               }
             )(callbackMarker, valueMarker);
             globalThis.rawSignalCallbackMarkerRef = new WeakRef(
               callbackMarker
             );
             globalThis.rawSignalValueMarkerRef = new WeakRef(valueMarker);
             globalThis.rawSignalCallbackRef = new WeakRef(callback);
             globalThis.rawSignalTransientRefs = [
               new WeakRef(outer), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._often, outer,
               [callback, pure('source').fast(8), extra]
             );
             callbackMarker = valueMarker = outer = extra = callback = null;
             return result;
           })()"#,
    );
    assert!(owned.active_needs_host(), "JS callback route lost its host");
    assert_eq!(owned.get_number("rawSignalCallbackCalls"), Some(0.0));
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawSignalCallbackOwnerAlive = Number(
                 rawSignalCallbackMarkerRef.deref()?.marker
                   === 'raw-signal-callback'
                 && typeof rawSignalCallbackRef.deref() === 'function'
               );
               globalThis.rawSignalValueOwnerAlive = Number(
                 rawSignalValueMarkerRef.deref()?.marker
                   === 'raw-signal-value'
               );
               globalThis.rawSignalTransientsPruned = Number(
                 rawSignalTransientRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawSignalCallbackOwnerAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSignalValueOwnerAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSignalTransientsPruned"), Some(1.0));
    let first_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(owned.get_number("rawSignalCallbackCalls"), Some(1.0));
    assert!(
        first_values
            .iter()
            .any(|value| value.contains("raw-signal-value"))
    );
    query_active(&owned, Fraction::ONE, Fraction::int(2)).unwrap();
    assert_eq!(owned.get_number("rawSignalCallbackCalls"), Some(2.0));
    assert_clean(&owned, "raw signal callback ownership");
    owned.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-signal-terminal' };
             let outer = { marker: 'raw-signal-terminal-outer' };
             let target = {};
             let callback = function callback() {};
             let extra = { marker: 'raw-signal-terminal-extra' };
             let method = function (threshold, received) {
               if (this !== target || threshold !== 0.25
                   || received !== callback) {
                 throw new Error('terminal dispatch changed');
               }
               return pure(kept);
             };
             target.sometimesBy = method;
             globalThis.rawSignalTerminalKeptRef = new WeakRef(kept);
             globalThis.rawSignalTerminalDroppedRefs = [
               new WeakRef(outer), new WeakRef(target),
               new WeakRef(callback), new WeakRef(extra),
               new WeakRef(method),
             ];
             const result = Reflect.apply(
               Pattern.prototype._rarely, outer,
               [callback, target, extra]
             );
             kept = outer = target = callback = extra = method = null;
             return result;
           })()"#,
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawSignalTerminalKeptAlive = Number(
                 rawSignalTerminalKeptRef.deref()?.marker
                   === 'raw-signal-terminal'
               );
               globalThis.rawSignalTerminalTransientsPruned = Number(
                 rawSignalTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(terminal.get_number("rawSignalTerminalKeptAlive"), Some(1.0));
    assert_eq!(
        terminal.get_number("rawSignalTerminalTransientsPruned"),
        Some(1.0)
    );
    assert!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show()
            .contains("raw-signal-terminal")
    );
    assert_clean(&terminal, "raw signal custom-terminal ownership");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure('neutral')._almostAlways(rev)");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw signal quartet charged stepwise work"
    );
    assert_clean(&direct, "raw signal direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure('nested')._often(() => gap({limit}).shrink(0))"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested shrink accounting was not preserved"
    );
    assert_clean(&exact, "raw signal nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure('nested')._often(() => gap({}).shrink(0))",
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_clean(&over, "raw signal nested refusal attribution");
    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .unwrap();
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "raw signal nested refusal recovery");
    over.clear_active();

    // Parser-produced/function-Pattern routing, invalid/non-callable and
    // callable exotica, native own/custom-query replacement, aggregate opaque
    // lineage, exact V8 stacks/toString/absolute order, arbitrary callback
    // graphs/allocation/resources and asynchronous terminals remain residual.
}

#[test]
fn raw_apply_keeps_surface_strict_forwarding_and_terminal_semantics() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._apply;

                   phase = 'surface';
                   check(typeof raw === 'function'
                     && raw !== Pattern.prototype.apply,
                     'raw identity');
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_apply'
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'prototype descriptor');
                   check(Reflect.ownKeys(raw).join('|')
                     === 'length|name|prototype', 'function own keys');
                   check(raw.name === '' && raw.length === 0
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'function reflection');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'constructibility');
                   check(!('_apply' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_apply'
                     ), 'raw publication');
                   const ownNames = Reflect.ownKeys(Pattern.prototype);
                   check(ownNames.indexOf('_apply')
                     === ownNames.indexOf('apply') + 1,
                     'own-key interleave');
                   const enumerableNames = Object.keys(Pattern.prototype);
                   check(enumerableNames.indexOf('_apply')
                     === enumerableNames.indexOf('apply') + 1,
                     'enumerable interleave');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined, 7, 'x']) {
                     let trapHits = 0;
                     const callback = new Proxy(function () {
                       throw new Error('proxy target body reached');
                     }, {
                       apply(_target, thisArg, args) {
                         trapHits++;
                         check(thisArg === undefined,
                           'callback thisArg was not undefined');
                         check(args.length === 1
                           && Object.is(args[0], receiver),
                           'wrapper receiver was boxed or replaced');
                         return args[0];
                       },
                     });
                     check(Object.is(apply(raw, receiver, [callback]), receiver)
                       && trapHits === 1,
                       'strict receiver forwarding changed');
                   }

                   phase = 'argument-forwarding';
                   let forwardHits = 0;
                   const zeroTerminal = {};
                   function zeroCallback(received) {
                     'use strict';
                     forwardHits++;
                     check(this === undefined && received === undefined,
                       'zero-argument forwarding');
                     return zeroTerminal;
                   }
                   check(apply(raw, zeroCallback, []) === zeroTerminal,
                     'zero-argument terminal');

                   const wrapperTarget = {};
                   const oneTerminal = {};
                   function oneCallback(received) {
                     'use strict';
                     forwardHits++;
                     check(this === undefined && received === wrapperTarget,
                       'one-argument forwarding');
                     return oneTerminal;
                   }
                   check(apply(raw, wrapperTarget, [oneCallback])
                     === oneTerminal, 'one-argument terminal');

                   const explicitTarget = {};
                   const wrapperDecoy = {};
                   const explicitTerminal = {};
                   function explicitCallback(received) {
                     'use strict';
                     forwardHits++;
                     check(this === undefined && received === explicitTarget,
                       'explicit target forwarding');
                     return explicitTerminal;
                   }
                   check(apply(raw, wrapperDecoy, [
                     explicitCallback, explicitTarget
                   ]) === explicitTerminal, 'two-argument terminal');
                   check(apply(raw, wrapperDecoy, [
                     explicitCallback, explicitTarget, 'ignored', {}
                   ]) === explicitTerminal, 'extra-argument terminal');
                   check(forwardHits === 4,
                     'callback was skipped or invoked repeatedly');

                   phase = 'throw-identity';
                   const sentinel = { marker: 'raw-apply-throw' };
                   let caught;
                   try {
                     apply(raw, wrapperDecoy, [() => { throw sentinel; }]);
                   } catch (error) {
                     caught = error;
                   }
                   check(caught === sentinel, 'callback throw was translated');

                   phase = 'arbitrary-terminals';
                   const symbol = Symbol('raw-apply-terminal');
                   const object = { marker: 'object-terminal' };
                   const callable = function terminal() {};
                   const promise = Promise.resolve('promise-terminal');
                   const terminals = [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ];
                   for (const terminal of terminals) {
                     const result = apply(raw, wrapperDecoy, [
                       () => terminal, explicitTarget
                     ]);
                     check(Object.is(result, terminal),
                       'arbitrary terminal was transformed');
                   }

                   phase = 'constructors';
                   const constructedObjectTerminal = {};
                   let objectPat;
                   const objectConstruction = Reflect.construct(raw, [
                     function (pat) {
                       'use strict';
                       check(this === undefined, 'constructor callback this');
                       objectPat = pat;
                       return constructedObjectTerminal;
                     },
                   ]);
                   check(objectConstruction === constructedObjectTerminal
                     && objectPat instanceof raw,
                     'object constructor return did not replace instance');
                   let primitivePat;
                   const primitiveConstruction = Reflect.construct(raw, [
                     function (pat) {
                       'use strict';
                       primitivePat = pat;
                       return 7;
                     },
                   ]);
                   check(primitiveConstruction === primitivePat
                     && primitiveConstruction instanceof raw
                     && Reflect.ownKeys(primitiveConstruction).length === 0,
                     'primitive constructor return did not fall back');
                   check(Object.is(apply(raw, null, [() => -0]), -0),
                     'ordinary call changed primitive return');

                   phase = 'poisoning';
                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned apply surface reached');
                   };
                   const poisonedTarget = {};
                   Object.defineProperty(poisonedTarget, 'apply', {
                     configurable: true,
                     get() {
                       poisonHits++;
                       throw new Error('target apply getter reached');
                     },
                   });
                   Pattern.prototype.apply = poison;
                   Pattern.prototype._apply = poison;
                   globalThis.apply = poison;
                   rustelScope.apply = poison;
                   const poisonTerminal = {};
                   check(apply(raw, null, [
                     received => {
                       check(received === poisonedTarget,
                         'poison target identity');
                       return poisonTerminal;
                     },
                     poisonedTarget,
                   ]) === poisonTerminal && poisonHits === 0,
                   'saved raw routed through poisoned surface');

                   globalThis.rawApplySurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawApplySurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawApplySurfaceError"), None);
    assert_eq!(runtime.get_number("rawApplySurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw apply surface and forwarding");
}

#[test]
fn raw_apply_is_eager_host_free_and_preserves_only_returned_ownership() {
    let native = semantic_runtime(
        r#"(() => {
             globalThis.rawApplyNativeHits = 0;
             globalThis.rawApplyNativeMismatch = 0;
             let callback = function (pat) {
               'use strict';
               rawApplyNativeHits++;
               if (this !== undefined || pat !== source) {
                 rawApplyNativeMismatch++;
               }
               return pat;
             };
             globalThis.rawApplyNativeCallbackRef = new WeakRef(callback);
             const source = pure('raw-apply-native').setSteps(3);
             const location = { start: 4, end: 9 };
             source.__pure_loc = location;
             const query = source.query;
             const result = source._apply(callback);
             globalThis.rawApplyNativeIdentity = Number(
               result === source
               && result.query === query
               && result.__pure_loc === location
             );
             callback = null;
             return result;
           })()"#,
    );
    assert_eq!(native.get_number("rawApplyNativeHits"), Some(1.0));
    assert_eq!(native.get_number("rawApplyNativeMismatch"), Some(0.0));
    assert_eq!(native.get_number("rawApplyNativeIdentity"), Some(1.0));
    assert!(
        !native.active_needs_host(),
        "an eager native raw-apply return retained its callback"
    );
    assert_eq!(
        native
            .active_pattern()
            .expect("raw apply native pattern")
            .steps,
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        native.run_gc();
    }
    native
        .eval(
            r#"globalThis.rawApplyNativeCallbackPruned = Number(
                 rawApplyNativeCallbackRef.deref() === undefined
               )"#,
        )
        .unwrap();
    assert_eq!(native.get_number("rawApplyNativeCallbackPruned"), Some(1.0));
    assert_eq!(
        query_active(&native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-apply-native"
    );
    assert_eq!(
        native.get_number("rawApplyNativeHits"),
        Some(1.0),
        "raw apply callback was retained until query"
    );
    assert_clean(&native, "raw apply eager native return");
    native.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawApplyOwnedHits = 0;
             globalThis.rawApplyOwnedQueryHits = 0;
             globalThis.rawApplyOwnedMismatch = 0;
             const makePattern = kept => new Pattern(state => {
               rawApplyOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-apply-owned' };
             let outer = { marker: 'outer' };
             let target = { marker: 'target' };
             let transient = { marker: 'captured-transient' };
             let extra = { marker: 'ignored-extra' };
             let callback = function (received) {
               'use strict';
               rawApplyOwnedHits++;
               if (this !== undefined || received !== target
                   || transient.marker !== 'captured-transient') {
                 rawApplyOwnedMismatch++;
               }
               return makePattern(kept);
             };
             globalThis.rawApplyKeptRef = new WeakRef(kept);
             globalThis.rawApplyDroppedRefs = [
               new WeakRef(outer), new WeakRef(target),
               new WeakRef(transient), new WeakRef(extra),
               new WeakRef(callback)
             ];
             const result = Reflect.apply(Pattern.prototype._apply, outer, [
               callback, target, extra
             ]);
             callback = outer = target = transient = extra = kept = null;
             return result;
           })()"#,
    );
    assert_eq!(owned.get_number("rawApplyOwnedHits"), Some(1.0));
    assert_eq!(owned.get_number("rawApplyOwnedQueryHits"), Some(0.0));
    assert_eq!(owned.get_number("rawApplyOwnedMismatch"), Some(0.0));
    assert!(
        owned.active_needs_host(),
        "a returned JavaScript Pattern/value lost its ownership sidecar"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawApplyKeptAlive = Number(
                 rawApplyKeptRef.deref()?.marker === 'raw-apply-owned'
               );
               globalThis.rawApplyTransientsPruned = Number(
                 rawApplyDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawApplyKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawApplyTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values.iter().any(|value| value.contains("raw-apply-owned")),
        "returned JavaScript value was not queryable: {values:?}"
    );
    assert_eq!(owned.get_number("rawApplyOwnedHits"), Some(1.0));
    assert_eq!(owned.get_number("rawApplyOwnedQueryHits"), Some(1.0));
    assert_clean(&owned, "raw apply returned ownership");
    owned.clear_active();
}

#[test]
fn raw_apply_is_pool_neutral_while_nested_work_keeps_its_accounting() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure('raw-apply')._apply(pat => pat)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw apply charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw apply charged query-time stepwise work"
    );
    assert_clean(&direct, "raw apply pool-neutral direct return");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => pure('x')._apply(() => gap({limit}).shrink(0)))"
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested raw apply callback ran before query"
    );
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw apply hid or duplicated nested shrink accounting"
    );
    assert_clean(&exact, "raw apply nested exact accounting");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => pure('x')._apply(() => gap({}).shrink(0)))",
        limit + 1
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw apply nested +1 refusal");
    over.clear_active();

    // Callback-created arbitrary graphs remain governed by their own
    // operation-specific limits; this bounded raw slot adds no policy.
}

#[test]
fn raw_when_keeps_surface_strict_truthiness_and_terminal_semantics() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._when;

                   phase = 'surface';
                   check(typeof raw === 'function'
                     && raw !== Pattern.prototype.when,
                     'raw identity');
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_when'
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'prototype descriptor');
                   check(Reflect.ownKeys(raw).join('|')
                     === 'length|name|prototype', 'function own keys');
                   check(raw.name === '' && raw.length === 0
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'function reflection');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'constructibility');
                   check(!('_when' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_when'
                     ), 'raw publication');
                   const ownNames = Reflect.ownKeys(Pattern.prototype);
                   check(ownNames.indexOf('_when')
                     === ownNames.indexOf('when') + 1,
                     'own-key interleave');
                   const enumerableNames = Object.keys(Pattern.prototype);
                   check(enumerableNames.indexOf('_when')
                     === enumerableNames.indexOf('when') + 1,
                     'enumerable interleave');

                   phase = 'strict-receivers';
                   for (const receiver of [
                     null, undefined, 7, 'x', false, 9n, Symbol('receiver')
                   ]) {
                     let trapHits = 0;
                     const callback = new Proxy(function () {
                       throw new Error('proxy target body reached');
                     }, {
                       apply(_target, thisArg, args) {
                         trapHits++;
                         check(thisArg === undefined,
                           'callback thisArg was not undefined');
                         check(args.length === 1
                           && Object.is(args[0], receiver),
                           'wrapper receiver was boxed or replaced');
                         return args[0];
                       },
                     });
                     check(Object.is(
                       apply(raw, receiver, [true, callback]), receiver
                     ) && trapHits === 1,
                     'strict receiver forwarding changed');
                   }

                   phase = 'argument-forwarding';
                   check(apply(raw, null, []) === undefined,
                     'zero-argument false receiver');
                   let zeroTruthyError;
                   try { apply(raw, {}, []); }
                   catch (error) { zeroTruthyError = error.name; }
                   check(zeroTruthyError === 'TypeError',
                     'zero-argument truthy receiver did not call undefined');

                   let oneFalseHits = 0;
                   function oneFalseReceiver() { oneFalseHits++; }
                   check(apply(raw, oneFalseReceiver, [false]) === undefined
                     && oneFalseHits === 0,
                     'one-argument false branch touched receiver callback');
                   let oneTrueHits = 0;
                   function oneTrueReceiver(received) {
                     'use strict';
                     oneTrueHits++;
                     check(this === undefined && received === undefined,
                       'one-argument true forwarding');
                     return received;
                   }
                   check(apply(raw, oneTrueReceiver, [true]) === undefined
                     && oneTrueHits === 1,
                     'one-argument true branch count');

                   const receiver = { marker: 'receiver' };
                   const target = { marker: 'target' };
                   const terminal = { marker: 'terminal' };
                   let normalHits = 0;
                   function normalCallback(received) {
                     'use strict';
                     normalHits++;
                     check(this === undefined, 'normal callback this');
                     check(received === (normalHits === 1 ? receiver : target),
                       'normal callback target');
                     return terminal;
                   }
                   check(apply(raw, receiver, [true, normalCallback])
                     === terminal, 'two-argument terminal');
                   check(apply(raw, receiver, [
                     true, normalCallback, target
                   ]) === terminal, 'three-argument retarget');
                   check(apply(raw, receiver, [
                     true, normalCallback, target, Symbol('ignored'),
                     { get poison() { throw new Error('ignored extra'); } }
                   ]) === terminal, 'extra-argument terminal');
                   check(normalHits === 3,
                     'two/three/extra callback count');

                   phase = 'truthiness';
                   let falseHits = 0;
                   const falseCallback = new Proxy(function () {}, {
                     apply() {
                       falseHits++;
                       throw new Error('false callback reached');
                     },
                   });
                   for (const on of [
                     undefined, null, false, 0, -0, 0n, NaN, ''
                   ]) {
                     check(apply(raw, receiver, [on, falseCallback])
                       === receiver, 'falsey value did not return receiver');
                   }
                   const revocable = Proxy.revocable(function () {}, {});
                   revocable.revoke();
                   check(apply(raw, receiver, [false, revocable.proxy])
                     === receiver && falseHits === 0,
                     'false branch validated or called func');
                   let nonCallableReads = 0;
                   const hostileNonCallable = new Proxy({}, {
                     get() {
                       nonCallableReads++;
                       throw new Error('false non-callable was inspected');
                     },
                   });
                   for (const func of [
                     undefined, null, 7, Symbol('not-callable'),
                     hostileNonCallable
                   ]) {
                     check(apply(raw, receiver, [false, func, target])
                       === target,
                       'false branch validated a non-callable func');
                   }
                   check(nonCallableReads === 0,
                     'false branch inspected non-callable func');

                   let coercionReads = 0;
                   const objectCondition = new Proxy({}, {
                     get() {
                       coercionReads++;
                       throw new Error('condition coercion reached');
                     },
                   });
                   let truthyHits = 0;
                   const truthyValues = [
                     true, 1, -1, 1n, '0', Symbol('on'), {}, [],
                     new Boolean(false), objectCondition
                   ];
                   for (const on of truthyValues) {
                     const result = apply(raw, receiver, [on, value => {
                       truthyHits++;
                       check(value === receiver,
                         'truthy callback target changed');
                       return value;
                     }]);
                     check(result === receiver,
                       'truthy value did not invoke callback');
                   }
                   check(truthyHits === truthyValues.length
                     && coercionReads === 0,
                     'ToBoolean coerced an object or changed count');

                   phase = 'guards-and-parser';
                   let targetReads = 0;
                   const guardedTarget = new Proxy({}, {
                     get() {
                       targetReads++;
                       throw new Error('raw when read target');
                     },
                   });
                   check(apply(raw, receiver, [
                     false, falseCallback, guardedTarget
                   ]) === guardedTarget && targetReads === 0,
                   'false branch inspected target');
                   const guardedTerminal = {};
                   Object.defineProperties(guardedTerminal, {
                     query: {
                       get() { throw new Error('raw when read result query'); },
                     },
                     _steps: {
                       get() { throw new Error('raw when read result steps'); },
                       set() { throw new Error('raw when wrote result steps'); },
                     },
                   });
                   check(apply(raw, receiver, [
                     true, () => guardedTerminal, guardedTarget
                   ]) === guardedTerminal && targetReads === 0,
                   'true branch inspected target or terminal');
                   const parserCalls = [];
                   setStringParser(value => {
                     parserCalls.push(value);
                     return pure(value);
                   });
                   check(apply(raw, receiver, [
                     false, falseCallback, 'false text'
                   ]) === 'false text', 'false string target');
                   check(apply(raw, receiver, [
                     true, value => value, 'true text'
                   ]) === 'true text', 'true string target');
                   setStringParser(undefined);
                   check(parserCalls.length === 0,
                     'raw when invoked configured parser');

                   phase = 'terminals-and-throws';
                   const symbol = Symbol('raw-when-terminal');
                   const object = { marker: 'object-terminal' };
                   const callable = function returnedFunction() {};
                   const promise = Promise.resolve('promise-terminal');
                   const terminals = [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ];
                   for (const value of terminals) {
                     check(Object.is(apply(raw, receiver, [
                       true, () => value, target
                     ]), value), 'arbitrary terminal was transformed');
                   }
                   const sentinel = { marker: 'raw-when-throw' };
                   let caught;
                   try {
                     apply(raw, receiver, [
                       true, () => { throw sentinel; }, target
                     ]);
                   } catch (error) {
                     caught = error;
                   }
                   check(caught === sentinel,
                     'callback throw was translated');
                   let nonCallableError;
                   try { apply(raw, receiver, [true, 7, target]); }
                   catch (error) { nonCallableError = error.name; }
                   check(nonCallableError === 'TypeError',
                     'truthy non-callable did not throw TypeError');

                   phase = 'constructors';
                   const objectTerminal = { marker: 'constructed-object' };
                   let objectPat;
                   const objectConstruction = Reflect.construct(raw, [
                     true,
                     function (pat) {
                       'use strict';
                       check(this === undefined,
                         'constructor callback this');
                       objectPat = pat;
                       return objectTerminal;
                     },
                   ]);
                   check(objectConstruction === objectTerminal
                     && objectPat instanceof raw,
                     'object constructor return did not replace instance');
                   let primitivePat;
                   const primitiveConstruction = Reflect.construct(raw, [
                     true,
                     function (pat) {
                       'use strict';
                       primitivePat = pat;
                       return 7;
                     },
                   ]);
                   check(primitiveConstruction === primitivePat
                     && primitiveConstruction instanceof raw,
                     'primitive constructor return did not fall back');
                   let falseConstructorHits = 0;
                   const falseTarget = { marker: 'false-constructor-target' };
                   check(Reflect.construct(raw, [
                     false, () => { falseConstructorHits++; }, falseTarget
                   ]) === falseTarget,
                   'false object target did not replace instance');
                   const falsePrimitive = Reflect.construct(raw, [
                     false, () => { falseConstructorHits++; }, 7
                   ]);
                   check(falsePrimitive instanceof raw
                     && falseConstructorHits === 0,
                     'false primitive constructor fallback');

                   phase = 'poisoning';
                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned when surface reached');
                   };
                   const poisonedTarget = {};
                   Object.defineProperties(poisonedTarget, {
                     when: {
                       configurable: true,
                       get() {
                         poisonHits++;
                         throw new Error('target when getter reached');
                       },
                     },
                     _when: {
                       configurable: true,
                       get() {
                         poisonHits++;
                         throw new Error('target raw when getter reached');
                       },
                     },
                   });
                   const saved = {
                     method: Pattern.prototype.when,
                     raw: Pattern.prototype._when,
                     global: globalThis.when,
                     scope: rustelScope.when,
                   };
                   const poisonTerminal = {};
                   try {
                     Pattern.prototype.when = poison;
                     Pattern.prototype._when = poison;
                     globalThis.when = poison;
                     rustelScope.when = poison;
                     check(apply(raw, null, [
                       true,
                       value => {
                         check(value === poisonedTarget,
                           'poison target identity');
                         return poisonTerminal;
                       },
                       poisonedTarget,
                     ]) === poisonTerminal && poisonHits === 0,
                     'saved raw routed through poisoned surface');
                   } finally {
                     Pattern.prototype.when = saved.method;
                     Pattern.prototype._when = saved.raw;
                     globalThis.when = saved.global;
                     rustelScope.when = saved.scope;
                   }

                   globalThis.rawWhenSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawWhenSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawWhenSurfaceError"), None);
    assert_eq!(runtime.get_number("rawWhenSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw when surface and forwarding");
}

#[test]
fn raw_when_is_eager_host_free_and_preserves_only_returned_ownership() {
    let false_native = semantic_runtime(
        r#"(() => {
             globalThis.rawWhenFalseHits = 0;
             let transient = { marker: 'false-transient' };
             let callback = function () {
               rawWhenFalseHits++;
               if (transient.marker !== 'false-transient') {
                 throw new Error('false transient changed');
               }
               throw new Error('false callback reached');
             };
             globalThis.rawWhenFalseDroppedRefs = [
               new WeakRef(callback), new WeakRef(transient)
             ];
             const source = pure('raw-when-false-native').setSteps(3);
             const location = { start: 4, end: 9 };
             source.__pure_loc = location;
             const query = source.query;
             const steps = source._steps;
             const pureValue = source.__pure;
             const result = source._when(false, callback);
             globalThis.rawWhenFalseIdentity = Number(
               result === source
               && result.query === query
               && result._steps === steps
               && Object.hasOwn(result, '__pure')
               && result.__pure === pureValue
               && result.__pure_loc === location
             );
             callback = transient = null;
             return result;
           })()"#,
    );
    assert_eq!(false_native.get_number("rawWhenFalseHits"), Some(0.0));
    assert_eq!(false_native.get_number("rawWhenFalseIdentity"), Some(1.0));
    assert!(
        !false_native.active_needs_host(),
        "a false raw-when identity retained its skipped callback"
    );
    assert_eq!(
        false_native
            .active_pattern()
            .expect("raw when false native pattern")
            .steps,
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        false_native.run_gc();
    }
    false_native
        .eval(
            r#"globalThis.rawWhenFalseTransientsPruned = Number(
                 rawWhenFalseDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               )"#,
        )
        .unwrap();
    assert_eq!(
        false_native.get_number("rawWhenFalseTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&false_native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-when-false-native"
    );
    assert_eq!(
        false_native.get_number("rawWhenFalseHits"),
        Some(0.0),
        "false raw when callback ran during query"
    );
    assert_clean(&false_native, "raw when false eager native return");
    false_native.clear_active();

    let true_native = semantic_runtime(
        r#"(() => {
             globalThis.rawWhenTrueHits = 0;
             globalThis.rawWhenTrueMismatch = 0;
             let condition = { marker: 'truthy-condition' };
             let transient = { marker: 'true-transient' };
             let callback = function (pat) {
               'use strict';
               rawWhenTrueHits++;
               if (this !== undefined || pat !== source
                   || transient.marker !== 'true-transient') {
                 rawWhenTrueMismatch++;
               }
               return pat;
             };
             globalThis.rawWhenTrueDroppedRefs = [
               new WeakRef(condition), new WeakRef(callback),
               new WeakRef(transient)
             ];
             const source = pure('raw-when-true-native').setSteps(5);
             const location = { start: 11, end: 17 };
             source.__pure_loc = location;
             const query = source.query;
             const steps = source._steps;
             const pureValue = source.__pure;
             const result = source._when(condition, callback);
             globalThis.rawWhenTrueIdentity = Number(
               result === source
               && result.query === query
               && result._steps === steps
               && Object.hasOwn(result, '__pure')
               && result.__pure === pureValue
               && result.__pure_loc === location
             );
             condition = callback = transient = null;
             return result;
           })()"#,
    );
    assert_eq!(true_native.get_number("rawWhenTrueHits"), Some(1.0));
    assert_eq!(true_native.get_number("rawWhenTrueMismatch"), Some(0.0));
    assert_eq!(true_native.get_number("rawWhenTrueIdentity"), Some(1.0));
    assert!(
        !true_native.active_needs_host(),
        "a true native raw-when identity retained its callback"
    );
    assert_eq!(
        true_native
            .active_pattern()
            .expect("raw when true native pattern")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        true_native.run_gc();
    }
    true_native
        .eval(
            r#"globalThis.rawWhenTrueTransientsPruned = Number(
                 rawWhenTrueDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               )"#,
        )
        .unwrap();
    assert_eq!(
        true_native.get_number("rawWhenTrueTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&true_native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-when-true-native"
    );
    assert_eq!(
        true_native.get_number("rawWhenTrueHits"),
        Some(1.0),
        "true raw when callback was reread during query"
    );
    assert_clean(&true_native, "raw when true eager native return");
    true_native.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawWhenOwnedHits = 0;
             globalThis.rawWhenOwnedQueryHits = 0;
             globalThis.rawWhenOwnedMismatch = 0;
             const makePattern = kept => new Pattern(state => {
               rawWhenOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-when-owned' };
             let condition = { marker: 'owned-condition' };
             let outer = { marker: 'outer' };
             let target = { marker: 'target' };
             let transient = { marker: 'captured-transient' };
             let extra = { marker: 'ignored-extra' };
             let callback = function (received) {
               'use strict';
               rawWhenOwnedHits++;
               if (this !== undefined || received !== target
                   || transient.marker !== 'captured-transient') {
                 rawWhenOwnedMismatch++;
               }
               return makePattern(kept);
             };
             globalThis.rawWhenKeptRef = new WeakRef(kept);
             globalThis.rawWhenDroppedRefs = [
               new WeakRef(condition), new WeakRef(outer),
               new WeakRef(target), new WeakRef(transient),
               new WeakRef(extra), new WeakRef(callback)
             ];
             const result = Reflect.apply(Pattern.prototype._when, outer, [
               condition, callback, target, extra
             ]);
             condition = callback = outer = target = transient = extra = kept = null;
             return result;
           })()"#,
    );
    assert_eq!(owned.get_number("rawWhenOwnedHits"), Some(1.0));
    assert_eq!(owned.get_number("rawWhenOwnedQueryHits"), Some(0.0));
    assert_eq!(owned.get_number("rawWhenOwnedMismatch"), Some(0.0));
    assert!(
        owned.active_needs_host(),
        "a returned JavaScript Pattern/value lost its ownership sidecar"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawWhenKeptAlive = Number(
                 rawWhenKeptRef.deref()?.marker === 'raw-when-owned'
               );
               globalThis.rawWhenTransientsPruned = Number(
                 rawWhenDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawWhenKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawWhenTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values.iter().any(|value| value.contains("raw-when-owned")),
        "returned JavaScript value was not queryable: {values:?}"
    );
    assert_eq!(owned.get_number("rawWhenOwnedHits"), Some(1.0));
    assert_eq!(owned.get_number("rawWhenOwnedQueryHits"), Some(1.0));
    assert_clean(&owned, "raw when returned ownership");
    owned.clear_active();
}

#[test]
fn raw_when_is_pool_neutral_and_short_circuits_nested_accounting() {
    rustel_core::reset_stepwise_entries_materialised();
    let false_direct =
        semantic_runtime("pure('raw-when-false')._when(false, () => gap(20000).shrink(0))");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "false raw when charged construction-time stepwise work"
    );
    query_active(&false_direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "false raw when charged query-time stepwise work"
    );
    assert_clean(&false_direct, "raw when pool-neutral false return");
    false_direct.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let true_direct = semantic_runtime("pure('raw-when-true')._when(true, pat => pat)");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&true_direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "true identity raw when charged shared stepwise work"
    );
    assert_clean(&true_direct, "raw when pool-neutral true return");
    true_direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    rustel_core::reset_stepwise_entries_materialised();
    let suppressed = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawWhenSuppressedHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._when(false, () => {{
                 rawWhenSuppressedHits++;
                 return gap({}).shrink(0);
               }})
             );
           }})()"#,
        limit + 1
    ));
    assert_eq!(suppressed.get_number("rawWhenSuppressedHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&suppressed, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(suppressed.get_number("rawWhenSuppressedHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "false branch entered or charged nested +1 work"
    );
    assert_clean(&suppressed, "raw when false nested suppression");
    suppressed.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawWhenExactHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._when(true, () => {{
                 rawWhenExactHits++;
                 return gap({limit}).shrink(0);
               }})
             );
           }})()"#
    ));
    assert_eq!(exact.get_number("rawWhenExactHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(exact.get_number("rawWhenExactHits"), Some(1.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw when hid or duplicated nested shrink accounting"
    );
    assert_clean(&exact, "raw when nested exact accounting");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawWhenOverHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._when(true, () => {{
                 rawWhenOverHits++;
                 return gap({}).shrink(0);
               }})
             );
           }})()"#,
        limit + 1
    ));
    assert_eq!(over.get_number("rawWhenOverHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(over.get_number("rawWhenOverHits"), Some(1.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw when nested +1 refusal");
    over.clear_active();

    // Callback-created arbitrary graphs remain governed by their own
    // operation-specific limits; this bounded raw slot adds no policy.
}

#[test]
fn raw_never_always_keep_surface_order_strict_forwarding_and_terminals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawNever = Pattern.prototype._never;
                   const rawAlways = Pattern.prototype._always;

                   phase = 'surface';
                   check(typeof rawNever === 'function'
                     && typeof rawAlways === 'function'
                     && rawNever !== Pattern.prototype.never
                     && rawAlways !== Pattern.prototype.always
                     && rawNever !== rawAlways
                     && rawAlways !== Pattern.prototype._apply,
                     'raw identities');
                   for (const [name, raw] of [
                     ['_never', rawNever], ['_always', rawAlways]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${name} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(
                         raw, 'prototype'
                       ), `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const quartet = [
                     'never', '_never', 'always', '_always'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => quartet.includes(name)).join('|')
                     === quartet.join('|'), 'own-key quartet order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => quartet.includes(name)).join('|')
                     === quartet.join('|'), 'enumerable quartet order');

                   phase = 'never-forwarding';
                   const ignored = Object.create(null);
                   Object.defineProperties(ignored, {
                     valueOf: { get() {
                       throw new Error('never read ignored valueOf');
                     } },
                     [Symbol.toPrimitive]: { get() {
                       throw new Error('never read ignored primitive hook');
                     } },
                   });
                   const receivers = [
                     null, undefined, 7, 'x', 1n, Symbol('receiver')
                   ];
                   for (const receiver of receivers) {
                     check(apply(rawNever, receiver, []) === undefined,
                       'never zero-argument shift');
                     check(Object.is(
                       apply(rawNever, receiver, [ignored]), receiver
                     ), 'never strict receiver forwarding');
                   }
                   const neverTarget = { marker: 'never-target' };
                   const neverOuter = { marker: 'never-outer' };
                   const neverExtra = { marker: 'never-extra' };
                   check(apply(rawNever, neverOuter, [
                     ignored, neverTarget
                   ]) === neverTarget, 'never explicit target');
                   check(apply(rawNever, neverOuter, [
                     ignored, neverTarget, neverExtra, 'ignored'
                   ]) === neverTarget, 'never ignored extras');
                   let ignoredCallbackHits = 0;
                   const ignoredCallback = new Proxy(function () {
                     ignoredCallbackHits++;
                   }, {
                     apply() {
                       ignoredCallbackHits++;
                       throw new Error('never callback reached');
                     },
                   });
                   check(apply(rawNever, neverOuter, [
                     ignoredCallback, neverTarget
                   ]) === neverTarget && ignoredCallbackHits === 0,
                   'never invoked ignored callback');

                   phase = 'always-forwarding';
                   let proxyHits = 0;
                   for (const receiver of receivers) {
                     const callback = new Proxy(function () {
                       throw new Error('always proxy body reached');
                     }, {
                       apply(_target, thisArg, args) {
                         proxyHits++;
                         check(thisArg === undefined,
                           'always callback thisArg');
                         check(args.length === 1
                           && Object.is(args[0], receiver),
                           'always strict receiver forwarding');
                         return args[0];
                       },
                     });
                     check(Object.is(
                       apply(rawAlways, receiver, [callback]), receiver
                     ), 'always one-argument terminal');
                   }
                   check(proxyHits === receivers.length,
                     'always Proxy apply count');
                   const zeroTerminal = {};
                   let zeroHits = 0;
                   function zeroCallback(received) {
                     'use strict';
                     zeroHits++;
                     check(this === undefined && new.target === undefined
                       && received === undefined,
                       'always zero-argument forwarding');
                     return zeroTerminal;
                   }
                   check(apply(rawAlways, zeroCallback, []) === zeroTerminal
                     && zeroHits === 1, 'always zero-argument terminal');
                   const explicitTarget = { marker: 'always-target' };
                   const outer = { marker: 'always-outer' };
                   const explicitTerminal = {};
                   let explicitHits = 0;
                   function explicitCallback(received) {
                     'use strict';
                     explicitHits++;
                     check(this === undefined && new.target === undefined
                       && received === explicitTarget,
                       'always explicit target forwarding');
                     return explicitTerminal;
                   }
                   check(apply(rawAlways, outer, [
                     explicitCallback, explicitTarget
                   ]) === explicitTerminal, 'always two arguments');
                   check(apply(rawAlways, outer, [
                     explicitCallback, explicitTarget, {}, 'ignored'
                   ]) === explicitTerminal, 'always ignored extras');
                   check(explicitHits === 2,
                     'always callback was skipped or repeated');

                   phase = 'non-touch-and-terminals';
                   let touchHits = 0;
                   const opaqueTarget = {};
                   for (const name of [
                     'query', '_steps', '__pure', '__pure_loc'
                   ]) {
                     Object.defineProperty(opaqueTarget, name, {
                       configurable: true,
                       get() {
                         touchHits++;
                         throw new Error(`raw read target ${name}`);
                       },
                     });
                   }
                   const parserCalls = [];
                   setStringParser(value => {
                     parserCalls.push(value);
                     return pure(value);
                   });
                   check(apply(rawNever, null, [ignored, opaqueTarget])
                     === opaqueTarget, 'never opaque target identity');
                   check(apply(rawAlways, null, [
                     value => value, opaqueTarget
                   ]) === opaqueTarget, 'always opaque target identity');
                   check(apply(rawNever, null, [ignored, 'never text'])
                     === 'never text', 'never raw string target');
                   check(apply(rawAlways, null, [
                     value => value, 'always text'
                   ]) === 'always text', 'always raw string target');
                   setStringParser(undefined);
                   check(touchHits === 0 && parserCalls.length === 0,
                     'raw pair touched query/steps/parser surfaces');

                   const symbol = Symbol('always-terminal');
                   const object = { marker: 'object-terminal' };
                   const callable = function terminal() {};
                   const promise = Promise.resolve('promise-terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(rawAlways, outer, [
                       () => terminal, explicitTarget
                     ]), terminal), 'always transformed terminal');
                   }
                   const sentinel = { marker: 'always-throw' };
                   let caught;
                   try {
                     apply(rawAlways, outer, [
                       () => { throw sentinel; }, explicitTarget
                     ]);
                   } catch (error) {
                     caught = error;
                   }
                   check(caught === sentinel, 'always translated throw');
                   let nonCallable;
                   try { apply(rawAlways, null, [7, explicitTarget]); }
                   catch (error) { nonCallable = error.name; }
                   check(nonCallable === 'TypeError',
                     'always non-callable did not throw TypeError');

                   phase = 'constructors';
                   const neverZero = Reflect.construct(rawNever, []);
                   const neverOne = Reflect.construct(rawNever, [ignored]);
                   check(neverZero instanceof rawNever
                     && neverOne instanceof rawNever,
                     'never zero/one constructor fallback');
                   const neverObject = { marker: 'never-object' };
                   check(Reflect.construct(rawNever, [
                     ignored, neverObject
                   ]) === neverObject, 'never object constructor return');
                   const neverPrimitive = Reflect.construct(rawNever, [
                     ignored, 7
                   ]);
                   check(neverPrimitive instanceof rawNever,
                     'never primitive constructor fallback');
                   let alwaysZero = 'no-error';
                   try { Reflect.construct(rawAlways, []); }
                   catch (error) { alwaysZero = error.name; }
                   check(alwaysZero === 'TypeError',
                     'always zero constructor did not call instance');
                   const alwaysObject = { marker: 'always-object' };
                   let constructedPat;
                   const alwaysObjectResult = Reflect.construct(rawAlways, [
                     function (pat) {
                       'use strict';
                       check(this === undefined && new.target === undefined,
                         'always constructor callback invocation');
                       constructedPat = pat;
                       return alwaysObject;
                     },
                   ]);
                   check(alwaysObjectResult === alwaysObject
                     && constructedPat instanceof rawAlways,
                     'always object constructor return');
                   let primitivePat;
                   const alwaysPrimitive = Reflect.construct(rawAlways, [
                     function (pat) {
                       primitivePat = pat;
                       return 7;
                     },
                   ]);
                   check(alwaysPrimitive === primitivePat
                     && alwaysPrimitive instanceof rawAlways,
                     'always primitive constructor fallback');
                   const redirectedObject = {
                     marker: 'always-redirected-object'
                   };
                   let redirectedPat;
                   check(Reflect.construct(rawAlways, [
                     function (pat) {
                       redirectedPat = pat;
                       return redirectedObject;
                     },
                     explicitTarget,
                   ]) === redirectedObject
                     && redirectedPat === explicitTarget,
                     'always redirected object constructor return');
                   let primitiveRedirectedPat;
                   const primitiveRedirected = Reflect.construct(rawAlways, [
                     function (pat) {
                       primitiveRedirectedPat = pat;
                       return 7;
                     },
                     9,
                   ]);
                   check(primitiveRedirected instanceof rawAlways
                     && primitiveRedirectedPat === 9,
                     'always redirected primitive constructor fallback');

                   phase = 'poisoning';
                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned public surface reached');
                   };
                   const poisonedTarget = {};
                   for (const name of [
                     'never', '_never', 'always', '_always'
                   ]) {
                     Object.defineProperty(poisonedTarget, name, {
                       configurable: true,
                       get() {
                         poisonHits++;
                         throw new Error(`target ${name} getter reached`);
                       },
                     });
                   }
                   Pattern.prototype.never = poison;
                   Pattern.prototype._never = poison;
                   Pattern.prototype.always = poison;
                   Pattern.prototype._always = poison;
                   globalThis.never = poison;
                   globalThis.always = poison;
                   rustelScope.never = poison;
                   rustelScope.always = poison;
                   check(apply(rawNever, null, [
                     poison, poisonedTarget
                   ]) === poisonedTarget, 'saved never poisoning');
                   check(apply(rawAlways, null, [
                     value => value, poisonedTarget
                   ]) === poisonedTarget && poisonHits === 0,
                   'saved always poisoning');

                   globalThis.rawNeverAlwaysSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawNeverAlwaysSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawNeverAlwaysSurfaceError"), None);
    assert_eq!(runtime.get_number("rawNeverAlwaysSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw never/always surface and forwarding");
}

#[test]
fn raw_never_always_are_eager_host_free_and_preserve_only_returned_ownership() {
    let never_native = semantic_runtime(
        r#"(() => {
             let ignored = { marker: 'never-ignored' };
             let outer = { marker: 'never-outer' };
             let extra = { marker: 'never-extra' };
             globalThis.rawNeverNativeDroppedRefs = [
               new WeakRef(ignored), new WeakRef(outer), new WeakRef(extra)
             ];
             const source = pure('raw-never-native').setSteps(3);
             const location = { start: 4, end: 9 };
             source.__pure_loc = location;
             const query = source.query;
             const steps = source._steps;
             const pureValue = source.__pure;
             const result = Reflect.apply(Pattern.prototype._never, outer, [
               ignored, source, extra
             ]);
             globalThis.rawNeverNativeIdentity = Number(
               result === source
               && result.query === query
               && result._steps === steps
               && Object.hasOwn(result, '__pure')
               && result.__pure === pureValue
               && result.__pure_loc === location
             );
             ignored = outer = extra = null;
             return result;
           })()"#,
    );
    assert_eq!(never_native.get_number("rawNeverNativeIdentity"), Some(1.0));
    assert!(
        !never_native.active_needs_host(),
        "an exact native raw-never target became host-backed"
    );
    assert_eq!(
        never_native
            .active_pattern()
            .expect("raw never native pattern")
            .steps,
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        never_native.run_gc();
    }
    never_native
        .eval(
            r#"globalThis.rawNeverNativeTransientsPruned = Number(
                 rawNeverNativeDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               )"#,
        )
        .unwrap();
    assert_eq!(
        never_native.get_number("rawNeverNativeTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&never_native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-never-native"
    );
    assert_clean(&never_native, "raw never eager native target");
    never_native.clear_active();

    let always_native = semantic_runtime(
        r#"(() => {
             globalThis.rawAlwaysNativeHits = 0;
             globalThis.rawAlwaysNativeMismatch = 0;
             let transient = { marker: 'always-transient' };
             let callback = function (pat) {
               'use strict';
               rawAlwaysNativeHits++;
               if (this !== undefined || new.target !== undefined
                   || pat !== source
                   || transient.marker !== 'always-transient') {
                 rawAlwaysNativeMismatch++;
               }
               return pat;
             };
             globalThis.rawAlwaysNativeDroppedRefs = [
               new WeakRef(callback), new WeakRef(transient)
             ];
             const source = pure('raw-always-native').setSteps(5);
             const location = { start: 11, end: 17 };
             source.__pure_loc = location;
             const query = source.query;
             const steps = source._steps;
             const pureValue = source.__pure;
             const result = source._always(callback);
             globalThis.rawAlwaysNativeIdentity = Number(
               result === source
               && result.query === query
               && result._steps === steps
               && Object.hasOwn(result, '__pure')
               && result.__pure === pureValue
               && result.__pure_loc === location
             );
             callback = transient = null;
             return result;
           })()"#,
    );
    assert_eq!(always_native.get_number("rawAlwaysNativeHits"), Some(1.0));
    assert_eq!(
        always_native.get_number("rawAlwaysNativeMismatch"),
        Some(0.0)
    );
    assert_eq!(
        always_native.get_number("rawAlwaysNativeIdentity"),
        Some(1.0)
    );
    assert!(
        !always_native.active_needs_host(),
        "an eager identity raw-always return retained its callback"
    );
    assert_eq!(
        always_native
            .active_pattern()
            .expect("raw always native pattern")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        always_native.run_gc();
    }
    always_native
        .eval(
            r#"globalThis.rawAlwaysNativeTransientsPruned = Number(
                 rawAlwaysNativeDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               )"#,
        )
        .unwrap();
    assert_eq!(
        always_native.get_number("rawAlwaysNativeTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&always_native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-always-native"
    );
    assert_eq!(
        always_native.get_number("rawAlwaysNativeHits"),
        Some(1.0),
        "raw always callback was reread during query"
    );
    assert_clean(&always_native, "raw always eager native return");
    always_native.clear_active();

    let never_owned = semantic_runtime(
        r#"(() => {
             globalThis.rawNeverOwnedQueryHits = 0;
             const makePattern = kept => new Pattern(state => {
               rawNeverOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-never-owned' };
             let ignored = { marker: 'never-owned-ignored' };
             let outer = { marker: 'never-owned-outer' };
             let extra = { marker: 'never-owned-extra' };
             let target = makePattern(kept);
             globalThis.rawNeverOwnedKeptRef = new WeakRef(kept);
             globalThis.rawNeverOwnedDroppedRefs = [
               new WeakRef(ignored), new WeakRef(outer), new WeakRef(extra)
             ];
             const result = Reflect.apply(Pattern.prototype._never, outer, [
               ignored, target, extra
             ]);
             globalThis.rawNeverOwnedIdentity = Number(result === target);
             kept = ignored = outer = extra = target = null;
             return result;
           })()"#,
    );
    assert_eq!(never_owned.get_number("rawNeverOwnedIdentity"), Some(1.0));
    assert_eq!(never_owned.get_number("rawNeverOwnedQueryHits"), Some(0.0));
    assert!(
        never_owned.active_needs_host(),
        "raw never lost its exact JavaScript target ownership"
    );
    for _ in 0..3 {
        never_owned.run_gc();
    }
    never_owned
        .eval(
            r#"globalThis.rawNeverOwnedKeptAlive = Number(
                 rawNeverOwnedKeptRef.deref()?.marker === 'raw-never-owned'
               );
               globalThis.rawNeverOwnedTransientsPruned = Number(
                 rawNeverOwnedDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(never_owned.get_number("rawNeverOwnedKeptAlive"), Some(1.0));
    assert_eq!(
        never_owned.get_number("rawNeverOwnedTransientsPruned"),
        Some(1.0)
    );
    let never_values = query_active(&never_owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        never_values
            .iter()
            .any(|value| value.contains("raw-never-owned")),
        "raw never JavaScript target was not queryable: {never_values:?}"
    );
    assert_eq!(never_owned.get_number("rawNeverOwnedQueryHits"), Some(1.0));
    assert_clean(&never_owned, "raw never returned ownership");
    never_owned.clear_active();

    let always_owned = semantic_runtime(
        r#"(() => {
             globalThis.rawAlwaysOwnedHits = 0;
             globalThis.rawAlwaysOwnedQueryHits = 0;
             globalThis.rawAlwaysOwnedMismatch = 0;
             const makePattern = kept => new Pattern(state => {
               rawAlwaysOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-always-owned' };
             let outer = { marker: 'always-owned-outer' };
             let target = { marker: 'always-owned-target' };
             let transient = { marker: 'always-owned-transient' };
             let extra = { marker: 'always-owned-extra' };
             let callback = function (received) {
               'use strict';
               rawAlwaysOwnedHits++;
               if (this !== undefined || new.target !== undefined
                   || received !== target
                   || transient.marker !== 'always-owned-transient') {
                 rawAlwaysOwnedMismatch++;
               }
               return makePattern(kept);
             };
             globalThis.rawAlwaysOwnedKeptRef = new WeakRef(kept);
             globalThis.rawAlwaysOwnedDroppedRefs = [
               new WeakRef(outer), new WeakRef(target),
               new WeakRef(transient), new WeakRef(extra),
               new WeakRef(callback)
             ];
             const result = Reflect.apply(Pattern.prototype._always, outer, [
               callback, target, extra
             ]);
             kept = outer = target = transient = extra = callback = null;
             return result;
           })()"#,
    );
    assert_eq!(always_owned.get_number("rawAlwaysOwnedHits"), Some(1.0));
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedQueryHits"),
        Some(0.0)
    );
    assert_eq!(always_owned.get_number("rawAlwaysOwnedMismatch"), Some(0.0));
    assert!(
        always_owned.active_needs_host(),
        "raw always lost its returned JavaScript Pattern ownership"
    );
    for _ in 0..3 {
        always_owned.run_gc();
    }
    always_owned
        .eval(
            r#"globalThis.rawAlwaysOwnedKeptAlive = Number(
                 rawAlwaysOwnedKeptRef.deref()?.marker === 'raw-always-owned'
               );
               globalThis.rawAlwaysOwnedTransientsPruned = Number(
                 rawAlwaysOwnedDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedKeptAlive"),
        Some(1.0)
    );
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedTransientsPruned"),
        Some(1.0)
    );
    let always_values = query_active(&always_owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        always_values
            .iter()
            .any(|value| value.contains("raw-always-owned")),
        "raw always JavaScript terminal was not queryable: {always_values:?}"
    );
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedHits"),
        Some(1.0),
        "raw always callback ran again during query"
    );
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedQueryHits"),
        Some(1.0)
    );
    assert_clean(&always_owned, "raw always returned ownership");
    always_owned.clear_active();
}

#[test]
fn raw_never_always_are_pool_neutral_and_preserve_nested_accounting() {
    rustel_core::reset_stepwise_entries_materialised();
    let never_direct = semantic_runtime(
        r#"(() => {
             globalThis.rawNeverDirectHits = 0;
             return pure('raw-never-direct')._never(() => {
               rawNeverDirectHits++;
               return gap(20000).shrink(0);
             });
           })()"#,
    );
    assert_eq!(never_direct.get_number("rawNeverDirectHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw never charged or entered its ignored callback"
    );
    query_active(&never_direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(never_direct.get_number("rawNeverDirectHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw never charged shared work during query"
    );
    assert_clean(&never_direct, "raw never direct pool neutrality");
    never_direct.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let always_direct = semantic_runtime("pure('raw-always-direct')._always(pat => pat)");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&always_direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw always identity charged shared work"
    );
    assert_clean(&always_direct, "raw always direct pool neutrality");
    always_direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    rustel_core::reset_stepwise_entries_materialised();
    let suppressed = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawNeverSuppressedHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._never(() => {{
                 rawNeverSuppressedHits++;
                 return gap({}).shrink(0);
               }})
             );
           }})()"#,
        limit + 1
    ));
    assert_eq!(suppressed.get_number("rawNeverSuppressedHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&suppressed, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(suppressed.get_number("rawNeverSuppressedHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw never entered or charged nested +1 callback work"
    );
    assert_clean(&suppressed, "raw never nested suppression");
    suppressed.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawAlwaysExactHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._always(() => {{
                 rawAlwaysExactHits++;
                 return gap({limit}).shrink(0);
               }})
             );
           }})()"#
    ));
    assert_eq!(exact.get_number("rawAlwaysExactHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(exact.get_number("rawAlwaysExactHits"), Some(1.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw always hid or duplicated exact nested accounting"
    );
    assert_clean(&exact, "raw always nested exact accounting");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawAlwaysOverHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._always(() => {{
                 rawAlwaysOverHits++;
                 return gap({}).shrink(0);
               }})
             );
           }})()"#,
        limit + 1
    ));
    assert_eq!(over.get_number("rawAlwaysOverHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(over.get_number("rawAlwaysOverHits"), Some(1.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw always nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw always nested +1 refusal");
    over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let recovery = semantic_runtime(&format!(
        r#"pure(0).polyBind(() =>
             pure('x')._always(() => gap({limit}).shrink(0))
           )"#
    ));
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw always nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw always nested recovery");
    recovery.clear_active();

    // Callback-created arbitrary graphs remain governed by their own
    // operation-specific limits; the two raw slots add no new policy.
}

#[test]
fn raw_range_pair_keep_surface_forwarding_and_dynamic_body_order() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawRange = Pattern.prototype._range;
                   const rawRange2 = Pattern.prototype._range2;

                   phase = 'surface';
                   check(typeof rawRange === 'function'
                     && typeof rawRange2 === 'function'
                     && rawRange !== Pattern.prototype.range
                     && rawRange2 !== Pattern.prototype.range2
                     && rawRange !== rawRange2, 'raw identities');
                   for (const [name, raw] of [
                     ['_range', rawRange], ['_range2', rawRange2]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} prototype descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${name} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(
                         raw, 'prototype'
                       ), `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'range', '_range', 'rangex', '_rangex',
                       'range2', '_range2'
                     ].includes(name)).join('|');
                   check(rawOrder === 'range|_range|rangex|range2|_range2',
                     `raw range order: ${rawOrder}`);
                   check(!('_rangex' in Pattern.prototype)
                     && !('_rangex' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_rangex'
                     ), '_rangex was inferred');
                   check(typeof globalThis.range === 'function'
                     && typeof globalThis.rangex === 'function'
                     && typeof globalThis.range2 === 'function'
                     && typeof Pattern.prototype.range === 'function'
                     && typeof Pattern.prototype.rangex === 'function'
                     && typeof Pattern.prototype.range2 === 'function',
                     'raw install replaced public registrations');

                   phase = 'range-body-order';
                   const rangeLog = [];
                   const min = {
                     valueOf() {
                       rangeLog.push(`min.valueOf:${this === min}`);
                       return 2;
                     },
                   };
                   const max = {
                     valueOf() {
                       rangeLog.push(`max.valueOf:${this === max}`);
                       return 9;
                     },
                   };
                   const rangeTerminal = { marker: 'range-terminal' };
                   const afterMul = {};
                   Object.defineProperty(afterMul, 'add', {
                     configurable: true,
                     get() {
                       rangeLog.push('add.get');
                       return function (received) {
                         rangeLog.push(`add.call:${this === afterMul}`
                           + `:${received === min}`);
                         return rangeTerminal;
                       };
                     },
                   });
                   const rangeTarget = {};
                   Object.defineProperty(rangeTarget, 'mul', {
                     configurable: true,
                     get() {
                       rangeLog.push('mul.get');
                       return function (difference) {
                         rangeLog.push(`mul.call:${this === rangeTarget}`
                           + `:${difference}`);
                         return afterMul;
                       };
                     },
                   });
                   const decoy = {};
                   Object.defineProperty(decoy, 'mul', {
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   check(apply(rawRange, decoy, [
                     min, max, rangeTarget, 'ignored'
                   ]) === rangeTerminal, '_range replaced terminal result');
                   check(rangeLog.join('|') === [
                     'mul.get', 'max.valueOf:true', 'min.valueOf:true',
                     'mul.call:true:7', 'add.get', 'add.call:true:true'
                   ].join('|'), `_range body order: ${rangeLog}`);

                   phase = 'range2-body-order';
                   const range2Log = [];
                   const range2Terminal = { marker: 'range2-terminal' };
                   const bipolarResult = {};
                   Object.defineProperty(bipolarResult, '_range', {
                     configurable: true,
                     get() {
                       range2Log.push('_range.get');
                       return function (receivedMin, receivedMax) {
                         range2Log.push(`_range.call:${this === bipolarResult}`
                           + `:${receivedMin === min}:${receivedMax === max}`);
                         return range2Terminal;
                       };
                     },
                   });
                   const range2Target = {};
                   Object.defineProperty(range2Target, 'fromBipolar', {
                     configurable: true,
                     get() {
                       range2Log.push('fromBipolar.get');
                       return function () {
                         range2Log.push(
                           `fromBipolar.call:${this === range2Target}`
                         );
                         return bipolarResult;
                       };
                     },
                   });
                   Object.defineProperty(decoy, 'fromBipolar', {
                     get() {
                       throw new Error('range2 wrapper receiver reached');
                     },
                   });
                   check(apply(rawRange2, decoy, [
                     min, max, range2Target, 'ignored'
                   ]) === range2Terminal,
                   '_range2 replaced terminal result');
                   check(range2Log.join('|') === [
                     'fromBipolar.get', 'fromBipolar.call:true',
                     '_range.get', '_range.call:true:true:true'
                   ].join('|'), `_range2 body order: ${range2Log}`);

                   phase = 'receiver-and-constructors';
                   const receiverTerminal = { marker: 'receiver-terminal' };
                   const receiverTarget = {
                     mul(difference) {
                       check(this === receiverTarget && difference === 4,
                         'method receiver or difference changed');
                       return {
                         add(received) {
                           check(received === 2,
                             'method original min changed');
                           return receiverTerminal;
                         },
                       };
                     },
                   };
                   check(apply(rawRange, receiverTarget, [2, 6])
                     === receiverTerminal,
                     'two explicit arguments did not use wrapper receiver');
                   check(Reflect.construct(rawRange, [
                     2, 6, receiverTarget
                   ]) === receiverTerminal,
                   '_range constructor did not return object terminal');
                   const primitiveRange2Target = {
                     fromBipolar() {
                       return { _range() { return 7; } };
                     },
                   };
                   const primitiveConstruction = Reflect.construct(
                     rawRange2, [2, 6, primitiveRange2Target]
                   );
                   check(primitiveConstruction instanceof rawRange2,
                     '_range2 constructor returned primitive terminal');
                   let omitted;
                   try { apply(rawRange, receiverTarget, [2]); }
                   catch (error) { omitted = error; }
                   check(omitted instanceof TypeError,
                     'omission did not leave the body target undefined');

                   phase = 'throw-cutoffs';
                   let coercionHits = 0;
                   const cutoffMin = {
                     valueOf() { coercionHits++; return 2; },
                   };
                   const cutoffMax = {
                     valueOf() { coercionHits++; return 6; },
                   };
                   const getterStop = {};
                   Object.defineProperty(getterStop, 'mul', {
                     get() { throw new Error('mul-get-stop'); },
                   });
                   let getterError;
                   try {
                     apply(rawRange, null, [
                       cutoffMin, cutoffMax, getterStop
                     ]);
                   } catch (error) { getterError = error; }
                   check(getterError?.message === 'mul-get-stop'
                     && coercionHits === 0,
                     'mul getter did not precede subtraction coercion');

                   const cutoffLog = [];
                   const maxStop = {
                     valueOf() {
                       cutoffLog.push('max.valueOf');
                       throw new Error('max-stop');
                     },
                   };
                   const minAfterStop = {
                     valueOf() { cutoffLog.push('min.valueOf'); return 2; },
                   };
                   const callAfterStop = {};
                   Object.defineProperty(callAfterStop, 'mul', {
                     get() {
                       cutoffLog.push('mul.get');
                       return function () { cutoffLog.push('mul.call'); };
                     },
                   });
                   let coercionError;
                   try {
                     apply(rawRange, null, [
                       minAfterStop, maxStop, callAfterStop
                     ]);
                   } catch (error) { coercionError = error; }
                   check(coercionError?.message === 'max-stop'
                     && cutoffLog.join('|') === 'mul.get|max.valueOf',
                     `subtraction cutoff changed: ${cutoffLog}`);

                   let range2Calls = 0;
                   const range2GetterStop = {};
                   Object.defineProperty(range2GetterStop, 'fromBipolar', {
                     get() { throw new Error('from-bipolar-stop'); },
                   });
                   let range2Error;
                   try {
                     apply(rawRange2, null, [
                       { valueOf() { range2Calls++; return 2; } },
                       { valueOf() { range2Calls++; return 6; } },
                       range2GetterStop
                     ]);
                   } catch (error) { range2Error = error; }
                   check(range2Error?.message === 'from-bipolar-stop'
                     && range2Calls === 0,
                     'fromBipolar getter did not cut off later phases');

                   phase = 'configured-parser';
                   const parserLog = [];
                   setStringParser(value => {
                     parserLog.push(value);
                     return pure(Number(value));
                   });
                   const parsedRange = pure(.5)._range('2', '4');
                   check(parserLog.join('|') === '2',
                     `_range parser phase: ${parserLog}`);
                   const parsedRange2 = pure(0)._range2('2', '4');
                   check(parserLog.join('|') === '2|2',
                     `_range2 parser phase: ${parserLog}`);
                   const state = {
                     span: { begin: 0, end: 1 }, controls: {},
                   };
                   const parsedValues = [
                     parsedRange.query(state)[0]?.value,
                     parsedRange2.query(state)[0]?.value,
                   ];
                   check(parsedValues[0] === 3 && parsedValues[1] === 3
                     && parserLog.join('|') === '2|2',
                     `parser reread or values changed: ${parserLog}`
                       + `:${parsedValues}`);
                   globalThis.rawRangePairSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawRangePairSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawRangePairSurfaceError"), None);
    assert_eq!(runtime.get_number("rawRangePairSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw range pair surface and routing");
}

#[test]
fn raw_range_pair_keep_scalar_timing_steps_and_tag_loss() {
    let third = Fraction::new(1, 3);
    let two_thirds = Fraction::new(2, 3);
    for (label, expression, expected_values) in [
        (
            "range forward",
            "fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(10, 20)",
            ["10", "15", "20"],
        ),
        (
            "range reversed",
            "fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(20, 10)",
            ["20", "15", "10"],
        ),
        (
            "range equal",
            "fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(7, 7)",
            ["7", "7", "7"],
        ),
        (
            "range2 bipolar",
            "fastcat(pure(-1), pure(0), pure(1)).setSteps(3)._range2(10, 20)",
            ["10", "15", "20"],
        ),
    ] {
        let runtime = semantic_runtime(&format!(
            r#"(() => {{
                 const result = {expression};
                 globalThis.rawRangePairTagless = Number(
                   !Object.prototype.hasOwnProperty.call(result, '__pure')
                   && !Object.prototype.hasOwnProperty.call(
                     result, '__pure_loc'
                   )
                 );
                 return result;
               }})()"#
        ));
        assert_eq!(
            runtime.get_number("rawRangePairTagless"),
            Some(1.0),
            "{label}: register fast-path tags survived"
        );
        let pattern = runtime.active_pattern().expect("active raw range result");
        assert_eq!(pattern.steps, Some(Fraction::int(3)), "{label}: steps");
        assert!(pattern.is_pure(), "{label}: native purity");
        assert!(
            pattern.as_pure().is_none(),
            "{label}: structural pure fast path survived"
        );
        assert!(
            !runtime.active_needs_host(),
            "{label}: native scalar result retained JavaScript"
        );
        let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            haps.iter().map(|hap| hap.value.show()).collect::<Vec<_>>(),
            expected_values,
            "{label}: values"
        );
        assert_eq!(
            haps.iter()
                .map(|hap| (
                    hap.part.begin,
                    hap.part.end,
                    hap.whole.expect("raw range whole").begin,
                    hap.whole.expect("raw range whole").end,
                ))
                .collect::<Vec<_>>(),
            [
                (Fraction::ZERO, third, Fraction::ZERO, third),
                (third, two_thirds, third, two_thirds),
                (two_thirds, Fraction::ONE, two_thirds, Fraction::ONE),
            ],
            "{label}: timing"
        );
        assert_clean(&runtime, label);
        runtime.clear_active();
    }

    let tagged = semantic_runtime(
        r#"(() => {
             const rangeSource = pure(.5);
             const range2Source = pure(0);
             rangeSource.__pure_loc = { start: 10, end: 11 };
             range2Source.__pure_loc = { start: 20, end: 21 };
             const ranged = rangeSource._range(10, 20);
             const ranged2 = range2Source._range2(10, 20);
             globalThis.rawRangePureTagLoss = Number(
               Object.prototype.hasOwnProperty.call(rangeSource, '__pure')
               && Object.prototype.hasOwnProperty.call(range2Source, '__pure')
               && Object.prototype.hasOwnProperty.call(
                 rangeSource, '__pure_loc'
               )
               && Object.prototype.hasOwnProperty.call(
                 range2Source, '__pure_loc'
               )
               && !Object.prototype.hasOwnProperty.call(ranged, '__pure')
               && !Object.prototype.hasOwnProperty.call(ranged, '__pure_loc')
               && !Object.prototype.hasOwnProperty.call(ranged2, '__pure')
               && !Object.prototype.hasOwnProperty.call(ranged2, '__pure_loc')
             );
             return stack(ranged, ranged2);
           })()"#,
    );
    assert_eq!(tagged.get_number("rawRangePureTagLoss"), Some(1.0));
    assert!(
        !tagged.active_needs_host(),
        "tag-loss controls retained JavaScript"
    );
    assert_eq!(
        query_active(&tagged, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["15", "15"]
    );
    assert_clean(&tagged, "raw range pair pure-tag loss");
    tagged.clear_active();
}

#[test]
fn raw_range_pair_are_eager_and_preserve_only_returned_ownership() {
    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawRangeEagerHits = 0;
             globalThis.rawRange2EagerHits = 0;
             globalThis.rawRangePairLateHits = 0;
             globalThis.rawRangePairMismatch = 0;

             const rangeSource = pure(.5);
             const rangeMiddle = {};
             Object.defineProperty(rangeSource, 'mul', {
               configurable: true,
               value(difference) {
                 globalThis.rawRangeEagerHits++;
                 if (this !== rangeSource || difference !== 4) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return rangeMiddle;
               },
             });
             Object.defineProperty(rangeMiddle, 'add', {
               configurable: true,
               value(min) {
                 globalThis.rawRangeEagerHits++;
                 if (this !== rangeMiddle || min !== 2) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return pure('range-eager').setSteps(1);
               },
             });
             const ranged = rangeSource._range(2, 6);
             Object.defineProperty(rangeSource, 'mul', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range-late');
               },
             });
             Object.defineProperty(rangeMiddle, 'add', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range-late');
               },
             });

             const range2Source = pure(0);
             const bipolarMiddle = {};
             Object.defineProperty(range2Source, 'fromBipolar', {
               configurable: true,
               value() {
                 globalThis.rawRange2EagerHits++;
                 if (this !== range2Source) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return bipolarMiddle;
               },
             });
             Object.defineProperty(bipolarMiddle, '_range', {
               configurable: true,
               value(min, max) {
                 globalThis.rawRange2EagerHits++;
                 if (this !== bipolarMiddle || min !== 2 || max !== 6) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return pure('range2-eager').setSteps(1);
               },
             });
             const ranged2 = range2Source._range2(2, 6);
             Object.defineProperty(range2Source, 'fromBipolar', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range2-late');
               },
             });
             Object.defineProperty(bipolarMiddle, '_range', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range2-late');
               },
             });
             return stack(ranged, ranged2);
           })()"#,
    );
    assert_eq!(eager.get_number("rawRangeEagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawRange2EagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawRangePairMismatch"), Some(0.0));
    assert!(
        !eager.active_needs_host(),
        "eager native range results retained chain functions"
    );
    assert_eq!(
        query_active(&eager, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["range-eager", "range2-eager"]
    );
    assert_eq!(eager.get_number("rawRangePairLateHits"), Some(0.0));
    assert_clean(&eager, "raw range pair eager mutation");
    eager.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             const apply = Reflect.apply;
             let rangeKept = { marker: 'raw-range-owned' };
             let rangeMin = { valueOf() { return 2; } };
             let rangeMax = { valueOf() { return 6; } };
             let rangeSource = {};
             let rangeMiddle = {};
             globalThis.rawRangeKeptRef = new WeakRef(rangeKept);
             globalThis.rawRangeDroppedRefs = [
               new WeakRef(rangeMin), new WeakRef(rangeMax),
               new WeakRef(rangeSource), new WeakRef(rangeMiddle)
             ];
             rangeSource.mul = () => rangeMiddle;
             rangeMiddle.add = () => pure(rangeKept).setSteps(1);
             const ranged = apply(Pattern.prototype._range, null, [
               rangeMin, rangeMax, rangeSource
             ]);

             let range2Kept = { marker: 'raw-range2-owned' };
             let range2Min = { marker: 'range2-min' };
             let range2Max = { marker: 'range2-max' };
             let range2Source = {};
             let bipolarMiddle = {};
             globalThis.rawRange2KeptRef = new WeakRef(range2Kept);
             globalThis.rawRange2DroppedRefs = [
               new WeakRef(range2Min), new WeakRef(range2Max),
               new WeakRef(range2Source), new WeakRef(bipolarMiddle)
             ];
             range2Source.fromBipolar = () => bipolarMiddle;
             bipolarMiddle._range = () => pure(range2Kept).setSteps(1);
             const ranged2 = apply(Pattern.prototype._range2, null, [
               range2Min, range2Max, range2Source
             ]);

             rangeKept = rangeMin = rangeMax = rangeSource = rangeMiddle = null;
             range2Kept = range2Min = range2Max = range2Source
               = bipolarMiddle = null;
             return stack(ranged, ranged2);
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw custom-chain results lost JavaScript-owned values"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawRangeKeptAlive = Number(
                 rawRangeKeptRef.deref()?.marker === 'raw-range-owned'
               );
               globalThis.rawRange2KeptAlive = Number(
                 rawRange2KeptRef.deref()?.marker === 'raw-range2-owned'
               );
               globalThis.rawRangeChainPruned = Number(
                 rawRangeDroppedRefs.every(ref => ref.deref() === undefined)
                 && rawRange2DroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawRangeKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawRange2KeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawRangeChainPruned"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-range-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-range2-owned"))
    );
    assert_clean(&owned, "raw range pair returned ownership");
    owned.clear_active();
}

/// The bounded raw pair is literal eager composition on stable native
/// receivers. Upstream query functions are receiver-independent closures, but
/// native public `mul`/`fromBipolar` still read the wrapper's stored graph.
/// Keep reassigned native and custom query functions executable as a named
/// residual instead of silently extending this slice to all public methods.
#[test]
fn raw_range_query_replacement_remains_an_explicit_residual() {
    let runtime = semantic_runtime(
        r#"(() => {
             const nativeSource = pure(.5);
             const nativeOwner = pure(.25);
             nativeSource.query = nativeOwner.query;

             const customSource = pure(.5);
             customSource.query = state => pure(.25).query(state);
             return stack(
               nativeSource._range(2, 6),
               nativeSource._range2(2, 6),
               customSource._range(2, 6),
               customSource._range2(2, 6)
             );
           })()"#,
    );
    assert!(
        !runtime.active_needs_host(),
        "the current residual unexpectedly retained replacement queries"
    );
    assert_eq!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["4", "5", "4", "5"],
        "native residual changed; pinned Node produces 3/4.5 for each pair"
    );
    assert_clean(&runtime, "raw range query replacement residual");
    runtime.clear_active();
}

#[test]
fn raw_range_pair_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(2, 6),
             fastcat(pure(-1), pure(0), pure(1)).setSteps(3)._range2(2, 6)
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw range pair charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw range pair charged query-time stepwise work"
    );
    assert_clean(&direct, "raw range pair pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return pure(.5)._range(2, 6);
             }}
             if (value === 1) {{
               return pure(0)._range2(2, 6);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw range pair consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw range pair shared-pool neutrality");
    shared.clear_active();
}

#[test]
fn raw_shrink_grow_keep_direct_body_routing_under_surface_mutation() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawShrink = Pattern.prototype._shrink;
                   const rawGrow = Pattern.prototype._grow;
                   const receiver = sequence('a', 'b', 'c', 'd');

                   // The raw wrapper appends its receiver after the caller's
                   // rest arguments. Omission therefore leaves `pat`
                   // undefined, while an explicit undefined is still an
                   // amount and leaves the receiver in the `pat` position.
                   phase = 'omission';
                   for (const [name, raw] of [
                     ['_shrink', rawShrink], ['_grow', rawGrow]
                   ]) {
                     let omitted;
                     try { apply(raw, receiver, []); }
                     catch (error) { omitted = error; }
                     check(omitted instanceof TypeError,
                       `${name} omission did not reach undefined pat`);
                   }
                   phase = 'explicit-undefined';
                   check(apply(rawShrink, receiver, [undefined])._steps.show()
                     === '16/1', '_shrink(undefined) collapsed to omission');
                   check(apply(rawGrow, receiver, [undefined])._steps.show()
                     === '16/1', '_grow(undefined) collapsed to omission');

                   phase = 'pattern-rejection';
                   const patternedAmount = sequence(1, 2);
                   for (const [name, raw] of [
                     ['_shrink', rawShrink], ['_grow', rawGrow]
                   ]) {
                     let rejected;
                     try { apply(raw, receiver, [patternedAmount]); }
                     catch (error) { rejected = error; }
                     check(rejected?.name === 'Error'
                       && rejected.message === 'Invalid argument',
                       `${name} patternified its raw amount`);
                   }

                   // Two or more explicit arguments retarget the body's
                   // second parameter; later values and the wrapper receiver
                   // are ignored by the two-parameter canonical closure.
                   phase = 'retarget';
                   const decoy = sequence('decoy-a', 'decoy-b');
                   decoy.shrinklist = function () {
                     throw new Error('wrapper receiver reached');
                   };
                   const target = sequence('target-a', 'target-b');
                   const targetCalls = [];
                   target.shrinklist = function (amount) {
                     targetCalls.push({ self: this, amount });
                     return [gap(3)];
                   };
                   const pair = [1, 2];
                   check(apply(rawShrink, decoy, [pair, target])._steps.show()
                     === '3/1', '_shrink did not retarget to arg two');
                   check(apply(rawGrow, decoy, [pair, target, 'ignored'])
                     ._steps.show() === '3/1',
                     '_grow did not ignore arguments after its target');
                   check(targetCalls.length === 2
                     && targetCalls.every(call => call.self === target)
                     && targetCalls[0].amount === pair
                     && targetCalls[1].amount instanceof Fraction._original
                     && targetCalls[1].amount !== pair
                     && targetCalls[1].amount.show() === '-1/2',
                     'retargeted helper arguments');

                   phase = 'no-steps';
                   const lexicalNothing = nothing;
                   const noSteps = new Pattern(() => []);
                   check(apply(rawShrink, decoy, [Symbol('ignored'), noSteps])
                     === lexicalNothing, '_shrink no-step shortcut');
                   check(apply(rawGrow, decoy, [Symbol('ignored'), noSteps])
                     === lexicalNothing, '_grow no-step shortcut');

                   phase = 'shrink-helper-identity';
                   const shrinkSource = sequence('a', 'b', 'c', 'd');
                   const shrinkPair = [1, 2];
                   let shrinkArgument;
                   let shrinkThis;
                   shrinkSource.shrinklist = function (amount) {
                     shrinkThis = this;
                     shrinkArgument = amount;
                     return [gap(2), gap(3)];
                   };
                   check(apply(rawShrink, shrinkSource, [shrinkPair])
                     ._steps.show() === '5/1', 'raw shrink result steps');
                   check(shrinkThis === shrinkSource
                     && shrinkArgument === shrinkPair,
                     'raw shrink changed helper receiver or pair identity');

                   // Member lookup precedes grow's Fraction negation; the
                   // helper then returns the exact Array reversed in place
                   // before that same Array performs the step reduction.
                   phase = 'grow-order';
                   const growSource = sequence('a', 'b', 'c', 'd');
                   const left = gap(2);
                   const right = gap(3);
                   const returned = [left, right];
                   const order = [];
                   const savedReverse = Array.prototype.reverse;
                   const savedReduce = Array.prototype.reduce;
                   const savedSub = Fraction._original.prototype.sub;
                   let growArgument;
                   Object.defineProperty(growSource, 'shrinklist', {
                     configurable: true,
                     get() {
                       order.push('get');
                       return function (amount) {
                         order.push(`call:${this === growSource}`);
                         growArgument = amount;
                         return returned;
                       };
                     },
                   });
                   returned.reverse = function (...args) {
                     order.push(`reverse:${this === returned}:${this[0] === left}`);
                     return apply(savedReverse, this, args);
                   };
                   returned.reduce = function (...args) {
                     order.push(`reduce:${this === returned}:${this[0] === right}`);
                     return apply(savedReduce, this, args);
                   };
                   Fraction._original.prototype.sub = function (...args) {
                     order.push('fraction');
                     return apply(savedSub, this, args);
                   };
                   let orderedGrow;
                   try {
                     orderedGrow = apply(rawGrow, growSource, [[1, 2]]);
                   } finally {
                     Fraction._original.prototype.sub = savedSub;
                   }
                   check(order.join('|') ===
                     'get|fraction|call:true|reverse:true:true|reduce:true:true',
                     `raw grow order: ${order}`);
                   check(returned[0] === right && returned[1] === left,
                     'raw grow did not reverse the helper Array in place');
                   check(growArgument instanceof Fraction._original
                     && growArgument.show() === '-1/2'
                     && orderedGrow._steps.show() === '5/1',
                     'raw grow Fraction or result steps');

                   // The registered body and raw wrapper are lexical. Public
                   // free functions, public methods, and same-named globals
                   // are not redispatched after the raw method is captured.
                   phase = 'lexical-poison';
                   const savedGlobals = {
                     Fraction: globalThis.Fraction,
                     stepcat: globalThis.stepcat,
                     nothing: globalThis.nothing,
                   };
                   const savedMethods = {
                     shrink: Pattern.prototype.shrink,
                     grow: Pattern.prototype.grow,
                   };
                   const lexicalSource = sequence('a', 'b');
                   let lexicalCalls = 0;
                   lexicalSource.shrinklist = function () {
                     lexicalCalls++;
                     return [gap(2)];
                   };
                   const poison = function () {
                     throw new Error('poisoned public surface');
                   };
                   let lexicalShrink;
                   let lexicalGrow;
                   let lexicalNoSteps;
                   globalThis.Fraction = poison;
                   globalThis.stepcat = poison;
                   globalThis.nothing = poison;
                   Pattern.prototype.shrink = poison;
                   Pattern.prototype.grow = poison;
                   try {
                     lexicalShrink = apply(rawShrink, lexicalSource, [1]);
                     lexicalGrow = apply(rawGrow, lexicalSource, [1]);
                     lexicalNoSteps = apply(
                       rawShrink, lexicalSource, [Symbol('ignored'), noSteps]
                     );
                   } finally {
                     globalThis.Fraction = savedGlobals.Fraction;
                     globalThis.stepcat = savedGlobals.stepcat;
                     globalThis.nothing = savedGlobals.nothing;
                     Pattern.prototype.shrink = savedMethods.shrink;
                     Pattern.prototype.grow = savedMethods.grow;
                   }
                   check(lexicalCalls === 2
                     && lexicalShrink._steps.show() === '2/1'
                     && lexicalGrow._steps.show() === '2/1'
                     && lexicalNoSteps === lexicalNothing,
                     'raw bodies redispatched through a public surface');
                   globalThis.rawDirectRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawDirectRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawDirectRoutingError"), None);
    assert_eq!(runtime.get_number("rawDirectRoutingOkay"), Some(1.0));
    assert_clean(&runtime, "raw direct routing");
}

#[test]
fn raw_shrink_is_eager_host_free_but_retains_js_owned_helper_results() {
    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawPatternAmountHits = 0;
             globalThis.rawPatternAmountMismatch = 0;
             globalThis.rawPatternLateHits = 0;
             const amount = sequence(1, 2);
             const source = sequence('a', 'b', 'c', 'd');
             source.shrinklist = function (received) {
               globalThis.rawPatternAmountHits++;
               if (this !== source || received !== amount) {
                 globalThis.rawPatternAmountMismatch++;
               }
               return [pure('raw-eager').setSteps(2)];
             };
             const result = source._shrink(amount);
             source.shrinklist = function () {
               globalThis.rawPatternLateHits++;
               return [pure('late').setSteps(9)];
             };
             return result;
           })()"#,
    );
    assert!(
        !eager.active_needs_host(),
        "a custom raw helper returning a native graph must not become PatternOfJs"
    );
    assert_eq!(eager.get_number("rawPatternAmountHits"), Some(1.0));
    assert_eq!(eager.get_number("rawPatternAmountMismatch"), Some(0.0));
    let values = query_active(&eager, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(values.iter().any(|value| value == "raw-eager"));
    assert_eq!(
        eager.get_number("rawPatternLateHits"),
        Some(0.0),
        "raw shrink unexpectedly retained dynamic helper dispatch"
    );
    assert_clean(&eager, "raw eager purity");
    eager.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let marker = { marker: 'raw-owned-helper-result' };
             globalThis.rawOwnedResultRef = new WeakRef(marker);
             let amount = sequence(1, 2);
             let source = sequence('a', 'b', 'c', 'd');
             source.shrinklist = function (received) {
               if (received !== amount) throw new Error('raw amount identity');
               return [pure(marker).setSteps(2)];
             };
             const result = source._shrink(amount);
             marker = null;
             amount = null;
             source = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "a raw helper result containing a JS-owned value lost its sidecar"
    );
    owned.run_gc();
    owned.run_gc();
    owned.run_gc();
    owned
        .eval(
            r#"globalThis.rawOwnedResultAlive = Number(
                 rawOwnedResultRef.deref()?.marker === 'raw-owned-helper-result'
               )"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawOwnedResultAlive"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values
            .iter()
            .any(|value| value.contains("raw-owned-helper-result")),
        "raw shrink lost its helper result's JS-owned value: {values:?}"
    );
    assert_clean(&owned, "raw helper ownership");
    owned.clear_active();
}

#[test]
fn raw_shrink_grow_share_the_canonical_stepwise_pool_and_recover_atomically() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    for raw in ["_shrink", "_grow"] {
        rustel_core::reset_stepwise_entries_materialised();
        let exact = semantic_runtime(&format!("gap({limit}).{raw}(0)"));
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            limit,
            "{raw} double-charged its exact default-helper handoff"
        );
        assert!(
            !exact.active_needs_host(),
            "{raw} scalar default unexpectedly retained JavaScript"
        );
        query_active(&exact, Fraction::ZERO, Fraction::ZERO).unwrap();
        assert_clean(&exact, &format!("{raw} exact boundary"));
        exact.clear_active();

        rustel_core::reset_stepwise_entries_materialised();
        let over = semantic_runtime(&format!("gap({}).{raw}(0)", limit + 1));
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "{raw} materialised after its +1 refusal"
        );
        assert!(matches!(
            query_active(&over, Fraction::ZERO, Fraction::ONE),
            Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
                operation: "shrink/grow",
                minimum_entries,
                limit: actual,
            })) if minimum_entries == limit + 1 && actual == limit
        ));
        assert_clean(&over, &format!("{raw} +1 refusal"));
        over.evaluate_score("pure('recovered')", &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("{raw} heap did not recover: {error}"));
        assert_eq!(
            query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "recovered"
        );
        assert_clean(&over, &format!("{raw} +1 recovery"));
        over.clear_active();
    }

    // A raw grow override is charged before any observable reverse, reduce,
    // spread iteration, or string reification. This is the same bounded
    // safety-phase divergence as the canonical public body.
    rustel_core::reset_stepwise_entries_materialised();
    let override_over = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawGrowHelperHits = 0;
             globalThis.rawGrowReverseHits = 0;
             globalThis.rawGrowReduceHits = 0;
             globalThis.rawGrowIteratorHits = 0;
             globalThis.rawGrowParserHits = 0;
             setStringParser(value => {{
               globalThis.rawGrowParserHits++;
               return pure(value);
             }});
             const source = gap(1);
             const returned = Array({}).fill('raw');
             returned.reverse = function () {{
               globalThis.rawGrowReverseHits++;
               return this;
             }};
             returned.reduce = function () {{
               globalThis.rawGrowReduceHits++;
               return 0;
             }};
             returned[Symbol.iterator] = function () {{
               globalThis.rawGrowIteratorHits++;
               return Array.prototype[Symbol.iterator].call(this);
             }};
             source.shrinklist = function () {{
               globalThis.rawGrowHelperHits++;
               return returned;
             }};
             return source._grow(1);
           }})()"#,
        limit + 1
    ));
    assert_eq!(override_over.get_number("rawGrowHelperHits"), Some(1.0));
    for counter in [
        "rawGrowReverseHits",
        "rawGrowReduceHits",
        "rawGrowIteratorHits",
        "rawGrowParserHits",
    ] {
        assert_eq!(
            override_over.get_number(counter),
            Some(0.0),
            "raw grow crossed its preflight at {counter}"
        );
    }
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&override_over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_clean(&override_over, "raw grow override refusal");
    override_over.clear_active();

    let exact_shared = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? gap(8192)._shrink(0)
               : gap(8192)._grow(0)
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact_shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "raw shrink/grow did not consume one shared query allowance"
    );
    assert_clean(&exact_shared, "raw exact shared pool");
    exact_shared.clear_active();

    let refused_shared = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? gap(8000)._shrink(0)
               : gap(8385)._grow(0)
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&refused_shared, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_000,
        "the refused raw branch materialised or used a separate pool"
    );
    assert_clean(&refused_shared, "raw shared-pool refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused_shared, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_000);
    assert_clean(&refused_shared, "raw shared-pool recovery");
    refused_shared.clear_active();
}

#[test]
fn canonical_fast_path_stays_private_and_falls_back_for_observable_lists() {
    rustel_core::reset_stepwise_entries_materialised();
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const savedHelper = Pattern.prototype.shrinklist;
                   const savedReverse = Array.prototype.reverse;
                   const savedReduce = Array.prototype.reduce;

                   // A wrapper around the saved canonical helper is itself an
                   // observable helper. It must receive a fully materialised
                   // list, and grow must reverse and reduce that same Array.
                   phase = 'wrapped-helper';
                   const wrappedSource = sequence('a', 'b', 'c', 'd');
                   let wrappedList;
                   let wrappedFirst;
                   let wrappedLast;
                   let reverseThis;
                   let reduceThis;
                   let beforeReverse;
                   let beforeReduce;
                   wrappedSource.shrinklist = function (amount) {
                     wrappedList = savedHelper.call(this, amount);
                     wrappedFirst = wrappedList[0];
                     wrappedLast = wrappedList[wrappedList.length - 1];
                     wrappedList.reverse = function (...args) {
                       reverseThis = this;
                       beforeReverse = this[0];
                       return apply(savedReverse, this, args);
                     };
                     wrappedList.reduce = function (...args) {
                       reduceThis = this;
                       beforeReduce = this[0];
                       return apply(savedReduce, this, args);
                     };
                     return wrappedList;
                   };
                   const wrappedGrow = wrappedSource.grow(1);
                   check(wrappedList.length === 4, 'wrapped helper saw a sentinel');
                   check(reverseThis === wrappedList && reduceThis === wrappedList,
                     'grow changed the helper Array identity');
                   check(beforeReverse === wrappedFirst
                     && beforeReduce === wrappedLast
                     && wrappedList[0] === wrappedLast,
                     'grow did not reverse the helper Array before reducing it');
                   check(wrappedGrow._steps.show() === '10/1',
                     'wrapped helper changed grow steps');

                   // Mutating the exact Array primitives observed by the
                   // canonical bodies must disable substitution and expose a
                   // real four-entry helper list to those mutations.
                   phase = 'reverse-mutation';
                   const reverseLengths = [];
                   Array.prototype.reverse = function (...args) {
                     reverseLengths.push(this.length);
                     return apply(savedReverse, this, args);
                   };
                   let reverseGrow;
                   try {
                     reverseGrow = sequence('a', 'b', 'c', 'd').grow(1);
                   } finally {
                     Array.prototype.reverse = savedReverse;
                   }
                   check(reverseLengths.includes(4),
                     `mutated reverse missed helper list: ${reverseLengths}`);
                   check(reverseGrow._steps.show() === '10/1',
                     'reverse fallback changed grow steps');

                   phase = 'reduce-mutation';
                   const reduceLengths = [];
                   Array.prototype.reduce = function (...args) {
                     reduceLengths.push(this.length);
                     return apply(savedReduce, this, args);
                   };
                   let reduceShrink;
                   try {
                     reduceShrink = sequence('a', 'b', 'c', 'd').shrink(1);
                   } finally {
                     Array.prototype.reduce = savedReduce;
                   }
                   check(reduceLengths.includes(4),
                     `mutated reduce missed helper list: ${reduceLengths}`);
                   check(reduceShrink._steps.show() === '10/1',
                     'reduce fallback changed shrink steps');

                   // An accessor zoom is another observable surface and must
                   // be read and called once for every materialised entry.
                   phase = 'zoom-accessor';
                   const zoomSource = sequence('a', 'b', 'c', 'd');
                   const savedZoom = zoomSource.zoom;
                   let zoomGets = 0;
                   let zoomCalls = 0;
                   Object.defineProperty(zoomSource, 'zoom', {
                     configurable: true,
                     get() {
                       zoomGets++;
                       return function (...args) {
                         zoomCalls++;
                         return apply(savedZoom, this, args);
                       };
                     },
                   });
                   const zoomShrink = zoomSource.shrink(1);
                   check(zoomGets === 4 && zoomCalls === 4,
                     `zoom fallback counts: ${zoomGets}/${zoomCalls}`);
                   check(zoomShrink instanceof Pattern,
                     'zoom fallback did not return a Pattern');

                   // The helper's strudel.cc Array.isArray(amount) lookup stays
                   // dynamic and sees the exact pair. Private preflight uses
                   // its captured intrinsic and must not expose the empty
                   // handoff Array as a second argument to this observer.
                   phase = 'array-is-array-poison';
                   const arrayCheckSource = sequence('a', 'b', 'c', 'd');
                   const pair = [1, 4];
                   const savedArrayIsArray = Array.isArray;
                   const arrayCheckArguments = [];
                   let arrayCheckShrink;
                   Array.isArray = function (value) {
                     arrayCheckArguments.push(value);
                     return savedArrayIsArray(value);
                   };
                   try {
                     arrayCheckShrink = arrayCheckSource.shrink(pair);
                   } finally {
                     Array.isArray = savedArrayIsArray;
                   }
                   check(arrayCheckArguments.length === 1
                     && arrayCheckArguments[0] === pair,
                     `private preflight reached dynamic Array.isArray: ${arrayCheckArguments.length}`);
                   check(arrayCheckShrink._steps.show() === '10/1',
                     'Array.isArray poison changed canonical shrink');

                   // The private marker uses captured WeakMap operations, so
                   // post-install prototype poisoning cannot observe or alter
                   // the otherwise eligible empty handoff Array.
                   phase = 'weakmap-poison';
                   const savedWeakGet = WeakMap.prototype.get;
                   const savedWeakSet = WeakMap.prototype.set;
                   let weakGets = 0;
                   let weakSets = 0;
                   let privateShrink;
                   let poisonError;
                   WeakMap.prototype.get = function () {
                     weakGets++;
                     throw new Error('dynamic WeakMap.get reached');
                   };
                   WeakMap.prototype.set = function () {
                     weakSets++;
                     throw new Error('dynamic WeakMap.set reached');
                   };
                   try {
                     privateShrink = sequence('a', 'b', 'c', 'd').shrink(1);
                   } catch (error) {
                     poisonError = error;
                   } finally {
                     WeakMap.prototype.get = savedWeakGet;
                     WeakMap.prototype.set = savedWeakSet;
                   }
                   check(poisonError === undefined,
                     `private marker escaped: ${poisonError}`);
                   check(weakGets === 0 && weakSets === 0,
                     `dynamic WeakMap operations: ${weakGets}/${weakSets}`);
                   check(privateShrink._steps.show() === '10/1',
                     'WeakMap poison changed canonical shrink');
                   globalThis.canonicalFastPathFallbackOkay = 1;
                 } catch (error) {
                   globalThis.canonicalFastPathFallbackError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("canonicalFastPathFallbackError"), None);
    assert_eq!(
        runtime.get_number("canonicalFastPathFallbackOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "canonical fast-path fallbacks");
    rustel_core::reset_stepwise_entries_materialised();
}

#[test]
fn patterned_shrink_keeps_receiver_override_and_owned_values_for_late_queries() {
    let runtime = semantic_runtime(
        r#"(() => {
             const source = sequence('a', 'b', 'c', 'd');
             const kept = { marker: 'owned-shrinklist' };
             globalThis.patternedShrinkKept = new WeakRef(kept);
             globalThis.patternedShrinkConstructionHits = 0;
             globalThis.patternedShrinkQueryHits = 0;
             globalThis.patternedShrinkThisMismatch = 0;
             source.shrinklist = function (amount) {
               globalThis.patternedShrinkConstructionHits++;
               if (this !== source) globalThis.patternedShrinkThisMismatch++;
               return [pure(`initial-${amount}`).setSteps(2)];
             };
             const result = source.shrink(sequence(1, 2));
             globalThis.patternedShrinkConstructionSteps = result._steps.show();
             source.shrinklist = function (amount) {
               globalThis.patternedShrinkQueryHits++;
               if (this !== source) globalThis.patternedShrinkThisMismatch++;
               return [pure(`${kept.marker}-${amount}`).setSteps(5)];
             };
             return result;
           })()"#,
    );
    assert!(
        runtime.active_needs_host(),
        "patterned shrink must retain its query-time JavaScript dispatch"
    );
    assert_eq!(
        runtime.get_number("patternedShrinkConstructionHits"),
        Some(2.0),
        "StepJoin did not resolve both cycle-zero factor haps during construction"
    );
    assert_eq!(
        runtime
            .get_string("patternedShrinkConstructionSteps")
            .as_deref(),
        Some("4/1")
    );

    runtime.run_gc();
    runtime.run_gc();
    runtime.run_gc();
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(
        runtime.get_number("patternedShrinkQueryHits"),
        Some(2.0),
        "query reused the eager shrinklist result"
    );
    assert_eq!(runtime.get_number("patternedShrinkThisMismatch"), Some(0.0));
    assert!(values.iter().any(|value| value == "owned-shrinklist-1"));
    assert!(values.iter().any(|value| value == "owned-shrinklist-2"));
    assert_eq!(
        runtime.active_pattern().expect("patterned shrink").steps,
        Some(Fraction::int(4)),
        "late replacement incorrectly refreshed eager StepJoin metadata"
    );
    runtime
        .eval(
            r#"globalThis.patternedShrinkOwnedAlive = Number(
                 patternedShrinkKept.deref()?.marker === 'owned-shrinklist'
               )"#,
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("patternedShrinkOwnedAlive"),
        Some(1.0),
        "the active PatternOfJs graph lost its receiver-owned closure value"
    );
    assert_clean(&runtime, "patterned shrink dynamic ownership");
    runtime.clear_active();
}

#[test]
fn patterned_shrink_observes_a_post_construction_prototype_replacement() {
    let runtime = semantic_runtime(
        r#"(() => {
             const source = sequence('a', 'b', 'c', 'd');
             const result = source.shrink(sequence(1, 2));
             globalThis.patternedShrinkSavedPrototype = Pattern.prototype.shrinklist;
             globalThis.patternedShrinkPrototypeHits = 0;
             globalThis.patternedShrinkPrototypeMismatch = 0;
             Pattern.prototype.shrinklist = function (amount) {
               globalThis.patternedShrinkPrototypeHits++;
               if (this !== source) globalThis.patternedShrinkPrototypeMismatch++;
               return [pure(`prototype-${amount}`).setSteps(3)];
             };
             return result;
           })()"#,
    );
    assert!(runtime.active_needs_host());
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(
        runtime.get_number("patternedShrinkPrototypeHits"),
        Some(2.0)
    );
    assert_eq!(
        runtime.get_number("patternedShrinkPrototypeMismatch"),
        Some(0.0)
    );
    assert!(values.iter().any(|value| value == "prototype-1"));
    assert!(values.iter().any(|value| value == "prototype-2"));
    runtime
        .eval("Pattern.prototype.shrinklist = patternedShrinkSavedPrototype;")
        .unwrap();
    assert_clean(&runtime, "patterned shrink prototype replacement");
    runtime.clear_active();
}

#[test]
fn canonical_shrink_default_helper_has_one_exact_typed_preflight() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!("gap({limit}).shrink([0, {limit}])"));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "the canonical consumer charged or materialised the helper array twice"
    );
    query_active(&exact, Fraction::ZERO, Fraction::ZERO).unwrap();
    assert_clean(&exact, "canonical shrink exact boundary");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!("gap({}).shrink([0, {}])", limit + 1, limit + 1));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "oversized canonical shrink built zoom or stepcat wrappers"
    );
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_clean(&over, "canonical shrink oversized refusal");

    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("same runtime should recover from canonical shrink refusal");
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "canonical shrink refusal recovery");
    over.clear_active();
}

#[test]
fn canonical_override_arrays_preflight_before_grow_mutation_and_reification() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        r#"(() => {{
             const source = gap(1);
             const returned = Array({limit}).fill(nothing);
             source.shrinklist = function () {{ return returned; }};
             const result = source.grow(1);
             globalThis.canonicalOverrideExactSame = Number(
               returned.length === {limit} && returned.every(value => value === nothing)
             );
             return result;
           }})()"#
    ));
    assert_eq!(exact.get_number("canonicalOverrideExactSame"), Some(1.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), limit);
    assert!(
        query_active(&exact, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&exact, "canonical override exact boundary");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.canonicalGrowReverseHits = 0;
             globalThis.canonicalGrowReduceHits = 0;
             globalThis.canonicalGrowParserHits = 0;
             setStringParser(value => {{
               globalThis.canonicalGrowParserHits++;
               return pure(value);
             }});
             const source = gap(1);
             const returned = Array({}).fill('raw');
             returned.reverse = function () {{
               globalThis.canonicalGrowReverseHits++;
               return this;
             }};
             returned.reduce = function () {{
               globalThis.canonicalGrowReduceHits++;
               return 0;
             }};
             source.shrinklist = function () {{ return returned; }};
             return source.grow(1);
           }})()"#,
        limit + 1
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_eq!(over.get_number("canonicalGrowReverseHits"), Some(0.0));
    assert_eq!(over.get_number("canonicalGrowReduceHits"), Some(0.0));
    assert_eq!(over.get_number("canonicalGrowParserHits"), Some(0.0));
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_clean(&over, "canonical grow override refusal");
    over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let grown_default = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.canonicalGrownHelperIteratorHits = 0;
             const helper = Pattern.prototype.shrinklist;
             const source = gap({limit});
             source.shrinklist = function () {{
               const returned = helper.call(this, [0, {limit}]);
               returned.push(this);
               returned[Symbol.iterator] = function () {{
                 globalThis.canonicalGrownHelperIteratorHits++;
                 return Array.prototype[Symbol.iterator].call(this);
               }};
               return returned;
             }};
             return source.shrink(1);
           }})()"#
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "the default helper should complete before its override grows the array"
    );
    assert_eq!(
        grown_default.get_number("canonicalGrownHelperIteratorHits"),
        Some(0.0),
        "a grown helper array reached stepcat spread after refusal"
    );
    assert!(matches!(
        query_active(&grown_default, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_clean(&grown_default, "grown canonical helper handoff");
    grown_default.clear_active();
}

#[test]
fn canonical_shrink_shares_one_query_pool_without_default_helper_double_charge() {
    let exact = semantic_runtime("fastcat(pure(0), pure(1)).polyBind(() => gap(8192).shrink(0))");
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "two canonical helper arrays did not consume exactly one shared pool"
    );
    assert_clean(&exact, "canonical shrink cumulative exact side");
    exact.clear_active();

    let refused = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? stack(...gap(8000).shrinklist(0))
               : gap(8385).shrink(0)
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&refused, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_000,
        "canonical shrink materialised after the mixed shared-pool refusal"
    );
    assert_clean(&refused, "mixed helper/canonical shrink refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_000);
    assert_clean(&refused, "mixed helper/canonical shrink recovery");
    refused.clear_active();
}

#[test]
fn zip_surface_filtering_empty_and_singleton_shapes_are_exact() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .eval(
            r#"(() => {
                 const descriptor = Object.getOwnPropertyDescriptor(Pattern.prototype, 's_zip');
                 globalThis.zipName = zip.name;
                 globalThis.zipLength = zip.length;
                 globalThis.zipAlias = Number(s_zip === zip);
                 globalThis.zipConstructible = Number(new zip() instanceof Pattern);
                 globalThis.zipProtoAbsent = Number(
                   !Object.prototype.hasOwnProperty.call(Pattern.prototype, 'zip')
                 );
                 globalThis.zipProtoSlot = Number(
                   descriptor.value === undefined && descriptor.writable &&
                   descriptor.enumerable && descriptor.configurable
                 );
               })()"#,
        )
        .unwrap();
    assert_eq!(runtime.get_string("zipName").as_deref(), Some("zip"));
    assert_eq!(runtime.get_number("zipLength"), Some(0.0));
    assert_eq!(runtime.get_number("zipAlias"), Some(1.0));
    assert_eq!(runtime.get_number("zipConstructible"), Some(1.0));
    assert_eq!(runtime.get_number("zipProtoAbsent"), Some(1.0));
    assert_eq!(runtime.get_number("zipProtoSlot"), Some(1.0));

    runtime
        .evaluate_score("zip()", &TranspileOptions::default())
        .unwrap();
    assert_eq!(runtime.active_pattern().expect("empty zip").steps, None);
    assert!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );

    runtime
        .evaluate_score(
            r#"(() => {
                 globalThis.zipParserHits = 0;
                 setStringParser(value => {
                   globalThis.zipParserHits++;
                   return pure(value);
                 });
                 return zip('raw', 42, false);
               })()"#,
            &TranspileOptions::default(),
        )
        .unwrap();
    assert_eq!(runtime.get_number("zipParserHits"), Some(0.0));
    assert_eq!(runtime.active_pattern().expect("filtered zip").steps, None);
    assert!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );

    runtime
        .evaluate_score(
            r#"(() => {
                 const source = pure('solo').setSteps(2);
                 const result = zip(source);
                 globalThis.zipSingletonDistinct = Number(result !== source);
                 return result;
               })()"#,
            &TranspileOptions::default(),
        )
        .unwrap();
    assert_eq!(runtime.get_number("zipSingletonDistinct"), Some(1.0));
    assert_eq!(
        runtime.active_pattern().expect("singleton zip").steps,
        Some(Fraction::int(2))
    );
    let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
    assert!(!haps.is_empty());
    assert!(haps.iter().all(|hap| hap.value.show() == "solo"));
    assert_clean(&runtime, "zip surface and filtering");
    runtime.clear_active();
}

#[test]
fn zip_zero_steps_throw_an_eager_generic_javascript_error() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .eval(
            r#"try {
                 zip(nothing, silence);
               } catch (error) {
                 globalThis.zipZeroName = error.name;
                 globalThis.zipZeroMessage = error.message;
               }"#,
        )
        .unwrap();
    assert_eq!(runtime.get_string("zipZeroName").as_deref(), Some("Error"));
    assert_eq!(
        runtime.get_string("zipZeroMessage").as_deref(),
        Some("Division by Zero")
    );
    assert_clean(&runtime, "zero-step zip error");
}

#[test]
fn zip_retains_callback_sidecars_after_source_wrappers_are_collected() {
    rustel_core::reset_stepwise_entries_materialised();
    let runtime = semantic_runtime(
        r#"zip(
             pure('left').fmap(value => value + '-kept').setSteps(2),
             pure('right').setSteps(3)
           )"#,
    );
    assert!(runtime.active_needs_host());
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);

    runtime.run_gc();
    runtime.run_gc();
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(values.iter().any(|value| value == "left-kept"));
    assert!(values.iter().any(|value| value == "right"));
    assert_clean(&runtime, "zip callback sidecars");
    runtime.clear_active();
}

#[test]
fn zip_lcm_fanout_is_preflighted_at_the_exact_boundary() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!("zip(gap({limit}), silence)"));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);
    assert_eq!(
        exact.active_pattern().expect("exact zip").steps,
        Some(Fraction::int(i128::from(limit)))
    );
    assert!(query_active(&exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&exact, "exact zip LCM fanout");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!("zip(gap({}), silence)", limit + 1));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "LCM limit+1 built slowed graph entries before refusal"
    );
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "zip",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_clean(&over, "oversized zip LCM fanout");

    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("same runtime should accept a new score after zip refusal");
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "zip LCM recovery");
    over.clear_active();
}

#[test]
fn query_time_zip_calls_share_one_cumulative_allowance_and_recover() {
    let accepted =
        semantic_runtime("fastcat(pure(0), pure(1)).polyBind(() => zip(gap(8192), silence))");
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&accepted, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        4,
        "two exact-boundary zip calls should retain two slowed operands each"
    );
    assert_clean(&accepted, "cumulative zip exact side");
    accepted.clear_active();

    let refused =
        semantic_runtime("fastcat(pure(0), pure(1)).polyBind(() => zip(gap(8193), silence))");
    rustel_core::reset_stepwise_entries_materialised();
    let error = query_active(&refused, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "zip",
            minimum_entries: 16_386,
            limit: 16_384,
        })
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        2,
        "the second zip call materialised before the cumulative overshoot"
    );
    assert_clean(&refused, "cumulative zip refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);
    assert_clean(&refused, "cumulative zip recovery");
    refused.clear_active();
}

#[test]
fn shrink_tour_and_zip_share_one_query_time_stepwise_allowance() {
    let runtime = semantic_runtime(
        r#"(() => {
             const many = Array(79).fill(silence);
             return fastcat(pure(0), pure(1), pure(2)).polyBind(value => {
               if (value === 0) return gap(7000).shrink(0);
               if (value === 1) return pure('x').tour(...many);
               return zip(gap(3000), silence);
             });
           })()"#,
    );

    rustel_core::reset_stepwise_entries_materialised();
    let error = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "zip",
            minimum_entries: 16_400,
            limit: 16_384,
        })
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        13_400,
        "zip did not share shrink/tour's allowance or built after refusal"
    );
    assert_clean(&runtime, "mixed shrink/tour/zip refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&runtime, Fraction::ZERO, Fraction::new(1, 3)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 7_000);
    assert_clean(&runtime, "mixed shrink/tour/zip recovery");
    runtime.clear_active();
}

#[test]
fn stepalt_source_and_lcm_expansions_are_preflighted_before_reification() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        "stepalt(Array({}).fill(silence), silence)",
        limit / 2
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "two groups with an 8,192-cycle LCM should fill the shared bound"
    );
    assert_eq!(
        exact.active_pattern().expect("exact stepalt").steps,
        Some(Fraction::int(i128::from(limit)))
    );
    assert!(query_active(&exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&exact, "exact stepalt expansion");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             globalThis.stepaltParserHits = 0;
             globalThis.stepaltGetterHits = 0;
             setStringParser(value => {
               globalThis.stepaltParserHits++;
               return pure(value);
             });
             const group = Array(8193).fill('raw');
             Object.defineProperty(group, 0, {
               get() { globalThis.stepaltGetterHits++; return 'getter'; }
             });
             return stepalt(group, silence);
           })()"#,
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_eq!(over.get_number("stepaltParserHits"), Some(0.0));
    assert_eq!(over.get_number("stepaltGetterHits"), Some(0.0));
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "stepalt",
            minimum_entries: 16_386,
            limit: actual,
        })) if actual == limit
    ));
    assert_clean(&over, "oversized stepalt LCM expansion");

    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("same runtime should recover after stepalt refusal");
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "stepalt expansion recovery");
    over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let source_exact = semantic_runtime(
        r#"(() => {
             const result = stepalt([], Array(16384).fill(nothing));
             globalThis.stepaltExactSourceIsNothing = Number(result === nothing);
             return result;
           })()"#,
    );
    assert_eq!(
        source_exact.get_number("stepaltExactSourceIsNothing"),
        Some(1.0),
        "the exact source-cardinality boundary did not return lexical nothing"
    );
    assert_eq!(
        source_exact
            .active_pattern()
            .expect("exact source stepalt")
            .steps,
        Some(Fraction::ZERO)
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(
        query_active(&source_exact, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&source_exact, "exact stepalt source list");
    source_exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let source_over = semantic_runtime(
        r#"(() => {
             globalThis.stepaltSourceParserHits = 0;
             setStringParser(value => {
               globalThis.stepaltSourceParserHits++;
               return pure(value);
             });
             return stepalt(Array(16385).fill('raw'), []);
           })()"#,
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_eq!(source_over.get_number("stepaltSourceParserHits"), Some(0.0));
    assert!(matches!(
        query_active(&source_over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "stepalt",
            minimum_entries: 16_385,
            limit: actual,
        })) if actual == limit
    ));
    assert_clean(&source_over, "oversized stepalt source list");
    source_over.clear_active();
}

#[test]
fn stepalt_checked_lcm_overflow_refuses_before_parser_or_index_getters() {
    rustel_core::reset_stepwise_entries_materialised();
    let runtime = semantic_runtime(
        r#"(() => {
             globalThis.stepaltOverflowParserHits = 0;
             globalThis.stepaltOverflowGetterHits = 0;
             setStringParser(value => {
               globalThis.stepaltOverflowParserHits++;
               return pure(value);
             });
             const lengths = [
               53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
               101, 103, 107, 109, 113, 127, 131, 137, 139, 149
             ];
             const groups = lengths.map(length => Array(length).fill('raw'));
             Object.defineProperty(groups[0], 0, {
               get() { globalThis.stepaltOverflowGetterHits++; return 'getter'; }
             });
             return stepalt(...groups);
           })()"#,
    );

    assert_eq!(runtime.get_number("stepaltOverflowParserHits"), Some(0.0));
    assert_eq!(runtime.get_number("stepaltOverflowGetterHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::NativeFraction {
            operation: "stepalt"
        }))
    ));
    assert_clean(&runtime, "stepalt checked LCM overflow");
    runtime.clear_active();
}

/// Step scaling with an absurd but convertible factor used to panic through
/// the fraction crate's `ck` (native: the session ends; wasm: abort). Each
/// score now reaches the host as the typed `NativeFraction` refusal naming
/// the function the score called: at construction (`expand`), at a late
/// query-time cycle (`pace` runs at ≈5e37, so cycle 4 overflows; `extend` on
/// one step overflows from cycle 2), through a patterned factor's stepJoin,
/// whose two legal 1e38-step slices total past i128::MAX, and in `drop`'s
/// zoom into the slot [1/1e38, 1), which a half-cycle query cannot be mapped
/// into.
#[test]
fn overflowing_step_scaling_refuses_through_the_typed_channel() {
    let cycle = |n: i128| (Fraction::int(n), Fraction::int(n + 1));
    for (source, (begin, end), operation) in [
        (r#"s("bd cp").expand(1e38)"#, cycle(0), "expand"),
        (r#"s("bd cp").pace(1e38)"#, cycle(4), "pace"),
        (r#"s("bd cp").pace(1e38)"#, cycle(1000), "pace"),
        (r#"s("bd").extend(1e38)"#, cycle(2), "extend"),
        (r#"s("bd").ply(1e38)"#, cycle(2), "ply"),
        (
            r#"s("bd").expand(sequence(1e38, 1e38))"#,
            cycle(0),
            "stepcat",
        ),
        (
            r#"s("bd").expand(1e38).drop(1)"#,
            (Fraction::ZERO, Fraction::new(1, 2)),
            "drop",
        ),
    ] {
        let runtime = semantic_runtime(source);
        let result = query_active(&runtime, begin, end);
        assert!(
            matches!(
                &result,
                Err(QueryError::Limit(QueryLimit::NativeFraction { operation: got }))
                    if *got == operation
            ),
            "{source} over [{begin:?}, {end:?}): {result:?}"
        );
        assert_clean(&runtime, source);
        runtime.clear_active();
    }

    // The ordinary factor on the same path is untouched, late cycles included.
    let runtime = semantic_runtime(r#"s("bd cp").pace(4)"#);
    let haps =
        query_active(&runtime, Fraction::int(1000), Fraction::int(1001)).expect("pace(4) is legal");
    assert_eq!(haps.len(), 4, "pace(4) lost or duplicated events");
    assert_clean(&runtime, "legal pace");
    runtime.clear_active();
}

/// The inputs each have a valid native step count, but their common grid (or
/// the polyBind scaling ratio) does not fit in a native fraction. The list
/// aliases and composition wrappers must preserve the refusal at query time.
#[test]
fn overflowing_step_combinations_refuse_through_the_typed_channel() {
    let a = "pure('a').setSteps(1e38)";
    let b = "pure('b').setSteps(17)";
    let cases = [
        (format!("stack({a}, {b})"), "stack"),
        (r#"stack(s("bd@1e38 sd"), s("cp@17 hh"))"#.into(), "stack"),
        (
            r#"stack(s("bd").segment(1e38), s("cp").segment(17))"#.into(),
            "stack",
        ),
        (format!("cat({a}, {b})"), "slowcat"),
        (format!("slowcat({a}, {b})"), "slowcat"),
        (format!("xfade({a}, 0.5, {b})"), "stack"),
        (format!("{a}.appBoth({b})"), "appBoth"),
        (
            "pure(1).setSteps(1e38).add.mix(pure(2).setSteps(17))".into(),
            "appBoth",
        ),
        (
            "pure('a').setSteps(1e38).polyBind(() => pure('b').setSteps(0.5))".into(),
            "polyJoin",
        ),
        (format!("{a}.jux(p => p.setSteps(17))"), "juxBy"),
        (
            "pure('a').setSteps(1e38).sometimes(p => p.setSteps(17))".into(),
            "stack",
        ),
        ("s('hh').euclidish(15, 17, 1e38)".into(), "euclidish"),
    ];
    for (source, operation) in cases {
        let runtime = semantic_runtime(&source);
        let result = query_active(&runtime, Fraction::ZERO, Fraction::ONE);
        assert!(
            matches!(
                &result,
                Err(QueryError::Limit(QueryLimit::NativeFraction { operation: got }))
                    if *got == operation
            ),
            "{source}: {result:?}"
        );
        assert_clean(&runtime, &source);
        runtime.clear_active();
    }
}

/// Pins that a missing weight whose known weights overflow the checked sum
/// refuses with the typed `NativeFraction` limit instead of panicking, through
/// `stepcat`, `timecat` and `tour`.
#[test]
fn stepcat_weight_average_overflow_refuses_through_the_typed_channel() {
    for source in [
        r#"stepcat([1e38, "bd"], [1e38, "sd"], [undefined, "hh"])"#,
        r#"stepcat([1e38, "bd"], [1e38, "sd"], [NaN, "hh"])"#,
        r#"timecat([1e38, "bd"], [1e38, "sd"], [, "hh"])"#,
        // `tour` flattens its tail arrays one level, where a bare `undefined`
        // is the `_steps` TypeError, so a NaN weight stands for the missing one.
        r#"s("bd").tour([1e38, "cp"], [1e38, "cp"], [NaN, "hh"])"#,
    ] {
        let runtime = semantic_runtime(source);
        let result = query_active(&runtime, Fraction::ZERO, Fraction::ONE);
        assert!(
            matches!(
                &result,
                Err(QueryError::Limit(QueryLimit::NativeFraction { operation: got }))
                    if *got == "stepcat"
            ),
            "{source} over [0, 1): {result:?}"
        );
        assert_clean(&runtime, source);
        runtime.clear_active();
    }
}

#[test]
fn query_time_stepalt_calls_share_one_cumulative_allowance_and_recover() {
    let accepted = semantic_runtime(
        r#"(() => {
             const group = Array(4096).fill(silence);
             return fastcat(pure(0), pure(1))
               .polyBind(() => stepalt(group, silence));
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&accepted, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "two individually legal 8,192-entry stepalt calls did not share the allowance"
    );
    assert_clean(&accepted, "cumulative stepalt exact side");
    accepted.clear_active();

    let refused = semantic_runtime(
        r#"(() => {
             const group = Array(4097).fill(silence);
             return fastcat(pure(0), pure(1))
               .polyBind(() => stepalt(group, silence));
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    let error = query_active(&refused, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "stepalt",
            minimum_entries: 16_388,
            limit: 16_384,
        })
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_194,
        "the second stepalt call materialised before the cumulative refusal"
    );
    assert_clean(&refused, "cumulative stepalt refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_194);
    assert_clean(&refused, "cumulative stepalt recovery");
    refused.clear_active();
}

#[test]
fn stepalt_shares_the_existing_shrink_tour_and_zip_allowance() {
    let runtime = semantic_runtime(
        r#"(() => {
             const many = Array(69).fill(silence);
             const alternate = Array(1743).fill(silence);
             return fastcat(pure(0), pure(1), pure(2), pure(3)).polyBind(value => {
               if (value === 0) return gap(5000).shrink(0);
               if (value === 1) return pure('x').tour(...many);
               if (value === 2) return zip(gap(3000), silence);
               return stepalt(alternate, silence);
             });
           })()"#,
    );

    rustel_core::reset_stepwise_entries_materialised();
    let error = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap_err();
    assert!(matches!(
        error,
        QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "stepalt",
            minimum_entries: 16_386,
            limit: 16_384,
        })
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        9_902,
        "stepalt used a private pool or materialised after the mixed overshoot"
    );
    assert_clean(&runtime, "mixed shrink/tour/zip/stepalt refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&runtime, Fraction::ZERO, Fraction::new(3, 4)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 9_902);
    assert_clean(&runtime, "mixed shrink/tour/zip/stepalt recovery");
    runtime.clear_active();
}

#[test]
fn stepalt_drops_filtered_sidecars_even_when_a_retained_source_is_opaque() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    let baseline = rustel_jsruntime::cells_live();
    runtime
        .evaluate_score(
            r#"(() => {
                 const filtered = pure('drop')
                   .fmap(value => value)
                   .setSteps(-1);
                 const retained = new Pattern(
                   state => pure('keep').query(state), 1
                 );
                 return stepalt([filtered], [retained]);
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("construct opaque stepalt with one filtered callback");

    for _ in 0..3 {
        runtime.run_gc();
    }
    assert_eq!(
        rustel_jsruntime::cells_live(),
        baseline + 1,
        "the retained opaque source conservatively rooted a filtered source sidecar"
    );
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(values, ["keep"]);
    assert_clean(&runtime, "stepalt filtered sidecar pruning");

    runtime.clear_active();
    for _ in 0..3 {
        runtime.run_gc();
    }
    assert_eq!(
        rustel_jsruntime::cells_live(),
        baseline,
        "the retained stepalt callback did not collect with the active root"
    );
}

#[test]
fn stepalt_filtered_js_values_do_not_survive_opaque_outer_compositions() {
    for (label, outer) in [
        ("direct", "inner"),
        ("ordinary outer", "stack(inner, pure('tail'))"),
        ("eager callback", "inner.every(2, pattern => pattern)"),
    ] {
        let runtime = JsRuntime::new().unwrap();
        runtime.install_semantic_bindings().unwrap();

        // Construct the opaque retained source in an earlier frame. This
        // isolates stepalt's ownership boundary: the source cannot already
        // have conservatively absorbed the later filtered value cells.
        runtime
            .evaluate_score(
                r#"(() => {
                     globalThis.stepaltSavedOpaque = new Pattern(
                       state => pure('opaque').query(state), 1
                     );
                     return stepaltSavedOpaque;
                   })()"#,
                &TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("{label}: construct retained opaque: {error}"));

        runtime
            .evaluate_score(
                &format!(
                    r#"(() => {{
                         const droppedObject = {{ marker: 'drop-object' }};
                         const droppedArray = Array.from(
                           {{ length: 64 }}, (_, i) => ({{ i }})
                         );
                         const droppedPattern = pure('drop-pattern');
                         globalThis.stepaltDroppedRefs = [
                           new WeakRef(droppedObject),
                           new WeakRef(droppedArray),
                           new WeakRef(droppedPattern)
                         ];

                         const keptObject = {{ marker: 47 }};
                         const keptFunction = value => `${{value}}!`;
                         const sharedObject = {{ marker: 99 }};
                         const sharedPattern = pure(sharedObject);
                         const filteredShared = sharedPattern.fast(1).setSteps(-1);
                         globalThis.stepaltKeptRefs = [
                           new WeakRef(keptObject),
                           new WeakRef(keptFunction),
                           new WeakRef(sharedObject)
                         ];
                         const inner = stepalt(
                           [
                             pure(droppedObject).setSteps(-1),
                             pure(droppedArray).setSteps(-1),
                             pure(droppedPattern).setSteps(-1),
                             filteredShared
                           ],
                           [
                             pure(keptObject),
                             pure(keptFunction),
                             stepaltSavedOpaque,
                             sharedPattern
                           ]
                         );
                         const result = {outer};
                         globalThis.stepaltOwnershipResult = result;
                         globalThis.stepaltSavedOpaque = null;
                         return result;
                       }})()"#,
                ),
                &TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("{label}: construct ownership graph: {error}"));

        for _ in 0..3 {
            runtime.run_gc();
        }
        runtime
            .evaluate_score(
                r#"(() => {
                     const drops = stepaltDroppedRefs.map(ref => ref.deref());
                     const keptObject = stepaltKeptRefs[0].deref();
                     const keptFunction = stepaltKeptRefs[1].deref();
                     const sharedObject = stepaltKeptRefs[2].deref();
                     const state = {
                       span: { begin: 0, end: 1 }, controls: {}
                     };
                     const values = stepaltOwnershipResult
                       .query(state).map(hap => hap.value);
                     globalThis.stepaltDroppedValuesCollected = Number(
                       drops.every(value => value === undefined)
                     );
                     globalThis.stepaltRetainedValuesAlive = Number(
                       keptObject !== undefined
                       && keptFunction !== undefined
                       && sharedObject !== undefined
                       && values.includes(keptObject)
                       && values.includes(keptFunction)
                       && values.includes(sharedObject)
                       && values.includes('opaque')
                       && keptObject.marker === 47
                       && sharedObject.marker === 99
                       && keptFunction('kept') === 'kept!'
                     );
                     return stepaltOwnershipResult;
                   })()"#,
                &TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("{label}: inspect ownership after GC: {error}"));
        assert_eq!(
            runtime.get_number("stepaltDroppedValuesCollected"),
            Some(1.0),
            "{label}: a filtered object, array, or Pattern value stayed rooted"
        );
        assert_eq!(
            runtime.get_number("stepaltRetainedValuesAlive"),
            Some(1.0),
            "{label}: a retained object/function value lost identity or ownership"
        );
        assert_clean(&runtime, label);
        runtime.clear_active();
    }
}

#[test]
fn stepalt_saved_exclusion_survives_transform_before_late_owner_publication() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 globalThis.stepaltFutureOpaque = new Pattern(
                   state => pure('opaque').query(state), 1
                 );
                 return stepaltFutureOpaque;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("construct prior-turn opaque source");
    runtime
        .evaluate_score(
            r#"(() => {
                 const dropped = { marker: 'future-owner' };
                 const owner = pure(dropped);
                 globalThis.stepaltFutureRef = new WeakRef(dropped);
                 globalThis.stepaltFutureOwner = owner;
                 globalThis.stepaltFutureExcluded = stepalt(
                   [owner.fast(1).setSteps(-1)],
                   [stepaltFutureOpaque]
                 );
                 return pure('intermediate');
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("save a persistent exclusion and its old owner");

    runtime
        .evaluate_score(
            r#"(() => {
                 // The exclusion must survive this metadata-only transform
                 // even though its owner has not entered the new frame yet.
                 const transformed = stepaltFutureExcluded.fast(1);
                 // Publishing the old owner later invalidates live-frame
                 // suppression, but must not erase the saved wrapper's proof.
                 stepaltFutureOwner.every(2, pattern => pattern);
                 globalThis.stepaltFutureOwner = null;
                 globalThis.stepaltFutureExcluded = null;
                 globalThis.stepaltFutureOpaque = null;
                 return stack(transformed, pure('tail'));
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("compose transformed exclusion after late owner publication");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            "globalThis.stepaltFutureCollected = Number(\
               stepaltFutureRef.deref() === undefined);",
        )
        .unwrap();
    assert_eq!(runtime.get_number("stepaltFutureCollected"), Some(1.0));
    assert_eq!(
        runtime.active_exclusion_count(),
        1,
        "durable publication cleared the future-owner exclusion"
    );
    assert_clean(&runtime, "persistent future-owner stepalt exclusion");
    runtime.clear_active();
}

#[test]
fn all_filtered_stepalt_does_not_pollute_a_later_opaque_outer_graph() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 const dropped = { marker: 'all-filtered' };
                 globalThis.stepaltAllFilteredRef = new WeakRef(dropped);
                 nothing._steps = 7;
                 const inner = stepalt([pure(dropped).setSteps(-1)]);
                 globalThis.stepaltAllFilteredIdentity = Number(inner === nothing);
                 globalThis.stepaltAllFilteredSteps = Number(inner._steps);
                 return stack(
                   inner,
                   new Pattern(state => pure('kept').query(state), 1)
                 );
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("construct all-filtered stepalt inside opaque outer graph");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            "globalThis.stepaltAllFilteredCollected = Number(\
               stepaltAllFilteredRef.deref() === undefined);",
        )
        .unwrap();
    assert_eq!(runtime.get_number("stepaltAllFilteredIdentity"), Some(1.0));
    assert_eq!(runtime.get_number("stepaltAllFilteredSteps"), Some(0.0));
    assert_eq!(runtime.get_number("stepaltAllFilteredCollected"), Some(1.0));
    assert_eq!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["kept"]
    );
    assert_clean(&runtime, "all-filtered stepalt opaque outer");
    runtime.clear_active();
}

#[test]
fn stepalt_exclusion_metadata_persists_across_turns_with_a_per_wrapper_cap() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 globalThis.stepaltSavedChain = new Pattern(
                   state => pure('kept').query(state), 1
                 );
                 return pure('active');
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("construct saved opaque chain");

    for turn in 0..64 {
        runtime
            .evaluate_score(
                &format!(
                    r#"(() => {{
                         const dropped = {{ turn: {turn} }};
                         globalThis.stepaltSavedChain = stepalt(
                           [pure(dropped).setSteps(-1)],
                           [stepaltSavedChain]
                         );
                         return pure('active');
                       }})()"#,
                ),
                &TranspileOptions::default(),
            )
            .unwrap_or_else(|error| panic!("turn {turn}: rebuild saved chain: {error}"));
        assert_eq!(
            runtime.active_exclusion_count(),
            turn + 1,
            "turn {turn}: active graph lost persistent same-turn exclusion provenance"
        );
        assert_eq!(
            runtime.wrapper_exclusion_count("stepaltSavedChain"),
            turn + 1,
            "turn {turn}: saved wrapper lost persistent exclusion provenance"
        );
    }

    runtime
        .evaluate_score("stepaltSavedChain", &TranspileOptions::default())
        .expect("publish saved chain after repeated turns");
    assert_eq!(runtime.active_exclusion_count(), 64);
    assert_eq!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["kept"]
    );
    assert_clean(&runtime, "stepalt exclusion lifecycle");
    runtime.clear_active();
}

#[test]
fn stepalt_checked_compression_handles_large_denominators_and_total_overflow() {
    rustel_core::reset_stepwise_entries_materialised();
    let representable =
        semantic_runtime("stepalt(pure('a').contract(1e30), pure('b').contract(1e30))");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);
    let haps = query_active(&representable, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        haps.iter().map(|hap| hap.value.show()).collect::<Vec<_>>(),
        ["a", "b"],
        "representable large-denominator compression changed"
    );
    let half = Fraction::new(1, 2);
    assert_eq!(haps[0].part.begin, Fraction::ZERO);
    assert_eq!(haps[0].part.end, half);
    assert_eq!(haps[0].whole.expect("first whole").begin, Fraction::ZERO);
    assert_eq!(haps[0].whole.expect("first whole").end, half);
    assert_eq!(haps[1].part.begin, half);
    assert_eq!(haps[1].part.end, Fraction::ONE);
    assert_eq!(haps[1].whole.expect("second whole").begin, half);
    assert_eq!(haps[1].whole.expect("second whole").end, Fraction::ONE);
    assert_clean(&representable, "representable stepalt compression");
    representable.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let overflow = semantic_runtime("stepalt(gap(1e38), gap(1e38))");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&overflow, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::NativeFraction {
            operation: "stepalt"
        }))
    ));
    assert_clean(&overflow, "stepalt checked total overflow");
    overflow.clear_active();
}

#[test]
fn stepalt_empty_sentinel_and_outer_group_count_boundaries_are_exact() {
    rustel_core::reset_stepwise_entries_materialised();
    let sentinel = semantic_runtime(
        r#"(() => {
             nothing._steps = 7;
             const result = stepalt();
             globalThis.stepaltEmptyIdentity = Number(result === nothing);
             globalThis.stepaltEmptySteps = Number(result._steps);
             return result;
           })()"#,
    );
    assert_eq!(sentinel.get_number("stepaltEmptyIdentity"), Some(1.0));
    assert_eq!(sentinel.get_number("stepaltEmptySteps"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_clean(&sentinel, "empty stepalt sentinel");
    sentinel.clear_active();

    let exact = semantic_runtime(
        r#"(() => {
             const groups = Array.from({ length: 16384 }, () => []);
             const result = stepalt(...groups);
             globalThis.stepaltOuterExactIdentity = Number(result === nothing);
             return result;
           })()"#,
    );
    assert_eq!(exact.get_number("stepaltOuterExactIdentity"), Some(1.0));
    assert!(
        query_active(&exact, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&exact, "exact stepalt outer group count");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             const groups = Array.from({ length: 16385 }, () => []);
             return stepalt(...groups);
           })()"#,
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "stepalt",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&over, "oversized stepalt outer group count");
    over.clear_active();
}

#[test]
fn polymeter_modern_and_legacy_native_branches_keep_timing_and_identities() {
    let runtime = semantic_runtime(
        r#"(() => {
             globalThis.polymeterEmptyIdentity = Number(polymeter() === silence);
             globalThis.polymeterFilteredIdentity = Number(
               polymeter('raw', 42, false) === silence
             );
             globalThis.polymeterZeroIdentity = Number(
               polymeter(nothing, gap(3)) === nothing
             );
             globalThis.polymeterAliases = Number(
               pm === polymeter && s_polymeter === polymeter
             );
             const modern = polymeter(sequence('a', 'b'), sequence('x', 'y', 'z'));
             const legacy = polymeter(['a', 'b'], ['x', 'y', 'z']);
             globalThis.polymeterModernSteps = Number(modern._steps);
             globalThis.polymeterLegacySteps = Number(legacy._steps);
             globalThis.polymeterLegacyEmptyDistinct = Number(
               polymeter([]) !== silence && polymeter([]) !== nothing
             );
             return stack(modern, legacy);
           })()"#,
    );
    assert_eq!(runtime.get_number("polymeterEmptyIdentity"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterFilteredIdentity"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterZeroIdentity"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterAliases"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterModernSteps"), Some(6.0));
    assert_eq!(runtime.get_number("polymeterLegacySteps"), Some(6.0));
    assert_eq!(
        runtime.get_number("polymeterLegacyEmptyDistinct"),
        Some(1.0)
    );
    let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(haps.len(), 16, "modern 12 plus legacy 4 haps per cycle");
    assert_clean(&runtime, "polymeter branch timing");
    runtime.clear_active();
}

#[test]
fn polymeter_dynamic_pace_guard_survives_instance_and_prototype_poisoning() {
    let runtime = semantic_runtime(
        r#"(() => {
             const source = pure('x').setSteps(2);
             let calls = 0;
             source.pace = function () { calls++; return this; };
             try {
               polymeter(source);
             } catch (error) {
               globalThis.polymeterInstancePaceError = error.name;
             }
             delete source.pace;

             const originalPace = Pattern.prototype.pace;
             try {
               Pattern.prototype.pace = function () { calls++; return this; };
               polymeter(source);
             } catch (error) {
               globalThis.polymeterPrototypePaceError = error.name;
             } finally {
               Pattern.prototype.pace = originalPace;
             }
             globalThis.polymeterPoisonedPaceCalls = calls;
             return silence;
           })()"#,
    );
    assert_eq!(
        runtime.get_string("polymeterInstancePaceError").as_deref(),
        Some("TypeError")
    );
    assert_eq!(
        runtime.get_string("polymeterPrototypePaceError").as_deref(),
        Some("TypeError")
    );
    assert_eq!(runtime.get_number("polymeterPoisonedPaceCalls"), Some(0.0));
    assert_clean(&runtime, "polymeter dynamic pace poison guard");
    runtime.clear_active();
}

#[test]
fn polymeter_legacy_nested_plan_preserves_parser_order_and_later_value_mutation() {
    let runtime = semantic_runtime(
        r#"(() => {
             const log = [];
             const later = ['c', 'd'];
             setStringParser(value => {
               log.push(value);
               if (value === 'a') later[1] = 'MUT';
               return pure(value);
             });
             const result = polymeter([['a', 'b']], [later]);
             globalThis.polymeterParserLog = log.join(',');
             globalThis.polymeterLaterValue = later[1];
             return result;
           })()"#,
    );
    assert_eq!(
        runtime.get_string("polymeterParserLog").as_deref(),
        Some("a,b,c,MUT")
    );
    assert_eq!(
        runtime.get_string("polymeterLaterValue").as_deref(),
        Some("MUT")
    );
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(values.iter().any(|value| value == "MUT"));
    assert_clean(&runtime, "nested polymeter parser order");
    runtime.clear_active();
}

#[test]
fn polymeter_legacy_rejects_parser_time_array_shape_changes_explicitly() {
    let runtime = semantic_runtime(
        r#"(() => {
             const later = ['x'];
             setStringParser(value => {
               if (value === 'a') later[0] = ['changed-shape'];
               return pure(value);
             });
             try {
               polymeter(['a'], later);
             } catch (error) {
               globalThis.polymeterShapeErrorName = error.name;
               globalThis.polymeterShapeErrorMessage = error.message;
             }
             return silence;
           })()"#,
    );
    assert_eq!(
        runtime.get_string("polymeterShapeErrorName").as_deref(),
        Some("TypeError")
    );
    assert_eq!(
        runtime.get_string("polymeterShapeErrorMessage").as_deref(),
        Some("polymeter Array nesting changed while reifying")
    );
    assert_clean(&runtime, "legacy polymeter shape-change boundary");
    runtime.clear_active();
}

#[test]
fn polymeter_legacy_singleton_nesting_is_iterative_at_the_exact_depth_cap() {
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(
        r#"(() => {
             globalThis.polymeterDeepParserHits = 0;
             setStringParser(value => {
               globalThis.polymeterDeepParserHits++;
               return pure(value);
             });
             let nested = 'a';
             for (let index = 0; index < 16384; index++) nested = [nested];
             return polymeter(nested);
           })()"#,
    );
    assert_eq!(exact.get_number("polymeterDeepParserHits"), Some(1.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 16_384);
    assert_eq!(
        query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "a"
    );
    assert_clean(&exact, "exact deeply nested polymeter");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             globalThis.polymeterDeepOverParserHits = 0;
             setStringParser(value => {
               globalThis.polymeterDeepOverParserHits++;
               return pure(value);
             });
             let nested = 'a';
             for (let index = 0; index < 16385; index++) nested = [nested];
             return polymeter(nested);
           })()"#,
    );
    assert_eq!(over.get_number("polymeterDeepOverParserHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&over, "oversized deeply nested polymeter");
    over.clear_active();
}

#[test]
fn polymeter_modern_and_legacy_resource_boundaries_are_exact_and_recover() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;

    rustel_core::reset_stepwise_entries_materialised();
    let modern_exact = semantic_runtime("polymeter(gap(1), gap(16383))");
    assert_eq!(
        modern_exact.active_pattern().expect("modern exact").steps,
        Some(Fraction::int(16_383))
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);
    assert!(query_active(&modern_exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&modern_exact, "exact modern polymeter");
    modern_exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let modern_over = semantic_runtime("polymeter(gap(1), gap(16384))");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&modern_over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_385,
            limit: actual,
        })) if actual == limit
    ));
    modern_over
        .evaluate_score("pure('modern-recovered')", &TranspileOptions::default())
        .unwrap();
    assert_eq!(
        query_active(&modern_over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "modern-recovered"
    );
    assert_clean(&modern_over, "modern polymeter recovery");
    modern_over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let legacy_exact = semantic_runtime("polymeter(Array(8192).fill(silence), [silence])");
    assert_eq!(
        legacy_exact.active_pattern().expect("legacy exact").steps,
        Some(Fraction::int(8_192))
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_193);
    assert!(query_active(&legacy_exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&legacy_exact, "exact legacy polymeter");
    legacy_exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let legacy_over = semantic_runtime(
        r#"(() => {
             globalThis.polymeterLegacyParserHits = 0;
             setStringParser(value => {
               globalThis.polymeterLegacyParserHits++;
               return pure(value);
             });
             return polymeter(Array(8193).fill('raw'), ['later']);
           })()"#,
    );
    assert_eq!(
        legacy_over.get_number("polymeterLegacyParserHits"),
        Some(0.0)
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&legacy_over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_386,
            limit: actual,
        })) if actual == limit
    ));
    assert_clean(&legacy_over, "oversized legacy polymeter");
    legacy_over.clear_active();
}

#[test]
fn query_time_polymeter_calls_share_the_stepwise_pool_and_recover_atomically() {
    let exact =
        semantic_runtime("fastcat(pure(0), pure(1)).polyBind(() => polymeter(gap(1), gap(8191)))");
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 4);
    assert_clean(&exact, "cumulative polymeter exact side");
    exact.clear_active();

    let over =
        semantic_runtime("fastcat(pure(0), pure(1)).polyBind(() => polymeter(gap(1), gap(8192)))");
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_386,
            limit: 16_384,
        }))
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);
    assert_clean(&over, "cumulative polymeter refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&over, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 2);
    assert_clean(&over, "cumulative polymeter recovery");
    over.clear_active();
}

#[test]
fn polymeter_zero_lcm_still_charges_every_retained_lane() {
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(
        r#"(() => {
             const result = polymeter(...Array(16384).fill(nothing));
             globalThis.polymeterZeroExactIdentity = Number(result === nothing);
             return result;
           })()"#,
    );
    assert_eq!(exact.get_number("polymeterZeroExactIdentity"), Some(1.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(
        query_active(&exact, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&exact, "exact zero-LCM polymeter");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime("polymeter(...Array(16385).fill(nothing))");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_385,
            limit: actual,
        })) if actual == limit
    ));
    assert_clean(&over, "oversized zero-LCM polymeter");
    over.clear_active();
}

#[test]
fn polymeter_shares_the_existing_stepwise_pool_with_exact_mixed_recovery() {
    let exact = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? gap(8000).shrink(0)
               : polymeter(gap(1), gap(8383))
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_002);
    assert_clean(&exact, "mixed polymeter exact side");
    exact.clear_active();

    let over = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? gap(8000).shrink(0)
               : polymeter(gap(1), gap(8384))
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_000);
    assert_clean(&over, "mixed polymeter refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&over, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_000);
    assert_clean(&over, "mixed polymeter recovery");
    over.clear_active();

    let legacy_exact = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? gap(16360).shrink(0)
               : polymeter(
                   Array(10).fill(silence),
                   [[silence, silence], silence, silence]
                 )
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&legacy_exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 16_375);
    assert_clean(&legacy_exact, "mixed legacy ceil exact side");
    legacy_exact.clear_active();

    let legacy_over = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? gap(16361).shrink(0)
               : polymeter(
                   Array(10).fill(silence),
                   [[silence, silence], silence, silence]
                 )
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&legacy_over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "polymeter",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 16_361);
    assert_clean(&legacy_over, "mixed legacy ceil refusal");
    legacy_over.clear_active();
}

#[test]
fn polymeter_lcm_overflow_is_a_typed_atomic_refusal_in_both_branches() {
    let steps = [
        "1000000000039",
        "1000000000061",
        "1000000000063",
        "1000000000091",
    ];
    let modern_source = format!(
        "polymeter({})",
        steps
            .iter()
            .map(|step| format!("gap({step})"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let legacy_source = format!(
        "polymeter({})",
        steps
            .iter()
            .map(|step| format!("[gap({step})]"))
            .collect::<Vec<_>>()
            .join(",")
    );
    for (label, source) in [("modern", modern_source), ("legacy", legacy_source)] {
        rustel_core::reset_stepwise_entries_materialised();
        let runtime = semantic_runtime(&source);
        assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
        assert!(matches!(
            query_active(&runtime, Fraction::ZERO, Fraction::ONE),
            Err(QueryError::Limit(QueryLimit::NativeFraction {
                operation: "polymeter"
            }))
        ));
        assert_clean(&runtime, label);
        runtime.clear_active();
    }
}

#[test]
fn legacy_polymeter_zero_target_contains_errors_from_emitting_lanes() {
    for source in ["polymeter([])", "polymeter([], [])"] {
        let runtime = semantic_runtime(source);
        assert_eq!(
            runtime.active_pattern().expect("empty legacy stack").steps,
            None
        );
        assert!(
            query_active(&runtime, Fraction::ZERO, Fraction::ONE)
                .unwrap()
                .is_empty()
        );
        assert_clean(&runtime, source);
        runtime.clear_active();
    }

    let silent = semantic_runtime("polymeter([], [silence])");
    assert_eq!(
        silent.active_pattern().expect("zero-target silence").steps,
        Some(Fraction::ONE)
    );
    assert!(
        query_active(&silent, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(rustel_core::take_query_error(), None);
    assert_clean(&silent, "silent zero-target lane");
    silent.clear_active();

    for source in ["polymeter([], ['a'])", "polymeter([], [], ['a'])"] {
        let runtime = semantic_runtime(source);
        assert_eq!(
            runtime
                .active_pattern()
                .expect("live zero-target lane")
                .steps,
            Some(Fraction::ONE)
        );
        let direct = runtime
            .active_pattern()
            .expect("live zero-target graph")
            .query(&rustel_core::State::new(rustel_core::TimeSpan::new(
                Fraction::ZERO,
                Fraction::ONE,
            )));
        assert!(
            direct.is_empty(),
            "the failed lane must emit no partial haps"
        );
        assert_eq!(rustel_core::take_query_error(), None);
        assert!(!rustel_core::query_interrupted());
        runtime.take_logs();
        assert!(
            query_active(&runtime, Fraction::ZERO, Fraction::ONE)
                .unwrap()
                .is_empty(),
            "polymeter's internal stack must discard the failed lane"
        );
        assert_eq!(rustel_core::take_query_error(), None);
        assert!(!rustel_core::query_interrupted());
        assert_eq!(
            rustel_core::take_query_callback_failure().as_deref(),
            Some("Division by Zero")
        );
        let logs = runtime.take_logs();
        assert!(
            logs.iter()
                .any(|line| line == "[query] error: Division by Zero"),
            "the bounded query must log the failed lane: {logs:?}"
        );
        // Score evaluation installs the host that records direct-query logs.
        runtime
            .evaluate_score(
                &format!(
                    r#"try {{
                         const haps = ({source}).query({{ span: {{ begin: 0, end: 1 }}, controls: {{}} }});
                         globalThis.polymeterZeroHapCount = haps.length;
                       }} catch (error) {{
                         globalThis.polymeterZeroErrorName = error.name;
                         globalThis.polymeterZeroErrorMessage = error.message;
                       }}
                       pure('recovered')"#
                ),
                &TranspileOptions::default(),
            )
            .unwrap();
        assert_eq!(runtime.get_number("polymeterZeroHapCount"), Some(0.0));
        assert_eq!(
            runtime.get_string("polymeterZeroErrorName").as_deref(),
            None
        );
        assert_eq!(
            runtime.get_string("polymeterZeroErrorMessage").as_deref(),
            None
        );
        assert_eq!(rustel_core::take_query_error(), None);
        assert_eq!(rustel_core::take_query_callback_failure(), None);
        assert!(!rustel_core::query_interrupted());
        let logs = runtime.take_logs();
        assert!(
            logs.iter()
                .any(|line| line == "[query] error: Division by Zero"),
            "the direct JavaScript query must log the failed lane: {logs:?}"
        );
        let recovered = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].value.as_str(), Some("recovered"));
        assert_clean(&runtime, source);
        runtime.clear_active();
    }

    let nested_nonzero = semantic_runtime("polymeter([[], []], ['a'])");
    assert!(query_active(&nested_nonzero, Fraction::ZERO, Fraction::ONE).is_ok());
    assert_clean(&nested_nonzero, "nested nonzero target");
    nested_nonzero.clear_active();
}

#[test]
fn polymeter_sentinels_suppress_discarded_values_but_positive_lanes_keep_owners() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 const filtered = { marker: 'filtered' };
                 const zero = { marker: 'zero' };
                 const kept = { marker: 'kept' };
                 globalThis.polymeterDiscardedRefs = [
                   new WeakRef(filtered), new WeakRef(zero)
                 ];
                 globalThis.polymeterKeptRef = new WeakRef(kept);

                 const filteredResult = polymeter(
                   pure(filtered).setSteps(undefined)
                 );
                 const zeroResult = polymeter(pure(zero).setSteps(0));
                 const positiveResult = polymeter(pure(kept).setSteps(2), silence);
                 globalThis.polymeterFilteredSentinel = Number(
                   filteredResult === silence
                 );
                 globalThis.polymeterZeroSentinel = Number(zeroResult === nothing);
                 globalThis.polymeterOwnershipResult = stack(
                   filteredResult,
                   zeroResult,
                   positiveResult,
                   new Pattern(state => pure('opaque').query(state), 1)
                 );
                 return polymeterOwnershipResult;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("construct polymeter sentinel ownership graph");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            r#"globalThis.polymeterDiscardedCollected = Number(
                 polymeterDiscardedRefs.every(ref => ref.deref() === undefined)
               );
               globalThis.polymeterPositiveAlive = Number(
                 polymeterKeptRef.deref()?.marker === 'kept'
               );"#,
        )
        .unwrap();
    assert_eq!(runtime.get_number("polymeterFilteredSentinel"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterZeroSentinel"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterDiscardedCollected"), Some(1.0));
    assert_eq!(runtime.get_number("polymeterPositiveAlive"), Some(1.0));
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(values.iter().any(|value| value.contains("marker:kept")));
    assert!(values.iter().any(|value| value == "opaque"));
    assert_clean(&runtime, "polymeter sentinel ownership");
    runtime.clear_active();
}

#[test]
fn polymeter_mixed_filtered_and_retained_sources_keep_only_explicit_owners() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 const dropped = { marker: 'mixed-drop' };
                 const kept = { marker: 'mixed-keep' };
                 globalThis.polymeterMixedDroppedRef = new WeakRef(dropped);
                 globalThis.polymeterMixedKeptRef = new WeakRef(kept);
                 const inner = polymeter(
                   pure(dropped).setSteps(undefined),
                   pure(kept).setSteps(2),
                   silence
                 );
                 globalThis.polymeterMixedResult = stack(
                   inner,
                   new Pattern(state => pure('opaque').query(state), 1)
                 );
                 return polymeterMixedResult;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("construct mixed polymeter ownership graph");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            r#"globalThis.polymeterMixedDroppedCollected = Number(
                 polymeterMixedDroppedRef.deref() === undefined
               );
               globalThis.polymeterMixedKeptAlive = Number(
                 polymeterMixedKeptRef.deref()?.marker === 'mixed-keep'
               );"#,
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("polymeterMixedDroppedCollected"),
        Some(1.0)
    );
    assert_eq!(runtime.get_number("polymeterMixedKeptAlive"), Some(1.0));
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values
            .iter()
            .any(|value| value.contains("marker:mixed-keep"))
    );
    assert!(values.iter().any(|value| value == "opaque"));
    assert_clean(&runtime, "mixed polymeter explicit ownership");
    runtime.clear_active();
}

#[test]
fn stepalt_thousands_of_unique_filtered_sidecars_remain_bounded() {
    let started = Instant::now();
    let runtime = semantic_runtime(
        r#"(() => {
             const sources = Array.from(
               { length: 4096 }, (_, i) => pure({ i }).setSteps(-1)
             );
             return stepalt(sources);
           })()"#,
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "4,096 unique filtered sidecars exposed quadratic ownership bookkeeping"
    );
    assert_eq!(runtime.active_exclusion_count(), 0);
    assert!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&runtime, "unique filtered stepalt sidecars");
    runtime.clear_active();
}

#[test]
fn raw_set_keeps_direct_composer_surface_routing_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._set;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_set'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'set'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public set getter descriptor');
                   const pair = ['_set', 'set'];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => pair.includes(name)).join('|')
                       === '_set|set', 'prototype pair order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => pair.includes(name)).join('|')
                       === '_set', 'enumerable pair order');
                   check(!('_set' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_set'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [1]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-set-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const expected = { receiver };
                     const source = new Proxy({}, {
                       get() { throw new Error('mapper inspected source'); },
                     });
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return mapper(source);
                       },
                     });
                     try {
                       check(apply(raw, receiver, [expected]) === expected,
                         'primitive receiver result changed');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap';
                   let gets = 0;
                   let calls = 0;
                   let maps = 0;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('lexical mapper read source'); },
                     ownKeys() {
                       throw new Error('lexical mapper enumerated source');
                     },
                   });
                   const target = {};
                   const helper = new Proxy(function () {}, {
                     apply(_helper, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.prototype.hasOwnProperty.call(
                           mapper, 'prototype'
                         ), 'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       maps++;
                       return mapper(source);
                     },
                   });
                   Object.defineProperty(target, 'fmap', {
                     configurable: true,
                     get() { gets++; return helper; },
                   });
                   const exact = new Proxy({ marker: 'exact' }, {});
                   check(apply(raw, target, []) === undefined,
                     'zero-argument value');
                   check(apply(raw, target, [exact]) === exact,
                     'one-argument exact value');
                   check(apply(raw, target, [exact, {}, 'ignored']) === exact,
                     'extras changed direct value capture');
                   check(gets === 3 && calls === 3 && maps === 3,
                     'dynamic fmap phase count');
                   check(new Set(mappers).size === 3,
                     'mapper callback was reused across calls');

                   phase = 'delayed-closure';
                   let delayedMapper;
                   const delayedTerminal = {};
                   const delayedTarget = {
                     fmap(mapper) {
                       delayedMapper = mapper;
                       return delayedTerminal;
                     },
                   };
                   const delayedValue = { version: 1 };
                   check(apply(raw, delayedTarget, [delayedValue])
                     === delayedTerminal, 'delayed terminal');
                   delayedValue.version = 2;
                   check(delayedMapper(new Proxy({}, {
                     get() { throw new Error('delayed mapper read source'); },
                   })) === delayedValue && delayedValue.version === 2,
                   'delayed value capture');

                   phase = 'cutoffs-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [exact]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [exact]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-set-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return terminal; },
                     }, [exact]), terminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let constructorTerminal;
                   let constructorThis;
                   let constructorMapped;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper('source');
                     return constructorTerminal;
                   };
                   const constructorValue = {};
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(
                     raw, [constructorValue]
                   );
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === constructorValue,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(
                     raw, [constructorValue]
                   );
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === constructorValue,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper('alternate') === constructorValue,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [constructorValue], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'dynamic-native-and-poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper('replacement-source');
                     },
                   });
                   const replacementValue = {};
                   check(apply(raw, pure(0), [replacementValue])
                     === replacementValue && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );

                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned set surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_set', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   Object.defineProperty(Pattern.prototype, 'set', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   globalThis.set = poison;
                   rustelScope.set = poison;
                   const poisonValue = {};
                   check(apply(raw, {
                     fmap(mapper) { return mapper('source'); },
                   }, [poisonValue]) === poisonValue && poisonHits === 0,
                   'saved raw routed through poisoned set surface');

                   globalThis.rawSetSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawSetSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSetSurfaceError"), None);
    assert_eq!(runtime.get_number("rawSetSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw set surface and direct routing");
}

#[test]
fn raw_set_keeps_lazy_exact_values_steps_tags_and_parser_cutoffs() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });

                   phase = 'lazy-exact-proxy';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   let mapperShape = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     mapperShape = Number(mapper.name === ''
                       && mapper.length === 1
                       && !Object.prototype.hasOwnProperty.call(
                         mapper, 'prototype'
                       ));
                     const counted = new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         mapperCalls++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     });
                     return Reflect.apply(originalFmap, this, [counted]);
                   };
                   const source = pure(0).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const exact = new Proxy({ marker: 'before' }, {});
                   const result = source._set(exact);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0 && mapperShape === 1,
                     'mapper was eager or malformed');
                   check(result !== source, 'result identity');
                   check(result.query !== sourceQuery,
                     'result query identity');
                   check(result._steps !== sourceSteps
                     && Number(result._steps) === Number(sourceSteps),
                     `result steps:${result._steps}:${sourceSteps}`);
                   exact.marker = 'after';
                   const first = result.query(state(0, 1));
                   check(first.length === 1 && first[0].value === exact
                     && first[0].value.marker === 'after'
                     && mapperCalls === 1,
                     'first exact Proxy query');
                   const second = result.query(state(1, 2));
                   check(second.length === 1 && second[0].value === exact
                     && mapperCalls === 2,
                     'second exact Proxy query');

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   const location = { start: 7, end: 8 };
                   tagged.__pure_loc = location;
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._set('tagged-result');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literal = pure('source')._set('literal-value');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalHaps = literal.query(state(0, 1));
                   check(parserCalls === 0 && literalHaps.length === 1
                     && literalHaps[0].value === 'literal-value',
                     'parser reached during query');
                   setStringParser(undefined);

                   phase = 'exact-value-matrix';
                   const fn = function exactFunction() {};
                   const values = [
                     undefined, null, -0, 'exact-string', fn
                   ];
                   for (const value of values) {
                     const haps = pure('source')._set(value).query(
                       state(0, 1)
                     );
                     check(haps.length === 1
                       && Object.is(haps[0].value, value),
                       'exact value matrix changed');
                   }

                   phase = 'source-throw-cutoff';
                   let sourceCalls = 0;
                   let mappedCalls = 0;
                   const sentinel = { marker: 'source-query-throw' };
                   const throwingSource = new Pattern(() => {
                     sourceCalls++;
                     throw sentinel;
                   });
                   const throwingOriginal = Pattern.prototype.fmap;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(throwingOriginal, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mappedCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const stopped = throwingSource._set('unreachable');
                   Pattern.prototype.fmap = throwingOriginal;
                   const stoppedHaps = stopped.query(state(0, 1));
                   check(stoppedHaps.length === 0 && sourceCalls === 1
                     && mappedCalls === 0,
                     'source throw did not stop mapping');

                   globalThis.rawSetSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawSetSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSetSemanticError"), None);
    assert_eq!(runtime.get_number("rawSetSemanticOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "raw set source throw did not retain its query error"
    );
    assert_clean(&runtime, "raw set lazy exact-value semantics");

    // Own/custom `query` replacement, exact Pattern/Symbol/BigInt values, and
    // tagged-native callable execution (including `rev`) remain outside this
    // direct native-fmap checkpoint; `_set` deliberately follows the
    // receiver's current `fmap`.
}

#[test]
fn raw_set_retains_values_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-set-kept' };
             let sourceValue = { marker: 'raw-set-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-set-extra' };
             globalThis.rawSetKeptRef = new WeakRef(kept);
             globalThis.rawSetSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawSetDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._set, source, [kept, extra]
             );
             kept = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "captured JavaScript value lost its query owner"
    );
    assert_eq!(
        owned.active_pattern().expect("raw set owned graph").steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawSetKeptAlive = Number(
                 rawSetKeptRef.deref()?.marker === 'raw-set-kept'
               );
               globalThis.rawSetSourceValueAlive = Number(
                 rawSetSourceValueRef.deref()?.marker === 'raw-set-source'
               );
               globalThis.rawSetTransientsPruned = Number(
                 rawSetDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawSetKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSetSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSetTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(
        values
            .iter()
            .all(|hap| matches!(&hap.value, Value::Object(_))),
        "outer query left JavaScript-owned values opaque"
    );
    assert_clean(&owned, "raw set captured-value ownership");
    owned.clear_active();
    drop(owned);
    assert!(
        values
            .iter()
            .all(|hap| hap.value.show().contains("marker:raw-set-kept"))
    );

    let terminal = semantic_runtime(
        r#"(() => {
             let target = {};
             let value = { marker: 'raw-set-unused-value' };
             let extra = { marker: 'raw-set-terminal-extra' };
             let helper = function () { return pure('raw-set-terminal'); };
             target.fmap = helper;
             globalThis.rawSetTerminalDroppedRefs = [
               new WeakRef(target), new WeakRef(value),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._set, target, [value, extra]
             );
             target = value = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawSetTerminalTransientsPruned = Number(
                 rawSetTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawSetTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-set-terminal"
    );
    assert_clean(&terminal, "raw set custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(0).fast(4).setSteps(5)._set(1)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw set charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw set charged query-time stepwise work"
    );
    assert_clean(&direct, "raw set direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._set('mapped');
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw set consumed shared stepwise allowance"
    );
    assert_clean(&exact, "raw set nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._set('mapped');
             return gap({}).shrink(0);
           }})"#,
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw set nested refusal attribution");
    over.clear_active();

    // Aggregate Pattern-valued lineage, exotic `fmap`, Rust/CLI Proxy
    // materialisation and trap phase, density/general allocation bounds,
    // exact V8 stacks/toString/absolute order, and native own/custom-query
    // replacement remain explicit residuals. `_set` itself adds no resource
    // operation or shared-stepwise charge.
}

#[test]
fn raw_keep_keeps_direct_composer_surface_routing_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._keep;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_keep'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'keep'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public keep getter descriptor');
                   const local = ['_set', 'set', '_keep', 'keep'];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|set|_keep|keep',
                     'prototype composer order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|_keep', 'enumerable composer order');
                   check(!('_keep' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_keep'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [{ ignored: true }]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-keep-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const source = new Proxy({}, {
                       get() { throw new Error('mapper inspected source'); },
                     });
                     const ignored = new Proxy({}, {
                       get() { throw new Error('mapper inspected value'); },
                       ownKeys() {
                         throw new Error('mapper enumerated value');
                       },
                     });
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return mapper(source);
                       },
                     });
                     try {
                       check(apply(raw, receiver, [ignored]) === source,
                         'primitive receiver mapper did not keep x');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap';
                   let gets = 0;
                   let calls = 0;
                   let maps = 0;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('lexical mapper read source'); },
                     ownKeys() {
                       throw new Error('lexical mapper enumerated source');
                     },
                   });
                   const target = {};
                   const helper = new Proxy(function () {}, {
                     apply(_helper, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.prototype.hasOwnProperty.call(
                           mapper, 'prototype'
                         ), 'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       maps++;
                       return mapper(source);
                     },
                   });
                   Object.defineProperty(target, 'fmap', {
                     configurable: true,
                     get() { gets++; return helper; },
                   });
                   let opPoisonHits = 0;
                   const opPoison = () => {
                     opPoisonHits++;
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = opPoison;
                   target.op = opPoison;
                   const ignored = new Proxy({ marker: 'ignored' }, {
                     get() { throw new Error('ignored value was read'); },
                     ownKeys() {
                       throw new Error('ignored value was enumerated');
                     },
                   });
                   check(apply(raw, target, []) === source,
                     'zero-argument mapper changed x');
                   check(apply(raw, target, [ignored]) === source,
                     'one-argument mapper selected value');
                   check(apply(raw, target, [ignored, {}, 'extra'])
                     === source, 'extras retargeted direct composer');
                   check(gets === 3 && calls === 3 && maps === 3,
                     'dynamic fmap phase count');
                   check(new Set(mappers).size === 3,
                     'mapper callback was reused across calls');
                   check(opPoisonHits === 0,
                     'mapper resolved op outside its lexical factory');

                   phase = 'delayed-closure';
                   let delayedMapper;
                   const delayedTerminal = {};
                   const delayedTarget = {
                     fmap(mapper) {
                       delayedMapper = mapper;
                       return delayedTerminal;
                     },
                   };
                   const delayedIgnored = new Proxy({}, {
                     get() { throw new Error('delayed value was read'); },
                   });
                   check(apply(raw, delayedTarget, [delayedIgnored])
                     === delayedTerminal, 'delayed terminal');
                   const delayedSource = {};
                   check(delayedMapper(delayedSource, 'extra')
                     === delayedSource, 'delayed mapper did not keep x');

                   phase = 'cutoffs-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [ignored]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [ignored]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-keep-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return terminal; },
                     }, [ignored]), terminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [ignored]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorTerminal;
                   let constructorThis;
                   let constructorMapped;
                   const constructorSource = {};
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper(constructorSource);
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [ignored]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === constructorSource,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [ignored]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === constructorSource,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(constructorSource) === constructorSource,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [ignored], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'dynamic-native-and-poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   const replacementSource = {};
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper(replacementSource);
                     },
                   });
                   check(apply(raw, pure(0), [ignored])
                     === replacementSource && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );

                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned keep surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_keep', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   Object.defineProperty(Pattern.prototype, 'keep', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   globalThis.keep = poison;
                   rustelScope.keep = poison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(replacementSource); },
                   }, [ignored]) === replacementSource && poisonHits === 0,
                   'saved raw routed through poisoned keep surface');

                   globalThis.rawKeepSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepSurfaceError"), None);
    assert_eq!(runtime.get_number("rawKeepSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw keep surface and direct routing");
}

#[test]
fn raw_keep_keeps_lazy_source_values_steps_tags_and_explicit_residuals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-exact-proxy';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   let mapperShape = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     mapperShape = Number(mapper.name === ''
                       && mapper.length === 1
                       && !Object.prototype.hasOwnProperty.call(
                         mapper, 'prototype'
                       ));
                     const counted = new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         mapperCalls++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     });
                     return Reflect.apply(originalFmap, this, [counted]);
                   };
                   const exact = new Proxy({ marker: 'before' }, {});
                   const source = pure(exact).fast(2).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const ignored = new Proxy({ marker: 'ignored' }, {
                     get() { throw new Error('ignored value was read'); },
                     ownKeys() {
                       throw new Error('ignored value was enumerated');
                     },
                   });
                   const result = source._keep(ignored);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0 && mapperShape === 1,
                     'mapper was eager or malformed');
                   check(result !== source, 'result identity');
                   check(result.query !== sourceQuery,
                     'result query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     `result steps:${result._steps}:${sourceSteps}`);
                   exact.marker = 'after';
                   const sourceHaps = source.query(state(0, 1));
                   const resultHaps = result.query(state(0, 1));
                   check(resultHaps.length === sourceHaps.length
                     && resultHaps.length === 2
                     && resultHaps.every(hap => hap.value === exact
                       && hap.value.marker === 'after')
                     && resultHaps.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === resultHaps.length,
                     'lazy exact source/timing query');

                   phase = 'step-matrix';
                   const stepSources = [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ];
                   for (const stepSource of stepSources) {
                     const stepResult = stepSource._keep('ignored');
                     if (stepSource._steps === undefined) {
                       check(stepResult._steps === undefined,
                         'no-step source gained steps');
                     } else {
                       check(stepResult._steps !== stepSource._steps
                         && stepResult._steps.show()
                           === stepSource._steps.show(),
                         'defined steps were not freshly preserved');
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 7, end: 8 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._keep('ignored');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   const parserSource = pure('source');
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literal = parserSource._keep('ignored-literal');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalHaps = literal.query(state(0, 1));
                   check(parserCalls === 0 && literalHaps.length === 1
                     && literalHaps[0].value === 'source',
                     'parser reached during query');
                   setStringParser(undefined);

                   phase = 'exact-source-matrix';
                   const fn = function exactFunction() {};
                   const object = { marker: 'exact-object' };
                   const proxy = new Proxy({ marker: 'exact-proxy' }, {});
                   const array = [object, proxy];
                   const values = [
                     undefined, null, -0, 'exact-string', fn, object,
                     proxy, array
                   ];
                   for (const value of values) {
                     const haps = pure(value)._keep('ignored').query(
                       state(0, 1)
                     );
                     check(haps.length === 1
                       && Object.is(haps[0].value, value),
                       'exact source value changed');
                   }

                   phase = 'ignored-pattern-cutoff';
                   let ignoredPatternQueries = 0;
                   const ignoredPattern = new Pattern(() => {
                     ignoredPatternQueries++;
                     throw new Error('ignored Pattern was queried');
                   });
                   const ignoredPatternHaps = pure('kept-source')
                     ._keep(ignoredPattern).query(state(0, 1));
                   check(ignoredPatternHaps.length === 1
                     && ignoredPatternHaps[0].value === 'kept-source'
                     && ignoredPatternQueries === 0,
                     'ignored Pattern was inspected or queried');

                   phase = 'unsupported-identity-residuals';
                   for (const value of [
                     1n, Symbol('raw-keep-symbol'), rev
                   ]) {
                     const haps = pure(value)._keep('ignored').query(
                       state(0, 1)
                     );
                     check(haps.length === 1
                       && !Object.is(haps[0].value, value),
                       'unsupported identity unexpectedly matched');
                   }
                   phase = 'query-reassignment-residual';
                   const nativeSource = pure('native-source');
                   const reassigned = pure('pinned-reassigned');
                   nativeSource.query = reassigned.query;
                   const reassignedHaps = nativeSource._keep().query(
                     state(0, 1)
                   );
                   check(reassignedHaps.length === 1
                     && reassignedHaps[0].value === 'native-source',
                     'native own-query residual changed');

                   phase = 'source-throw-cutoff';
                   let sourceCalls = 0;
                   let mappedCalls = 0;
                   const sentinel = { marker: 'source-query-throw' };
                   const throwingSource = new Pattern(() => {
                     sourceCalls++;
                     throw sentinel;
                   });
                   const throwingOriginal = Pattern.prototype.fmap;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(throwingOriginal, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mappedCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const stopped = throwingSource._keep('ignored');
                   Pattern.prototype.fmap = throwingOriginal;
                   const stoppedHaps = stopped.query(state(0, 1));
                   check(stoppedHaps.length === 0 && sourceCalls === 1
                     && mappedCalls === 0,
                     'source throw did not stop mapping');

                   globalThis.rawKeepSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepSemanticError"), None);
    assert_eq!(runtime.get_number("rawKeepSemanticOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "raw keep source throw did not retain its query error"
    );
    assert_clean(&runtime, "raw keep lazy source-value semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const patternHaps = pure(patternValue)
                   ._keep('ignored').query({
                     span: { begin: 0, end: 1 }, controls: {},
                   });
                 if (patternHaps.length !== 0) {
                   throw new Error('direct Pattern-valued residual changed');
                 }
                 globalThis.rawKeepPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawKeepPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "direct Pattern-valued residual lost its join-only query error"
    );

    // strudel.cc follows an own-reassigned `query` and preserves exact
    // BigInt, Symbol, tagged-native callable, and Pattern identities. The
    // executable witnesses above deliberately pin the first native gaps;
    // exotic `fmap` and later query replacement stay residual rather than
    // broadening this direct-composer checkpoint.
}

#[test]
fn raw_keep_retains_mapper_inputs_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let ignored = { marker: 'raw-keep-captured' };
             let sourceValue = { marker: 'raw-keep-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-keep-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawKeepMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawKeepCapturedRef = new WeakRef(ignored);
             globalThis.rawKeepSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawKeepDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keep, source, [ignored, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             ignored = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw keep mapper lost its JavaScript query owner"
    );
    assert_eq!(
        owned.active_pattern().expect("raw keep owned graph").steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawKeepCapturedAlive = Number(
                 rawKeepCapturedRef.deref()?.marker === 'raw-keep-captured'
               );
               globalThis.rawKeepSourceValueAlive = Number(
                 rawKeepSourceValueRef.deref()?.marker === 'raw-keep-source'
               );
               globalThis.rawKeepMapperAlive = Number(
                 typeof rawKeepMapperRef.deref() === 'function'
               );
               globalThis.rawKeepTransientsPruned = Number(
                 rawKeepDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawKeepCapturedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepMapperAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(
        values
            .iter()
            .all(|hap| matches!(&hap.value, Value::Object(_))),
        "outer query left JavaScript-owned source values opaque"
    );
    assert_clean(&owned, "raw keep captured mapper ownership");
    owned.clear_active();
    drop(owned);
    assert!(
        values
            .iter()
            .all(|hap| hap.value.show().contains("marker:raw-keep-source"))
    );

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-keep-js-terminal' };
             let target = {};
             let ignored = { marker: 'raw-keep-js-ignored' };
             let extra = { marker: 'raw-keep-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawKeepJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawKeepJsKeptRef = new WeakRef(kept);
             globalThis.rawKeepJsDroppedRefs = [
               new WeakRef(target), new WeakRef(ignored),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keep, target, [ignored, extra]
             );
             kept = target = ignored = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    assert_eq!(
        js_terminal
            .active_pattern()
            .expect("raw keep JavaScript terminal")
            .steps,
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawKeepJsKeptAlive = Number(
                 rawKeepJsKeptRef.deref()?.marker
                   === 'raw-keep-js-terminal'
               );
               globalThis.rawKeepJsMapperPruned = Number(
                 rawKeepJsMapperRef.deref() === undefined
               );
               globalThis.rawKeepJsTransientsPruned = Number(
                 rawKeepJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawKeepJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawKeepJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawKeepJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-keep-js-terminal"
    );
    assert_clean(&js_terminal, "raw keep JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let target = {};
             let ignored = { marker: 'raw-keep-unused-value' };
             let extra = { marker: 'raw-keep-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawKeepTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-keep-terminal');
             };
             target.fmap = helper;
             globalThis.rawKeepTerminalDroppedRefs = [
               new WeakRef(target), new WeakRef(ignored),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keep, target, [ignored, extra]
             );
             target = ignored = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawKeepTerminalTransientsPruned = Number(
                 rawKeepTerminalMapperRef.deref() === undefined
                 && rawKeepTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawKeepTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-keep-terminal"
    );
    assert_clean(&terminal, "raw keep custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(0).fast(4).setSteps(5)._keep('ignored')");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw keep charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw keep charged query-time stepwise work"
    );
    assert_clean(&direct, "raw keep direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure('kept')._keep('ignored');
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw keep consumed shared stepwise allowance"
    );
    assert_clean(&exact, "raw keep nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure('kept')._keep('ignored');
             return gap({}).shrink(0);
           }})"#,
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw keep nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure('kept')._keep('ignored');
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw keep nested recovery");
    recovery.clear_active();

    // strudel.cc retains the source wrapper through its dynamic query
    // closure, while the native graph retains the source value but prunes the
    // wrapper. Pattern-valued identity, aggregate lineage, exotic `fmap`,
    // density/general allocation, exact V8 stacks/toString/absolute order,
    // and broad deadline/cancellation bounds remain residuals. `_keep` adds no
    // resource operation or shared-stepwise charge.
}

#[test]
fn raw_keepif_keeps_direct_surface_truthiness_and_dynamic_routing() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._keepif;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_keepif'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'keepif'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   const lengthDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'length'
                   );
                   const nameDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'name'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable,
                     'name/length descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public keepif getter descriptor');
                   const local = [
                     '_set', 'set', '_keep', 'keep', '_keepif', 'keepif'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|set|_keep|keep|_keepif|keepif',
                     'prototype composer order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|_keep|_keepif',
                     'enumerable composer order');
                   check(!('_keepif' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_keepif'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [true]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-keepif-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const source = {};
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(source), mapper(source, 'extra')];
                       },
                     });
                     try {
                       const truthy = apply(raw, receiver, [true]);
                       const falsey = apply(raw, receiver, [false]);
                       check(truthy.length === 2
                         && truthy.every(value => value === source)
                         && falsey.length === 2
                         && falsey.every(value => value === undefined),
                         'primitive receiver mapper values');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap-and-toboolean';
                   let gets = 0;
                   let calls = 0;
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('mapper inspected source'); },
                     ownKeys() {
                       throw new Error('mapper enumerated source');
                     },
                   });
                   const target = {};
                   const helper = new Proxy(function () {}, {
                     apply(_helper, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.prototype.hasOwnProperty.call(
                           mapper, 'prototype'
                         ), 'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper(source, 'ignored-mapper-extra');
                     },
                   });
                   Object.defineProperty(target, 'fmap', {
                     configurable: true,
                     get() { gets++; return helper; },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('condition coercion hook read');
                       },
                     });
                   }
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   const falsey = [
                     undefined, null, false, 0, -0, 0n, NaN, ''
                   ];
                   const truthy = [
                     true, 1, -1, 1n, '0', Symbol('truthy'), hostile,
                     revoked
                   ];
                   for (const condition of falsey) {
                     check(apply(raw, target, [condition]) === undefined,
                       'falsey condition kept mapper input');
                   }
                   for (const condition of truthy) {
                     check(apply(raw, target, [condition]) === source,
                       'truthy condition discarded mapper input');
                   }
                   check(gets === falsey.length + truthy.length
                     && calls === gets && hookReads === 0
                     && proxyTraps === 0,
                     'dynamic fmap/ToBoolean phase count');
                   check(new Set(mappers).size === mappers.length,
                     'mapper callback was reused across calls');
                   check(apply(raw, target, [true, {}, 'extra']) === source,
                     'extra arguments affected direct composer');
                   check(apply(raw, target, []) === undefined,
                     'zero-argument condition was not undefined');

                   phase = 'delayed-closures';
                   const delayed = [];
                   const delayedTerminal = {};
                   const delayedTarget = {
                     fmap(mapper) {
                       delayed.push(mapper);
                       return delayedTerminal;
                     },
                   };
                   check(apply(raw, delayedTarget, [true])
                     === delayedTerminal
                     && apply(raw, delayedTarget, [false])
                       === delayedTerminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](source) === source
                     && delayed[1](source) === undefined,
                     'delayed mapper capture/freshness');

                   phase = 'cutoffs-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [hostile]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel && hookReads === 0,
                     'fmap getter cutoff/throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [hostile]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel && hookReads === 0,
                     'fmap call cutoff/throw identity');
                   const symbol = Symbol('raw-keepif-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return terminal; },
                     }, [true]), terminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [true]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorTerminal;
                   let constructorThis;
                   let constructorMapped;
                   const constructorSource = {};
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper(constructorSource);
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [false]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === undefined,
                     'object constructor terminal/false mapper');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [true]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === constructorSource,
                     'primitive constructor fallback/true mapper');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(constructorSource) === constructorSource,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [true], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'dynamic-native-and-poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   const replacementSource = {};
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper(replacementSource);
                     },
                   });
                   check(apply(raw, pure(0), [true])
                     === replacementSource && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned keepif surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_keepif', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'keepif', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.keepif = surfacePoison;
                   rustelScope.keepif = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(replacementSource); },
                   }, [true]) === replacementSource && poisonHits === 0,
                   'saved raw routed through poisoned keepif surface');

                   globalThis.rawKeepifSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepifSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepifSurfaceError"), None);
    assert_eq!(runtime.get_number("rawKeepifSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw keepif surface and direct routing");
}

#[test]
fn raw_keepif_preserves_undefined_haps_timing_metadata_and_explicit_residuals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-false-discriminator';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     const counted = new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         mapperCalls++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     });
                     return Reflect.apply(originalFmap, this, [counted]);
                   };
                   const exact = new Proxy({ marker: 'before' }, {});
                   const source = pure(exact).fast(2).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const rawFalse = source._keepif(false);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0,
                     'false mapper was eager');
                   check(rawFalse !== source
                     && rawFalse.query !== sourceQuery,
                     'raw false result/query identity');
                   check(rawFalse._steps !== sourceSteps
                     && rawFalse._steps.show() === sourceSteps.show(),
                     'raw false steps were not freshly preserved');
                   exact.marker = 'after';
                   const sourceHaps = source.query(state(0, 1));
                   const falseHaps = rawFalse.query(state(0, 1));
                   check(falseHaps.length === sourceHaps.length
                     && falseHaps.length === 2
                     && falseHaps.every(hap => hap.value === undefined)
                     && falseHaps.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === falseHaps.length,
                     'raw false removed haps or changed timing');
                   const publicFalse = source.keepif(false).query(state(0, 1));
                   check(publicFalse.length === 0,
                     'public false did not remove undefined haps');

                   phase = 'lazy-true';
                   let trueCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           trueCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const rawTrue = source._keepif(true);
                   Pattern.prototype.fmap = originalFmap;
                   check(trueCalls === 0, 'true mapper was eager');
                   const trueHaps = rawTrue.query(state(0, 1));
                   check(trueHaps.length === sourceHaps.length
                     && trueHaps.every(hap => hap.value === exact
                       && hap.value.marker === 'after')
                     && trueHaps.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && trueCalls === trueHaps.length,
                     'raw true changed values or timing');

                   phase = 'step-matrix';
                   for (const condition of [false, true]) {
                     const stepSources = [
                       pure('default'),
                       pure('none').setSteps(undefined),
                       pure('zero').setSteps(0),
                       pure('seven').setSteps(7),
                     ];
                     for (const stepSource of stepSources) {
                       const stepResult = stepSource._keepif(condition);
                       if (stepSource._steps === undefined) {
                         check(stepResult._steps === undefined,
                           'no-step source gained steps');
                       } else {
                         check(stepResult._steps !== stepSource._steps
                           && stepResult._steps.show()
                             === stepSource._steps.show(),
                           'defined steps were not freshly preserved');
                       }
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 9, end: 10 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   for (const condition of [false, true]) {
                     const tagResult = tagged._keepif(condition);
                     check(!Object.prototype.hasOwnProperty.call(
                       tagResult, '__pure'
                     ) && !Object.prototype.hasOwnProperty.call(
                       tagResult, '__pure_loc'
                     ), 'structural tags survived');
                   }

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literalTrue = pure('source')._keepif('truthy');
                   const literalFalse = pure('source')._keepif('');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalTrueHaps = literalTrue.query(state(0, 1));
                   const literalFalseHaps = literalFalse.query(state(0, 1));
                   check(parserCalls === 0
                     && literalTrueHaps.length === 1
                     && literalTrueHaps[0].value === 'source'
                     && literalFalseHaps.length === 1
                     && literalFalseHaps[0].value === undefined,
                     'parser reached or string truthiness changed at query');
                   setStringParser(undefined);

                   phase = 'exact-source-matrix';
                   const fn = function exactFunction() {};
                   const object = { marker: 'exact-object' };
                   const proxy = new Proxy({ marker: 'exact-proxy' }, {});
                   const array = [object, proxy];
                   for (const value of [
                     undefined, null, -0, 'exact-string', fn, object,
                     proxy, array
                   ]) {
                     const haps = pure(value)._keepif(true).query(state(0, 1));
                     check(haps.length === 1
                       && Object.is(haps[0].value, value),
                       'exact source value changed');
                   }

                   phase = 'pattern-control-cutoff';
                   let controlQueries = 0;
                   const patternControl = new Pattern(() => {
                     controlQueries++;
                     throw new Error('Pattern control was queried');
                   });
                   const controlled = pure('kept-source')
                     ._keepif(patternControl).query(state(0, 1));
                   check(controlled.length === 1
                     && controlled[0].value === 'kept-source'
                     && controlQueries === 0,
                     'Pattern control was coerced or queried');

                   phase = 'unsupported-identity-residuals';
                   for (const value of [
                     1n, Symbol('raw-keepif-symbol'), rev
                   ]) {
                     const haps = pure(value)._keepif(true).query(state(0, 1));
                     check(haps.length === 1
                       && !Object.is(haps[0].value, value),
                       'unsupported identity unexpectedly matched');
                   }

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure('native-source');
                   const reassigned = pure('pinned-reassigned');
                   nativeSource.query = reassigned.query;
                   const reassignedHaps = nativeSource._keepif(true).query(
                     state(0, 1)
                   );
                   check(reassignedHaps.length === 1
                     && reassignedHaps[0].value === 'native-source',
                     'native own-query residual changed');

                   phase = 'false-source-throw-cutoff';
                   let sourceCalls = 0;
                   let mappedCalls = 0;
                   const sentinel = { marker: 'source-query-throw' };
                   const throwingSource = new Pattern(() => {
                     sourceCalls++;
                     throw sentinel;
                   });
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mappedCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const stopped = throwingSource._keepif(false);
                   Pattern.prototype.fmap = originalFmap;
                   const stoppedHaps = stopped.query(state(0, 1));
                   check(stoppedHaps.length === 0 && sourceCalls === 1
                     && mappedCalls === 0,
                     'false condition skipped source or mapped after throw');

                   globalThis.rawKeepifSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepifSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepifSemanticError"), None);
    assert_eq!(runtime.get_number("rawKeepifSemanticOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "raw keepif false source throw did not retain its query error"
    );
    assert_clean(&runtime, "raw keepif lazy condition semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern-false');
                 const haps = pure(patternValue)._keepif(false).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('false Pattern-source residual changed');
                 }
                 globalThis.rawKeepifFalsePatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("rawKeepifFalsePatternResidualOkay"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "false Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern-true');
                 const haps = pure(patternValue)._keepif(true).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('true Pattern-source residual changed');
                 }
                 globalThis.rawKeepifTruePatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("rawKeepifTruePatternResidualOkay"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "true Pattern-source residual lost its join-only query error"
    );

    // strudel.cc preserves one undefined hap for a false Pattern source
    // and one exact Pattern-valued hap for a true source. Native joins before
    // the mapper in both cases, as the executable residual witnesses pin.
    // Exact BigInt, Symbol, tagged-native callable, own/custom-query, exotic
    // `fmap`, and later query replacement remain bounded residuals.
}

#[test]
fn raw_keepif_retains_controls_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let controlQueries = 0;
             let condition = new Pattern(() => {
               controlQueries++;
               throw new Error('raw keepif condition was queried');
             });
             let sourceValue = { marker: 'raw-keepif-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-keepif-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawKeepifMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawKeepifControlQueries = () => controlQueries;
             globalThis.rawKeepifControlRef = new WeakRef(condition);
             globalThis.rawKeepifSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawKeepifDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keepif, source, [condition, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             condition = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw keepif mapper lost its JavaScript query owner"
    );
    assert_eq!(
        owned
            .active_pattern()
            .expect("raw keepif owned graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawKeepifControlAlive = Number(
                 rawKeepifControlRef.deref() instanceof Pattern
               );
               globalThis.rawKeepifSourceValueAlive = Number(
                 rawKeepifSourceValueRef.deref()?.marker
                   === 'raw-keepif-source'
               );
               globalThis.rawKeepifMapperAlive = Number(
                 typeof rawKeepifMapperRef.deref() === 'function'
               );
               globalThis.rawKeepifTransientsPruned = Number(
                 rawKeepifDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawKeepifControlAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepifSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepifMapperAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepifTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(
        values
            .iter()
            .all(|hap| hap.value.show().contains("marker:raw-keepif-source"))
    );
    owned
        .eval("globalThis.rawKeepifControlQueryCount = rawKeepifControlQueries();")
        .unwrap();
    assert_eq!(owned.get_number("rawKeepifControlQueryCount"), Some(0.0));
    assert_clean(&owned, "raw keepif captured condition ownership");
    owned.clear_active();
    drop(owned);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-keepif-js-terminal' };
             let condition = { marker: 'raw-keepif-js-condition' };
             let target = {};
             let extra = { marker: 'raw-keepif-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawKeepifJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawKeepifJsKeptRef = new WeakRef(kept);
             globalThis.rawKeepifJsDroppedRefs = [
               new WeakRef(condition), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keepif, target, [condition, extra]
             );
             kept = condition = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawKeepifJsKeptAlive = Number(
                 rawKeepifJsKeptRef.deref()?.marker
                   === 'raw-keepif-js-terminal'
               );
               globalThis.rawKeepifJsMapperPruned = Number(
                 rawKeepifJsMapperRef.deref() === undefined
               );
               globalThis.rawKeepifJsTransientsPruned = Number(
                 rawKeepifJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawKeepifJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawKeepifJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawKeepifJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-keepif-js-terminal"
    );
    assert_clean(&js_terminal, "raw keepif JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let condition = { marker: 'raw-keepif-unused-condition' };
             let target = {};
             let extra = { marker: 'raw-keepif-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawKeepifTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-keepif-terminal');
             };
             target.fmap = helper;
             globalThis.rawKeepifTerminalDroppedRefs = [
               new WeakRef(condition), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keepif, target, [condition, extra]
             );
             condition = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawKeepifTerminalTransientsPruned = Number(
                 rawKeepifTerminalMapperRef.deref() === undefined
                 && rawKeepifTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawKeepifTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-keepif-terminal"
    );
    assert_clean(&terminal, "raw keepif custom terminal pruning");
    terminal.clear_active();

    for condition in ["false", "true"] {
        rustel_core::reset_stepwise_entries_materialised();
        let direct = semantic_runtime(&format!("pure(0).fast(4).setSteps(5)._keepif({condition})"));
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "raw keepif charged construction-time stepwise work"
        );
        query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "raw keepif charged query-time stepwise work"
        );
        assert_clean(&direct, "raw keepif direct pool neutrality");
        direct.clear_active();
    }

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._keepif(false)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "false raw keepif suppressed or added nested source work"
    );
    assert_clean(&exact, "raw keepif nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({}).shrink(0))._keepif(false)",
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw keepif nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._keepif(true)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw keepif nested recovery");
    recovery.clear_active();

    // The direct raw adds no operation or shared-stepwise charge, but its
    // mapper remains one JavaScript callback per source hap. Aggregate
    // Pattern lineage, arbitrary callback graphs, exotic `fmap`, density and
    // general allocation, exact V8 stack/toString/absolute order, and broad
    // cancellation/deadline behavior remain explicit residuals.
}

#[test]
fn raw_eqt_keeps_direct_surface_strict_routing_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._eqt;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_eqt'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'eqt'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   const lengthDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'length'
                   );
                   const nameDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'name'
                   );
                   const prototypeDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'prototype'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable
                     && prototypeDescriptor.writable
                     && !prototypeDescriptor.enumerable
                     && !prototypeDescriptor.configurable,
                     'function descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public eqt getter descriptor');
                   const local = ['eq', '_eqt', 'eqt', 'ne'];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === 'eq|_eqt|eqt|ne',
                     'filtered strict-equality order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_eqt',
                     'enumerable strict-equality order');
                   check(!('_eqt' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_eqt'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [receiver]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-eqt-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const source = {};
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(source), mapper(source, 'extra')];
                       },
                     });
                     try {
                       const result = apply(raw, receiver, [source, {}]);
                       check(result.length === 2
                         && result.every(value => value === true),
                         'primitive receiver mapper values');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap';
                   let gets = 0;
                   let calls = 0;
                   let target;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('mapper inspected source'); },
                     ownKeys() {
                       throw new Error('mapper enumerated source');
                     },
                   });
                   const method = new Proxy(function () {}, {
                     apply(_method, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.prototype.hasOwnProperty.call(
                           mapper, 'prototype'
                         ), 'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper(source, 'ignored-mapper-extra');
                     },
                   });
                   target = new Proxy({}, {
                     get(_target, key, receiver) {
                       gets++;
                       check(key === 'fmap' && receiver === target,
                         'unexpected receiver get');
                       return method;
                     },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   check(apply(raw, target, [source, {}, {}]) === true,
                     'same identity was not strict-equal');
                   check(apply(raw, target, [{}, source]) === false,
                     'distinct identity became strict-equal');
                   check(gets === 2 && calls === 2
                     && new Set(mappers).size === 2,
                     'dynamic fmap or mapper freshness');
                   check(apply(raw, {
                     fmap(mapper) { return mapper(source); },
                   }, []) === false,
                   'zero-argument comparison was not undefined');

                   phase = 'delayed-closures';
                   const delayed = [];
                   const terminal = {};
                   const delayedTarget = {
                     fmap(mapper) { delayed.push(mapper); return terminal; },
                   };
                   check(apply(raw, delayedTarget, [source]) === terminal
                     && apply(raw, delayedTarget, [{}]) === terminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](source) === true
                     && delayed[1](source) === false,
                     'delayed mapper capture/freshness');

                   phase = 'throws-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [source]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [source]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-eqt-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const customTerminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return customTerminal; },
                     }, [source]), customTerminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [source]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorThis;
                   let constructorMapped;
                   let constructorTerminal;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper(source);
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [source]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === true,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [{}]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === false,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(source) === true,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [source], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   const replacementSource = {};
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper(replacementSource);
                     },
                   });
                   check(apply(raw, pure(0), [replacementSource]) === true
                     && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned eqt surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_eqt', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'eqt', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.eqt = surfacePoison;
                   rustelScope.eqt = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(replacementSource); },
                   }, [replacementSource]) === true && poisonHits === 0,
                     'saved raw routed through poisoned eqt surface');

                   globalThis.rawEqtSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawEqtSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawEqtSurfaceError"), None);
    assert_eq!(runtime.get_number("rawEqtSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw eqt surface and direct routing");
}

#[test]
fn raw_eqt_is_lazy_strict_metadata_safe_and_pins_bridge_residuals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-repeated';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mapperCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const shared = {};
                   const distinct = {};
                   const source = fastcat(
                     pure(shared), pure(distinct), pure(shared)
                   ).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const result = source._eqt(shared);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0, 'comparison was eager');
                   check(result !== source && result.query !== sourceQuery,
                     'result/query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     'steps were not freshly preserved');
                   const sourceHaps = source.query(state(0, 1));
                   const first = result.query(state(0, 1));
                   const second = result.query(state(0, 1));
                   check(first.length === 3 && second.length === 3
                     && first.map(hap => hap.value).join('|')
                       === 'true|false|true'
                     && second.map(hap => hap.value).join('|')
                       === 'true|false|true'
                     && first.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === 6,
                     'lazy repeated strict equality');

                   phase = 'strict-matrix';
                   let matrixCase = 0;
                   const exact = (left, right, expected) => {
                     matrixCase += 1;
                     const haps = pure(left)._eqt(right).query(state(0, 1));
                     check(haps.length === 1 && haps[0].value === expected,
                       `strict matrix ${matrixCase}`);
                   };
                   exact(undefined, undefined, true);
                   exact(null, null, true);
                   exact(null, undefined, false);
                   exact(false, false, true);
                   exact(true, 1, false);
                   exact(7, 7, true);
                   exact(7, '7', false);
                   exact(-0, 0, true);
                   exact(NaN, NaN, false);
                   exact('same', 'same', true);
                   exact('same', 'different', false);
                   const object = {};
                   exact(object, object, true);
                   exact(object, {}, false);
                   const array = [];
                   exact(array, array, true);
                   exact(array, [], false);
                   const callable = function exactFunction() {};
                   exact(callable, callable, true);
                   exact(callable, function distinctFunction() {}, false);
                   exact(rev, rev, true);

                   phase = 'no-coercion-or-traps';
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('strict equality coercion hook read');
                       },
                     });
                   }
                   exact(hostile, hostile, true);
                   exact(hostile, {}, false);
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   exact(revoked, revoked, true);
                   check(hookReads === 0 && proxyTraps === 0,
                     'strict equality coerced or trapped');

                   phase = 'pattern-right-hand-side';
                   let controlQueries = 0;
                   const patternControl = new Pattern(() => {
                     controlQueries++;
                     throw new Error('raw eqt queried Pattern rhs');
                   });
                   exact(shared, patternControl, false);
                   check(controlQueries === 0,
                     'Pattern rhs was queried');

                   phase = 'step-matrix';
                   for (const stepSource of [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ]) {
                     const stepResult = stepSource._eqt('never-equal');
                     if (stepSource._steps === undefined) {
                       check(stepResult._steps === undefined,
                         'no-step source gained steps');
                     } else {
                       check(stepResult._steps !== stepSource._steps
                         && stepResult._steps.show()
                           === stepSource._steps.show(),
                         'defined steps were not freshly preserved');
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 9, end: 10 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._eqt('tagged-source');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literal = pure('source')._eqt('source');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalHaps = literal.query(state(0, 1));
                   check(parserCalls === 0 && literalHaps.length === 1
                     && literalHaps[0].value === true,
                     'parser reached at query');
                   setStringParser(undefined);

                   phase = 'unsupported-identities';
                   const bigint = 1n;
                   const symbol = Symbol('raw-eqt-source');
                   for (const [left, right] of [
                     [bigint, bigint], [symbol, symbol]
                   ]) {
                     const haps = pure(left)._eqt(right).query(state(0, 1));
                     check(haps.length === 1 && haps[0].value === false,
                       'unsupported identity residual changed');
                   }
                   check(!Object.hasOwn(globalThis, 'eqt')
                     && !Object.hasOwn(rustelScope, 'eqt')
                     && !Object.hasOwn(Pattern.prototype, '_eq')
                     && !Object.hasOwn(Pattern.prototype, '_ne')
                     && Object.hasOwn(Pattern.prototype, '_net'),
                     'public destination or strict raw residual changed');

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure('native-source');
                   nativeSource.query = pure('replacement-source').query;
                   const reassigned = nativeSource._eqt('native-source')
                     .query(state(0, 1));
                   check(reassigned.length === 1
                     && reassigned[0].value === true,
                     'native own-query residual changed');

                   globalThis.rawEqtSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawEqtSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawEqtSemanticError"), None);
    assert_eq!(runtime.get_number("rawEqtSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw eqt lazy strict semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const haps = pure(patternValue)._eqt(patternValue).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('Pattern-source residual changed');
                 }
                 globalThis.rawEqtPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawEqtPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawEqtSourceCalls = 0;
                 globalThis.rawEqtSourceMapped = 0;
                 const sentinel = { marker: 'source-query-throw' };
                 const source = new Pattern(() => {
                   globalThis.rawEqtSourceCalls++;
                   throw sentinel;
                 });
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         globalThis.rawEqtSourceMapped++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     }),
                   ]);
                 };
                 const result = source._eqt(sentinel);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('source throw produced haps');
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawEqtSourceCalls"), Some(1.0));
    assert_eq!(runtime.get_number("rawEqtSourceMapped"), Some(0.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "source query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw eqt source-query throw cutoff");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawEqtThrowingMapperCalls = 0;
                 const sentinel = { marker: 'mapper-query-throw' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawEqtThrowingMapperCalls++;
                         throw sentinel;
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(1), pure(1))._eqt(1);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 globalThis.rawEqtThrowingMapperHaps = haps.length;
                 globalThis.rawEqtThrowingMapperUndefined =
                   haps.every(hap => hap.value === undefined) ? 1 : -1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawEqtThrowingMapperCalls"), Some(2.0));
    assert_eq!(runtime.get_number("rawEqtThrowingMapperHaps"), Some(2.0));
    assert_eq!(
        runtime.get_number("rawEqtThrowingMapperUndefined"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "mapper query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw eqt direct mapper-query throw residual");

    let mapper_product = semantic_runtime(
        r#"(() => {
             globalThis.rawEqtProductMapperCalls = 0;
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               return Reflect.apply(originalFmap, this, [
                 new Proxy(mapper, {
                   apply() {
                     globalThis.rawEqtProductMapperCalls++;
                     throw new Error('product mapper query throw');
                   },
                 }),
               ]);
             };
             const result = fastcat(pure(1), pure(1))._eqt(1);
             Pattern.prototype.fmap = originalFmap;
             return result;
           })()"#,
    );
    assert!(
        query_active(&mapper_product, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mapper_product.get_number("rawEqtProductMapperCalls"),
        Some(2.0)
    );
    assert_clean(&mapper_product, "raw eqt outer mapper-query throw");
    mapper_product.clear_active();

    // strudel.cc produces one true hap for same BigInt, Symbol, and
    // Pattern sources. Native currently loses BigInt/Symbol source identity
    // before the mapper, joins a Pattern-valued source before the outer fmap,
    // and turns each throwing mapper call into an undefined hap rather than
    // stopping the direct query at its first throw. Public destinations,
    // own/custom/later query replacement, cross-realm and browser exotica,
    // exact errors/stacks, raw function toString, and complete absolute order
    // remain explicit residuals.
}

#[test]
fn raw_eqt_retains_operands_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let comparisonQueries = 0;
             let comparison = new Pattern(() => {
               comparisonQueries++;
               throw new Error('raw eqt comparison was queried');
             });
             let sourceValue = { marker: 'raw-eqt-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-eqt-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawEqtMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawEqtComparisonQueries = () => comparisonQueries;
             globalThis.rawEqtComparisonRef = new WeakRef(comparison);
             globalThis.rawEqtSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawEqtDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._eqt, source, [comparison, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             comparison = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw eqt mapper lost its JavaScript query owner"
    );
    assert_eq!(
        owned.active_pattern().expect("raw eqt owned graph").steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawEqtComparisonAlive = Number(
                 rawEqtComparisonRef.deref() instanceof Pattern
               );
               globalThis.rawEqtSourceValueAlive = Number(
                 rawEqtSourceValueRef.deref()?.marker === 'raw-eqt-source'
               );
               globalThis.rawEqtMapperAlive = Number(
                 typeof rawEqtMapperRef.deref() === 'function'
               );
               globalThis.rawEqtTransientsPruned = Number(
                 rawEqtDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawEqtComparisonAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawEqtSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawEqtMapperAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawEqtTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(values.iter().all(|hap| hap.value == Value::Bool(false)));
    owned
        .eval("globalThis.rawEqtComparisonQueryCount = rawEqtComparisonQueries();")
        .unwrap();
    assert_eq!(owned.get_number("rawEqtComparisonQueryCount"), Some(0.0));
    assert_clean(&owned, "raw eqt operand ownership");
    owned.clear_active();
    drop(owned);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-eqt-js-terminal' };
             let comparison = { marker: 'raw-eqt-js-comparison' };
             let target = {};
             let extra = { marker: 'raw-eqt-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawEqtJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawEqtJsKeptRef = new WeakRef(kept);
             globalThis.rawEqtJsDroppedRefs = [
               new WeakRef(comparison), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._eqt, target, [comparison, extra]
             );
             kept = comparison = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawEqtJsKeptAlive = Number(
                 rawEqtJsKeptRef.deref()?.marker === 'raw-eqt-js-terminal'
               );
               globalThis.rawEqtJsMapperPruned = Number(
                 rawEqtJsMapperRef.deref() === undefined
               );
               globalThis.rawEqtJsTransientsPruned = Number(
                 rawEqtJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawEqtJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawEqtJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawEqtJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-eqt-js-terminal"
    );
    assert_clean(&js_terminal, "raw eqt JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let comparison = { marker: 'raw-eqt-unused-comparison' };
             let target = {};
             let extra = { marker: 'raw-eqt-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawEqtTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-eqt-terminal');
             };
             target.fmap = helper;
             globalThis.rawEqtTerminalDroppedRefs = [
               new WeakRef(comparison), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._eqt, target, [comparison, extra]
             );
             comparison = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawEqtTerminalTransientsPruned = Number(
                 rawEqtTerminalMapperRef.deref() === undefined
                 && rawEqtTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawEqtTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-eqt-terminal"
    );
    assert_clean(&terminal, "raw eqt custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(1).fast(4).setSteps(5)._eqt(1)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw eqt charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw eqt charged query-time stepwise work"
    );
    assert_clean(&direct, "raw eqt direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._eqt(0)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw eqt suppressed or added nested source work"
    );
    assert_clean(&exact, "raw eqt nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({}).shrink(0))._eqt(0)",
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw eqt nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._eqt(0)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw eqt nested recovery");
    recovery.clear_active();

    // The direct strict comparison adds no operation or shared-stepwise
    // charge. Exotic fmap/terminal behavior, aggregate ownership lineage,
    // async settlement, arbitrary callback graphs, density and allocation,
    // general resources, exact stacks/toString, and absolute order remain
    // explicit residuals.
}

#[test]
fn raw_net_keeps_direct_surface_strict_routing_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._net;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_net'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'net'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   const lengthDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'length'
                   );
                   const nameDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'name'
                   );
                   const prototypeDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'prototype'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable
                     && prototypeDescriptor.writable
                     && !prototypeDescriptor.enumerable
                     && !prototypeDescriptor.configurable,
                     'function descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public net getter descriptor');
                   const local = ['ne', '_net', 'net', '_and', 'and'];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === 'ne|_net|net|_and|and',
                     'filtered strict-inequality order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_net|_and',
                     'enumerable strict-inequality order');
                   check(!('_net' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_net'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [receiver]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-net-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const source = {};
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(source), mapper(source, 'extra')];
                       },
                     });
                     try {
                       const result = apply(raw, receiver, [source, {}]);
                       check(result.length === 2
                         && result.every(value => value === false),
                         'primitive receiver mapper values');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap';
                   let gets = 0;
                   let calls = 0;
                   let target;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('mapper inspected source'); },
                     ownKeys() {
                       throw new Error('mapper enumerated source');
                     },
                   });
                   const method = new Proxy(function () {}, {
                     apply(_method, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.prototype.hasOwnProperty.call(
                           mapper, 'prototype'
                         ), 'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper(source, 'ignored-mapper-extra');
                     },
                   });
                   target = new Proxy({}, {
                     get(_target, key, receiver) {
                       gets++;
                       check(key === 'fmap' && receiver === target,
                         'unexpected receiver get');
                       return method;
                     },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   check(apply(raw, target, [source, {}, {}]) === false,
                     'same identity became strict-unequal');
                   check(apply(raw, target, [{}, source]) === true,
                     'distinct identity was not strict-unequal');
                   check(gets === 2 && calls === 2
                     && new Set(mappers).size === 2,
                     'dynamic fmap or mapper freshness');
                   check(apply(raw, {
                     fmap(mapper) { return mapper(source); },
                   }, []) === true,
                   'zero-argument comparison did not differ from undefined');

                   phase = 'delayed-closures';
                   const delayed = [];
                   const terminal = {};
                   const delayedTarget = {
                     fmap(mapper) { delayed.push(mapper); return terminal; },
                   };
                   check(apply(raw, delayedTarget, [source]) === terminal
                     && apply(raw, delayedTarget, [{}]) === terminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](source) === false
                     && delayed[1](source) === true,
                     'delayed mapper capture/freshness');

                   phase = 'throws-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [source]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [source]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-net-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const customTerminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return customTerminal; },
                     }, [source]), customTerminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [source]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorThis;
                   let constructorMapped;
                   let constructorTerminal;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper(source);
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [source]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === false,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [{}]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === true,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(source) === false,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [source], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   const replacementSource = {};
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper(replacementSource);
                     },
                   });
                   check(apply(raw, pure(0), [replacementSource]) === false
                     && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned net surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_net', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'net', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.net = surfacePoison;
                   rustelScope.net = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(replacementSource); },
                   }, [replacementSource]) === false && poisonHits === 0,
                     'saved raw routed through poisoned net surface');

                   globalThis.rawNetSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawNetSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawNetSurfaceError"), None);
    assert_eq!(runtime.get_number("rawNetSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw net surface and direct routing");
}

#[test]
fn raw_net_is_lazy_strict_metadata_safe_and_pins_bridge_residuals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-repeated';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mapperCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const shared = {};
                   const distinct = {};
                   const source = fastcat(
                     pure(shared), pure(distinct), pure(shared)
                   ).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const result = source._net(shared);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0, 'comparison was eager');
                   check(result !== source && result.query !== sourceQuery,
                     'result/query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     'steps were not freshly preserved');
                   const sourceHaps = source.query(state(0, 1));
                   const first = result.query(state(0, 1));
                   const second = result.query(state(0, 1));
                   check(first.length === 3 && second.length === 3
                     && first.map(hap => hap.value).join('|')
                       === 'false|true|false'
                     && second.map(hap => hap.value).join('|')
                       === 'false|true|false'
                     && first.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === 6,
                     'lazy repeated strict inequality');

                   phase = 'strict-matrix';
                   let matrixCase = 0;
                   const exact = (left, right, expected) => {
                     matrixCase += 1;
                     const haps = pure(left)._net(right).query(state(0, 1));
                     check(haps.length === 1 && haps[0].value === expected,
                       `strict matrix ${matrixCase}`);
                   };
                   exact(undefined, undefined, false);
                   exact(null, null, false);
                   exact(null, undefined, true);
                   exact(false, false, false);
                   exact(true, 1, true);
                   exact(7, 7, false);
                   exact(7, '7', true);
                   exact(-0, 0, false);
                   exact(NaN, NaN, true);
                   exact('same', 'same', false);
                   exact('same', 'different', true);
                   const object = {};
                   exact(object, object, false);
                   exact(object, {}, true);
                   const array = [];
                   exact(array, array, false);
                   exact(array, [], true);
                   const callable = function exactFunction() {};
                   exact(callable, callable, false);
                   exact(callable, function distinctFunction() {}, true);
                   exact(rev, rev, false);

                   phase = 'no-coercion-or-traps';
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('strict inequality coercion hook read');
                       },
                     });
                   }
                   exact(hostile, hostile, false);
                   exact(hostile, {}, true);
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   exact(revoked, revoked, false);
                   check(hookReads === 0 && proxyTraps === 0,
                     'strict inequality coerced or trapped');

                   phase = 'pattern-right-hand-side';
                   let controlQueries = 0;
                   const patternControl = new Pattern(() => {
                     controlQueries++;
                     throw new Error('raw net queried Pattern rhs');
                   });
                   exact(shared, patternControl, true);
                   check(controlQueries === 0,
                     'Pattern rhs was queried');

                   phase = 'step-matrix';
                   for (const stepSource of [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ]) {
                     const stepResult = stepSource._net('never-equal');
                     if (stepSource._steps === undefined) {
                       check(stepResult._steps === undefined,
                         'no-step source gained steps');
                     } else {
                       check(stepResult._steps !== stepSource._steps
                         && stepResult._steps.show()
                           === stepSource._steps.show(),
                         'defined steps were not freshly preserved');
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 9, end: 10 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._net('tagged-source');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literal = pure('source')._net('source');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalHaps = literal.query(state(0, 1));
                   check(parserCalls === 0 && literalHaps.length === 1
                     && literalHaps[0].value === false,
                     'parser reached at query');
                   setStringParser(undefined);

                   phase = 'unsupported-identities';
                   const bigint = 1n;
                   const symbol = Symbol('raw-net-source');
                   for (const [left, right] of [
                     [bigint, bigint], [symbol, symbol]
                   ]) {
                     const haps = pure(left)._net(right).query(state(0, 1));
                     check(haps.length === 1 && haps[0].value === true,
                       'unsupported identity residual changed');
                   }
                   check(!Object.hasOwn(globalThis, 'net')
                     && !Object.hasOwn(rustelScope, 'net')
                     && !Object.hasOwn(Pattern.prototype, '_eq')
                     && !Object.hasOwn(Pattern.prototype, '_ne')
                     && Object.hasOwn(Pattern.prototype, '_and'),
                     'public destination or strict raw residual changed');

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure('native-source');
                   nativeSource.query = pure('replacement-source').query;
                   const reassigned = nativeSource._net('native-source')
                     .query(state(0, 1));
                   check(reassigned.length === 1
                     && reassigned[0].value === false,
                     'native own-query residual changed');

                   globalThis.rawNetSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawNetSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawNetSemanticError"), None);
    assert_eq!(runtime.get_number("rawNetSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw net lazy strict semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const haps = pure(patternValue)._net(patternValue).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('Pattern-source residual changed');
                 }
                 globalThis.rawNetPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawNetPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const leftPattern = pure('left-pattern');
                 const rightPattern = pure('right-pattern');
                 const haps = pure(leftPattern)._net(rightPattern).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('distinct Pattern-source residual changed');
                 }
                 globalThis.rawNetDistinctPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("rawNetDistinctPatternResidualOkay"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "distinct Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawNetSourceCalls = 0;
                 globalThis.rawNetSourceMapped = 0;
                 const sentinel = { marker: 'source-query-throw' };
                 const source = new Pattern(() => {
                   globalThis.rawNetSourceCalls++;
                   throw sentinel;
                 });
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         globalThis.rawNetSourceMapped++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     }),
                   ]);
                 };
                 const result = source._net(sentinel);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('source throw produced haps');
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawNetSourceCalls"), Some(1.0));
    assert_eq!(runtime.get_number("rawNetSourceMapped"), Some(0.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "source query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw net source-query throw cutoff");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawNetThrowingMapperCalls = 0;
                 const sentinel = { marker: 'mapper-query-throw' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawNetThrowingMapperCalls++;
                         throw sentinel;
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(1), pure(1))._net(1);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 globalThis.rawNetThrowingMapperHaps = haps.length;
                 globalThis.rawNetThrowingMapperUndefined =
                   haps.every(hap => hap.value === undefined) ? 1 : -1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawNetThrowingMapperCalls"), Some(2.0));
    assert_eq!(runtime.get_number("rawNetThrowingMapperHaps"), Some(2.0));
    assert_eq!(
        runtime.get_number("rawNetThrowingMapperUndefined"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "mapper query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw net direct mapper-query throw residual");

    let mapper_product = semantic_runtime(
        r#"(() => {
             globalThis.rawNetProductMapperCalls = 0;
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               return Reflect.apply(originalFmap, this, [
                 new Proxy(mapper, {
                   apply() {
                     globalThis.rawNetProductMapperCalls++;
                     throw new Error('product mapper query throw');
                   },
                 }),
               ]);
             };
             const result = fastcat(pure(1), pure(1))._net(1);
             Pattern.prototype.fmap = originalFmap;
             return result;
           })()"#,
    );
    assert!(
        query_active(&mapper_product, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mapper_product.get_number("rawNetProductMapperCalls"),
        Some(2.0)
    );
    assert_clean(&mapper_product, "raw net outer mapper-query throw");
    mapper_product.clear_active();

    // strudel.cc produces one false hap for the same BigInt, Symbol, or
    // Pattern source and one true hap for a distinct Pattern. Native loses
    // BigInt/Symbol source identity and reports true, while Pattern-valued
    // sources join before the outer mapper. A generic throwing fmap mapper is
    // called twice for two native source haps and direct query returns two
    // undefined haps; the outer product empties only after both calls, whereas
    // pinned Node cuts off empty after the first call. Public destinations,
    // own/custom/later query replacement, cross-realm and browser exotica,
    // exact errors/stacks, raw function toString, and complete absolute order
    // remain explicit residuals.
}

#[test]
fn raw_net_retains_operands_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let comparisonQueries = 0;
             let comparison = new Pattern(() => {
               comparisonQueries++;
               throw new Error('raw net comparison was queried');
             });
             let sourceValue = { marker: 'raw-net-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-net-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawNetMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawNetComparisonQueries = () => comparisonQueries;
             globalThis.rawNetComparisonRef = new WeakRef(comparison);
             globalThis.rawNetSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawNetDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._net, source, [comparison, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             comparison = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw net mapper lost its JavaScript query owner"
    );
    assert_eq!(
        owned.active_pattern().expect("raw net owned graph").steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawNetComparisonAlive = Number(
                 rawNetComparisonRef.deref() instanceof Pattern
               );
               globalThis.rawNetSourceValueAlive = Number(
                 rawNetSourceValueRef.deref()?.marker === 'raw-net-source'
               );
               globalThis.rawNetMapperAlive = Number(
                 typeof rawNetMapperRef.deref() === 'function'
               );
               globalThis.rawNetTransientsPruned = Number(
                 rawNetDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawNetComparisonAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawNetSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawNetMapperAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawNetTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(values.iter().all(|hap| hap.value == Value::Bool(true)));
    owned
        .eval("globalThis.rawNetComparisonQueryCount = rawNetComparisonQueries();")
        .unwrap();
    assert_eq!(owned.get_number("rawNetComparisonQueryCount"), Some(0.0));
    assert_clean(&owned, "raw net operand ownership");
    owned.clear_active();
    drop(owned);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-net-js-terminal' };
             let comparison = { marker: 'raw-net-js-comparison' };
             let target = {};
             let extra = { marker: 'raw-net-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawNetJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawNetJsKeptRef = new WeakRef(kept);
             globalThis.rawNetJsDroppedRefs = [
               new WeakRef(comparison), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._net, target, [comparison, extra]
             );
             kept = comparison = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawNetJsKeptAlive = Number(
                 rawNetJsKeptRef.deref()?.marker === 'raw-net-js-terminal'
               );
               globalThis.rawNetJsMapperPruned = Number(
                 rawNetJsMapperRef.deref() === undefined
               );
               globalThis.rawNetJsTransientsPruned = Number(
                 rawNetJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawNetJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawNetJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawNetJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-net-js-terminal"
    );
    assert_clean(&js_terminal, "raw net JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let comparison = { marker: 'raw-net-unused-comparison' };
             let target = {};
             let extra = { marker: 'raw-net-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawNetTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-net-terminal');
             };
             target.fmap = helper;
             globalThis.rawNetTerminalDroppedRefs = [
               new WeakRef(comparison), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._net, target, [comparison, extra]
             );
             comparison = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawNetTerminalTransientsPruned = Number(
                 rawNetTerminalMapperRef.deref() === undefined
                 && rawNetTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawNetTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-net-terminal"
    );
    assert_clean(&terminal, "raw net custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(1).fast(4).setSteps(5)._net(1)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw net charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw net charged query-time stepwise work"
    );
    assert_clean(&direct, "raw net direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._net(0)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw net suppressed or added nested source work"
    );
    assert_clean(&exact, "raw net nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({}).shrink(0))._net(0)",
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw net nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._net(0)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw net nested recovery");
    recovery.clear_active();

    // The direct strict-inequality comparison adds no operation or shared-stepwise
    // charge. Exotic fmap/terminal behavior, aggregate ownership lineage,
    // async settlement, arbitrary callback graphs, density and allocation,
    // general resources, exact stacks/toString, and absolute order remain
    // explicit residuals.
}

#[test]
fn raw_and_keeps_direct_surface_operand_routing_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._and;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_and'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'and'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   const lengthDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'length'
                   );
                   const nameDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'name'
                   );
                   const prototypeDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'prototype'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable
                     && prototypeDescriptor.writable
                     && !prototypeDescriptor.enumerable
                     && !prototypeDescriptor.configurable,
                     'function descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public and getter descriptor');
                   const local = [
                     '_net', 'net', '_and', 'and', '_or', 'or'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_net|net|_and|and|_or|or',
                     'filtered logical-and order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_net|_and|_or',
                     'enumerable logical-and order');
                   check(!Object.hasOwn(globalThis, '_and')
                     && !Object.hasOwn(rustelScope, '_and')
                     && Object.hasOwn(Pattern.prototype, '_or'),
                     'raw escaped prototype or failed to open _or');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [receiver]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   const selected = { marker: 'selected' };
                   const truthy = { marker: 'truthy' };
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-and-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(false), mapper(truthy)];
                       },
                     });
                     try {
                       const result = apply(raw, receiver, [selected, {}]);
                       check(result.length === 2
                         && result[0] === false
                         && result[1] === selected,
                         'primitive receiver operand routing');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap-and-mutations';
                   let gets = 0;
                   let calls = 0;
                   let target;
                   const mappers = [];
                   const method = new Proxy(function () {}, {
                     apply(_method, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.hasOwn(mapper, 'prototype'),
                         'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper;
                     },
                   });
                   target = new Proxy({}, {
                     get(_target, key, receiver) {
                       gets++;
                       check(key === 'fmap' && receiver === target,
                         'unexpected receiver get');
                       return method;
                     },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   const ignoredExtra = new Proxy({}, {
                     get() {
                       throw new Error('ignored argument was read');
                     },
                     ownKeys() {
                       throw new Error('ignored argument was enumerated');
                     },
                   });
                   const rightTruthy = { marker: 'right-truthy' };
                   const leftTruthy = { marker: 'left-truthy' };
                   const mapperTruthy = apply(
                     raw, target, [rightTruthy, ignoredExtra, ignoredExtra]
                   );
                   const mapperZero = apply(raw, target, [0]);
                   const mapperUndefined = apply(raw, target, []);
                   check(gets === 3 && calls === 3
                     && new Set(mappers).size === 3,
                     'dynamic fmap or mapper freshness');
                   check(mapperTruthy(leftTruthy) === rightTruthy,
                     'truthy lhs did not select rhs');
                   check(mapperTruthy(false) === false,
                     'false lhs was not preserved');
                   check(Object.is(mapperTruthy(-0), -0),
                     'signed-zero lhs was not preserved');
                   check(Object.is(mapperTruthy(NaN), NaN),
                     'NaN lhs was not preserved');
                   check(mapperZero(false) === false,
                     'swapped logical-and mutation survived');
                   check(mapperUndefined(leftTruthy) === undefined
                     && mapperUndefined(false) === false,
                     'zero-argument capture changed');

                   phase = 'delayed-capture';
                   const delayed = [];
                   const terminal = {};
                   const delayedTarget = {
                     fmap(mapper) { delayed.push(mapper); return terminal; },
                   };
                   const firstRight = { marker: 'first-right' };
                   const secondRight = { marker: 'second-right' };
                   check(apply(raw, delayedTarget, [firstRight]) === terminal
                     && apply(raw, delayedTarget, [secondRight]) === terminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](leftTruthy) === firstRight
                     && delayed[1](leftTruthy) === secondRight
                     && delayed[0](false) === false,
                     'delayed mapper capture/freshness');

                   phase = 'throws-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [selected]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [selected]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-and-terminal');
                   const object = {};
                   const callable = function terminalFunction() {};
                   const promise = Promise.resolve('terminal');
                   for (const customTerminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return customTerminal; },
                     }, [selected]), customTerminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [selected]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorThis;
                   let constructorTerminal;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     check(mapper(false) === false,
                       'constructor lost false lhs');
                     check(mapper(leftTruthy) === selected,
                       'constructor lost selected rhs');
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [selected]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [selected]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(leftTruthy) === selected,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [selected], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return [mapper(false), mapper(leftTruthy)];
                     },
                   });
                   const replacement = apply(raw, pure(0), [selected]);
                   check(replacementHits === 1
                     && replacement[0] === false
                     && replacement[1] === selected,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned and surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_and', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'and', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.and = surfacePoison;
                   rustelScope.and = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(leftTruthy); },
                   }, [selected]) === selected && poisonHits === 0,
                     'saved raw routed through poisoned and surface');

                   globalThis.rawAndSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawAndSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawAndSurfaceError"), None);
    assert_eq!(runtime.get_number("rawAndSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw and surface and operand routing");
}

#[test]
fn raw_and_is_lazy_truthy_metadata_safe_and_pins_bridge_residuals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-repeated';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mapperCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const selected = { marker: 'selected' };
                   const truthy = { marker: 'truthy' };
                   const source = fastcat(
                     pure(false), pure(truthy), pure(NaN)
                   ).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const result = source._and(selected);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0, 'logical and was eager');
                   check(result !== source && result.query !== sourceQuery,
                     'result/query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     'steps were not freshly preserved');
                   const sourceHaps = source.query(state(0, 1));
                   const first = result.query(state(0, 1));
                   const second = result.query(state(0, 1));
                   const routed = haps => haps.length === 3
                     && Object.is(haps[0].value, false)
                     && Object.is(haps[1].value, selected)
                     && Object.is(haps[2].value, NaN);
                   check(routed(first) && routed(second)
                     && first.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && second.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === 6,
                     'lazy repeated operand routing');

                   phase = 'truthiness-matrix';
                   let matrixCase = 0;
                   const exact = (left, right, expected) => {
                     matrixCase += 1;
                     const haps = pure(left)._and(right).query(state(0, 1));
                     check(haps.length === 1
                       && Object.is(haps[0].value, expected),
                       `truthiness matrix ${matrixCase}`);
                   };
                   exact(undefined, selected, undefined);
                   exact(null, selected, null);
                   exact(false, selected, false);
                   exact(0, selected, 0);
                   exact(-0, selected, -0);
                   exact(NaN, selected, NaN);
                   exact('', selected, '');
                   exact(true, selected, selected);
                   exact(1, selected, selected);
                   exact(-1, selected, selected);
                   exact('left', selected, selected);
                   const leftObject = { marker: 'left-object' };
                   const rightObject = { marker: 'right-object' };
                   exact(leftObject, rightObject, rightObject);
                   const rightArray = [];
                   exact([], rightArray, rightArray);
                   const rightFunction = function rightFunction() {};
                   exact(function leftFunction() {}, rightFunction,
                     rightFunction);
                   exact(false, 0, false);
                   exact(true, false, false);

                   phase = 'no-coercion-or-traps';
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('logical and coercion hook read');
                       },
                     });
                   }
                   exact(hostile, selected, selected);
                   exact(false, hostile, false);
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   exact(revoked, selected, selected);
                   exact(false, revoked, false);
                   check(hookReads === 0 && proxyTraps === 0,
                     'logical and coerced or trapped');

                   phase = 'pattern-right-hand-side';
                   let controlQueries = 0;
                   const patternControl = new Pattern(() => {
                     controlQueries++;
                     throw new Error('raw and queried false-branch rhs');
                   });
                   exact(false, patternControl, false);
                   check(controlQueries === 0,
                     'false branch queried Pattern rhs');

                   phase = 'step-matrix';
                   for (const stepSource of [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ]) {
                     const stepResult = stepSource._and('selected');
                     if (stepSource._steps === undefined) {
                       check(stepResult._steps === undefined,
                         'no-step source gained steps');
                     } else {
                       check(stepResult._steps !== stepSource._steps
                         && stepResult._steps.show()
                           === stepSource._steps.show(),
                         'defined steps were not freshly preserved');
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 11, end: 12 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._and('selected');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const falseLiteral = pure(false)._and('selected');
                   const truthyLiteral = pure('source')._and('selected');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const falseHaps = falseLiteral.query(state(0, 1));
                   const truthyHaps = truthyLiteral.query(state(0, 1));
                   check(parserCalls === 0 && falseHaps.length === 1
                     && falseHaps[0].value === false
                     && truthyHaps.length === 1
                     && truthyHaps[0].value === 'selected',
                     'parser reached at query');
                   setStringParser(undefined);

                   phase = 'unsupported-transport';
                   const zeroBigInt = pure(0n)._and('selected-zero-bigint')
                     .query(state(0, 1));
                   check(zeroBigInt.length === 1
                     && zeroBigInt[0].value === 0,
                     'BigInt source number residual changed');
                   const selectedBigInt = 9007199254740993n;
                   const selectedSymbol = Symbol('raw-and-selected');
                   for (const selectedValue of [
                     selectedBigInt, selectedSymbol, rev
                   ]) {
                     const haps = pure(true)._and(selectedValue)
                       .query(state(0, 1));
                     check(haps.length === 1
                       && !Object.is(haps[0].value, selectedValue),
                       'selected unsupported identity residual changed');
                   }
                   check(!Object.hasOwn(globalThis, 'and')
                     && !Object.hasOwn(rustelScope, 'and')
                     && Object.hasOwn(Pattern.prototype, '_or'),
                     'public destination or next raw residual changed');

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure(false);
                   nativeSource.query = pure(true).query;
                   const reassigned = nativeSource._and(selected)
                     .query(state(0, 1));
                   check(reassigned.length === 1
                     && reassigned[0].value === false,
                     'native own-query residual changed');

                   globalThis.rawAndSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawAndSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawAndSemanticError"), None);
    assert_eq!(runtime.get_number("rawAndSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw and lazy truthiness semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const haps = pure(patternValue)._and('selected').query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('Pattern-source residual changed');
                 }
                 globalThis.rawAndPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawAndPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawAndSourceCalls = 0;
                 globalThis.rawAndSourceMapped = 0;
                 const sentinel = { marker: 'source-query-throw' };
                 const source = new Pattern(() => {
                   globalThis.rawAndSourceCalls++;
                   throw sentinel;
                 });
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         globalThis.rawAndSourceMapped++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     }),
                   ]);
                 };
                 const result = source._and(sentinel);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('source throw produced haps');
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawAndSourceCalls"), Some(1.0));
    assert_eq!(runtime.get_number("rawAndSourceMapped"), Some(0.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "source query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw and source-query throw cutoff");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawAndThrowingMapperCalls = 0;
                 const sentinel = { marker: 'mapper-query-throw' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawAndThrowingMapperCalls++;
                         throw sentinel;
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(false), pure(true))
                   ._and('selected');
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 globalThis.rawAndThrowingMapperHaps = haps.length;
                 globalThis.rawAndThrowingMapperUndefined =
                   haps.every(hap => hap.value === undefined) ? 1 : -1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawAndThrowingMapperCalls"), Some(2.0));
    assert_eq!(runtime.get_number("rawAndThrowingMapperHaps"), Some(2.0));
    assert_eq!(
        runtime.get_number("rawAndThrowingMapperUndefined"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "mapper query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw and direct mapper-query throw residual");

    let mapper_product = semantic_runtime(
        r#"(() => {
             globalThis.rawAndProductMapperCalls = 0;
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               return Reflect.apply(originalFmap, this, [
                 new Proxy(mapper, {
                   apply() {
                     globalThis.rawAndProductMapperCalls++;
                     throw new Error('product mapper query throw');
                   },
                 }),
               ]);
             };
             const result = fastcat(pure(false), pure(true))
               ._and('selected');
             Pattern.prototype.fmap = originalFmap;
             return result;
           })()"#,
    );
    assert!(
        query_active(&mapper_product, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mapper_product.get_number("rawAndProductMapperCalls"),
        Some(2.0)
    );
    assert_clean(&mapper_product, "raw and outer mapper-query throw");
    mapper_product.clear_active();

    // strudel.cc preserves a falsey 0n source and exact selected BigInt,
    // Symbol, tagged-native callable, and Pattern-left routing. Native keeps
    // the 0n source as the Number 0, materialises the selected unsupported
    // identities, and joins a Pattern-valued source before the outer mapper.
    // A generic throwing fmap mapper runs for both native haps and direct
    // query returns two undefined haps; the outer product empties only after
    // both calls, whereas pinned Node cuts off empty after the first call.
    // Public destinations, own/custom/later query replacement, cross-realm
    // and browser exotica, exact errors/stacks, raw function toString, and
    // complete absolute order remain explicit residuals.
}

#[test]
fn raw_and_retains_operands_prunes_transients_and_keeps_nested_accounting() {
    let selected_branch = semantic_runtime(
        r#"(() => {
             let sourceValue = { marker: 'raw-and-source' };
             let selected = { marker: 'raw-and-selected' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-and-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawAndSelectedMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawAndSelectedRef = new WeakRef(selected);
             globalThis.rawAndSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawAndSelectedDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, source, [selected, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             sourceValue = selected = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        selected_branch.active_needs_host(),
        "raw and selected mapper lost its JavaScript query owner"
    );
    assert_eq!(
        selected_branch
            .active_pattern()
            .expect("raw and selected graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        selected_branch.run_gc();
    }
    selected_branch
        .eval(
            r#"globalThis.rawAndSelectedAlive = Number(
                 rawAndSelectedRef.deref()?.marker === 'raw-and-selected'
               );
               globalThis.rawAndSourceValueAlive = Number(
                 rawAndSourceValueRef.deref()?.marker === 'raw-and-source'
               );
               globalThis.rawAndSelectedMapperAlive = Number(
                 typeof rawAndSelectedMapperRef.deref() === 'function'
               );
               globalThis.rawAndSelectedTransientsPruned = Number(
                 rawAndSelectedDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(selected_branch.get_number("rawAndSelectedAlive"), Some(1.0));
    assert_eq!(
        selected_branch.get_number("rawAndSourceValueAlive"),
        Some(1.0)
    );
    assert_eq!(
        selected_branch.get_number("rawAndSelectedMapperAlive"),
        Some(1.0)
    );
    assert_eq!(
        selected_branch.get_number("rawAndSelectedTransientsPruned"),
        Some(1.0)
    );
    let selected_values = query_active(&selected_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(selected_values.len(), 4);
    assert!(
        selected_values
            .iter()
            .all(|hap| hap.value.show() == "marker:raw-and-selected")
    );
    assert_clean(&selected_branch, "raw and selected-branch ownership");
    selected_branch.clear_active();
    drop(selected_branch);

    let false_branch = semantic_runtime(
        r#"(() => {
             let rightQueries = 0;
             let right = new Pattern(() => {
               rightQueries++;
               throw new Error('raw and false branch queried rhs');
             });
             let source = pure(false).fast(4).setSteps(5);
             let extra = { marker: 'raw-and-false-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawAndFalseMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawAndRightQueries = () => rightQueries;
             globalThis.rawAndRightRef = new WeakRef(right);
             globalThis.rawAndFalseDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, source, [right, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             right = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        false_branch.active_needs_host(),
        "raw and false mapper lost its JavaScript query owner"
    );
    assert_eq!(
        false_branch
            .active_pattern()
            .expect("raw and false graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        false_branch.run_gc();
    }
    false_branch
        .eval(
            r#"globalThis.rawAndRightAlive = Number(
                 rawAndRightRef.deref() instanceof Pattern
               );
               globalThis.rawAndFalseMapperAlive = Number(
                 typeof rawAndFalseMapperRef.deref() === 'function'
               );
               globalThis.rawAndFalseTransientsPruned = Number(
                 rawAndFalseDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(false_branch.get_number("rawAndRightAlive"), Some(1.0));
    assert_eq!(false_branch.get_number("rawAndFalseMapperAlive"), Some(1.0));
    assert_eq!(
        false_branch.get_number("rawAndFalseTransientsPruned"),
        Some(1.0)
    );
    let false_values = query_active(&false_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(false_values.len(), 4);
    assert!(
        false_values
            .iter()
            .all(|hap| hap.value == Value::Bool(false))
    );
    false_branch
        .eval("globalThis.rawAndRightQueryCount = rawAndRightQueries();")
        .unwrap();
    assert_eq!(false_branch.get_number("rawAndRightQueryCount"), Some(0.0));
    assert_clean(&false_branch, "raw and false-branch ownership");
    false_branch.clear_active();
    drop(false_branch);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-and-js-terminal' };
             let selected = { marker: 'raw-and-js-selected' };
             let target = {};
             let extra = { marker: 'raw-and-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawAndJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawAndJsKeptRef = new WeakRef(kept);
             globalThis.rawAndJsDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, target, [selected, extra]
             );
             kept = selected = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawAndJsKeptAlive = Number(
                 rawAndJsKeptRef.deref()?.marker === 'raw-and-js-terminal'
               );
               globalThis.rawAndJsMapperPruned = Number(
                 rawAndJsMapperRef.deref() === undefined
               );
               globalThis.rawAndJsTransientsPruned = Number(
                 rawAndJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawAndJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawAndJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawAndJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-and-js-terminal"
    );
    assert_clean(&js_terminal, "raw and JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let selected = { marker: 'raw-and-unused-selected' };
             let target = {};
             let extra = { marker: 'raw-and-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawAndTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-and-terminal');
             };
             target.fmap = helper;
             globalThis.rawAndTerminalDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, target, [selected, extra]
             );
             selected = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawAndTerminalTransientsPruned = Number(
                 rawAndTerminalMapperRef.deref() === undefined
                 && rawAndTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawAndTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-and-terminal"
    );
    assert_clean(&terminal, "raw and custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(false).fast(4).setSteps(5)._and('selected')");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw and charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw and charged query-time stepwise work"
    );
    assert_clean(&direct, "raw and direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._and('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw and suppressed or added nested source work"
    );
    assert_clean(&exact, "raw and nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({}).shrink(0))._and('selected')",
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw and nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._and('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw and nested recovery");
    recovery.clear_active();

    // The direct logical mapper adds no operation or shared-stepwise charge.
    // Exotic fmap/terminal behavior, aggregate ownership lineage, async
    // settlement, arbitrary callback graphs, density and allocation, general
    // resources, exact stacks/toString, and absolute order remain explicit
    // residuals.
}

#[test]
fn raw_or_keeps_direct_surface_operand_routing_and_constructors() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const raw = Pattern.prototype._or;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_or'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'or'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.hasOwn(raw, 'prototype'),
                     'raw function shape');
                   const lengthDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'length'
                   );
                   const nameDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'name'
                   );
                   const prototypeDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'prototype'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable
                     && prototypeDescriptor.writable
                     && !prototypeDescriptor.enumerable
                     && !prototypeDescriptor.configurable,
                     'function descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public or getter descriptor');
                   const local = [
                     '_and', 'and', '_or', 'or', '_func', 'func'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_and|and|_or|or',
                     'filtered logical-or order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_and|_or',
                     'enumerable logical-or order');
                   check(!Object.hasOwn(globalThis, '_or')
                     && !Object.hasOwn(rustelScope, '_or')
                     && !Object.hasOwn(globalThis, 'func')
                     && !Object.hasOwn(rustelScope, 'func')
                     && !Object.hasOwn(Pattern.prototype, '_func')
                     && !Object.hasOwn(Pattern.prototype, 'func'),
                     'raw escaped prototype or opened func pair');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [receiver]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   const selected = { marker: 'selected' };
                   const truthy = { marker: 'truthy' };
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-or-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(truthy), mapper(false)];
                       },
                     });
                     try {
                       const result = apply(raw, receiver, [selected, {}]);
                       check(result.length === 2
                         && result[0] === truthy
                         && result[1] === selected,
                         'primitive receiver operand routing');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap-and-mutations';
                   let gets = 0;
                   let calls = 0;
                   let target;
                   const mappers = [];
                   const method = new Proxy(function () {}, {
                     apply(_method, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.hasOwn(mapper, 'prototype'),
                         'mapper reflection');
                       check(Function.prototype.toString.call(mapper)
                         === '(x) => op(x, value)',
                         'mapper lexical body');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper;
                     },
                   });
                   target = new Proxy({}, {
                     get(_target, key, receiver) {
                       gets++;
                       check(key === 'fmap' && receiver === target,
                         'unexpected receiver get');
                       return method;
                     },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   const ignoredExtra = new Proxy({}, {
                     get() {
                       throw new Error('ignored argument was read');
                     },
                     ownKeys() {
                       throw new Error('ignored argument was enumerated');
                     },
                   });
                   const rightTruthy = { marker: 'right-truthy' };
                   const leftTruthy = { marker: 'left-truthy' };
                   const otherTruthy = { marker: 'other-truthy' };
                   const mapperSelected = apply(
                     raw, target, [rightTruthy, ignoredExtra, ignoredExtra]
                   );
                   const mapperFour = apply(raw, target, [4]);
                   const mapperUndefined = apply(raw, target, []);
                   check(gets === 3 && calls === 3
                     && new Set(mappers).size === 3,
                     'dynamic fmap or mapper freshness');
                   check(mapperSelected(leftTruthy) === leftTruthy,
                     'truthy lhs was not preserved');
                   check(mapperSelected(otherTruthy) === otherTruthy,
                     'swapped logical-or mutation survived');
                   check(mapperSelected(false) === rightTruthy
                     && mapperSelected(0) === rightTruthy
                     && mapperSelected('') === rightTruthy,
                     'falsey lhs did not select rhs');
                   check(mapperSelected(true) === true,
                     'booleanized or constant mutation survived');
                   check(mapperFour(2) === 2 && mapperFour(0) === 4,
                     'bitwise-or or logical-and mutation survived');
                   check(mapperUndefined(leftTruthy) === leftTruthy
                     && mapperUndefined(false) === undefined,
                     'zero-argument capture changed');
                   const selectedPattern = pure('selected-pattern');
                   check(apply(raw, {
                     fmap(mapper) { return mapper(false); },
                   }, [selectedPattern]) === selectedPattern,
                     'selected Pattern identity changed before transport');

                   phase = 'delayed-capture';
                   const delayed = [];
                   const terminal = {};
                   const delayedTarget = {
                     fmap(mapper) { delayed.push(mapper); return terminal; },
                   };
                   const firstRight = { marker: 'first-right' };
                   const secondRight = { marker: 'second-right' };
                   check(apply(raw, delayedTarget, [firstRight]) === terminal
                     && apply(raw, delayedTarget, [secondRight]) === terminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](false) === firstRight
                     && delayed[1](false) === secondRight
                     && delayed[0](leftTruthy) === leftTruthy,
                     'delayed mapper capture/freshness');

                   phase = 'throws-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [selected]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [selected]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-or-terminal');
                   const object = {};
                   const callable = function terminalFunction() {};
                   const promise = Promise.resolve('terminal');
                   for (const customTerminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return customTerminal; },
                     }, [selected]), customTerminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [selected]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorThis;
                   let constructorTerminal;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     check(mapper(leftTruthy) === leftTruthy,
                       'constructor lost truthy lhs');
                     check(mapper(false) === selected,
                       'constructor lost selected rhs');
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [selected]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [selected]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(false) === selected,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [selected], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return [mapper(leftTruthy), mapper(false)];
                     },
                   });
                   const replacement = apply(raw, pure(0), [selected]);
                   check(replacementHits === 1
                     && replacement[0] === leftTruthy
                     && replacement[1] === selected,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned or surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_or', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'or', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.or = surfacePoison;
                   rustelScope.or = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(false); },
                   }, [selected]) === selected && poisonHits === 0,
                     'saved raw routed through poisoned or surface');

                   globalThis.rawOrSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawOrSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawOrSurfaceError"), None);
    assert_eq!(runtime.get_number("rawOrSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw or surface and operand routing");
}

#[test]
fn raw_or_is_lazy_truthy_metadata_safe_and_pins_bridge_residuals() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-repeated';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mapperCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const selected = { marker: 'selected' };
                   const truthy = { marker: 'truthy' };
                   const source = fastcat(
                     pure(truthy), pure(false), pure(NaN)
                   ).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const result = source._or(selected);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0, 'logical or was eager');
                   check(result !== source && result.query !== sourceQuery,
                     'result/query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     'steps were not freshly preserved');
                   const sourceHaps = source.query(state(0, 1));
                   const first = result.query(state(0, 1));
                   const second = result.query(state(0, 1));
                   const routed = haps => haps.length === 3
                     && Object.is(haps[0].value, truthy)
                     && Object.is(haps[1].value, selected)
                     && Object.is(haps[2].value, selected);
                   check(routed(first) && routed(second)
                     && first.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && second.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === 6,
                     'lazy repeated operand routing');

                   phase = 'truthiness-matrix';
                   let matrixCase = 0;
                   const exact = (left, right, expected) => {
                     matrixCase += 1;
                     const haps = pure(left)._or(right).query(state(0, 1));
                     check(haps.length === 1
                       && Object.is(haps[0].value, expected),
                       `truthiness matrix ${matrixCase}`);
                   };
                   exact(undefined, selected, selected);
                   exact(null, selected, selected);
                   exact(false, selected, selected);
                   exact(0, selected, selected);
                   exact(-0, selected, selected);
                   exact(NaN, selected, selected);
                   exact('', selected, selected);
                   exact(true, selected, true);
                   exact(1, selected, 1);
                   exact(-1, selected, -1);
                   exact('left', selected, 'left');
                   const leftObject = { marker: 'left-object' };
                   const rightObject = { marker: 'right-object' };
                   exact(leftObject, rightObject, leftObject);
                   const leftArray = [];
                   exact(leftArray, [], leftArray);
                   const leftFunction = function leftFunction() {};
                   exact(leftFunction, function rightFunction() {},
                     leftFunction);
                   exact(false, 0, 0);
                   exact(false, false, false);
                   exact(true, false, true);
                   exact(2, 4, 2);

                   phase = 'no-coercion-or-traps';
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('logical or coercion hook read');
                       },
                     });
                   }
                   const direct = (left, right) => Reflect.apply(
                     Pattern.prototype._or,
                     { fmap(mapper) { return mapper(left); } },
                     [right]
                   );
                   check(direct(hostile, selected) === hostile
                     && direct(true, hostile) === true,
                     'logical or touched a coercion hook');
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   check(direct(revoked, selected) === revoked
                     && direct(false, revoked) === revoked,
                     'logical or changed a revoked Proxy operand');
                   check(hookReads === 0 && proxyTraps === 0,
                     'logical or coerced or trapped');

                   phase = 'selected-pattern';
                   let selectedQueries = 0;
                   const selectedPattern = new Pattern(() => {
                     selectedQueries++;
                     throw new Error('raw or eagerly queried selected rhs');
                   });
                   check(direct(false, selectedPattern) === selectedPattern
                     && selectedQueries === 0,
                     'selected Pattern identity or laziness changed');

                   phase = 'step-matrix';
                   for (const stepSource of [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ]) {
                     const stepResult = stepSource._or('selected');
                     if (stepSource._steps === undefined) {
                       check(stepResult._steps === undefined,
                         'no-step source gained steps');
                     } else {
                       check(stepResult._steps !== stepSource._steps
                         && stepResult._steps.show()
                           === stepSource._steps.show(),
                         'defined steps were not freshly preserved');
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 11, end: 12 };
                   check(Object.hasOwn(tagged, '__pure')
                     && Object.hasOwn(tagged, '__pure_loc'),
                     'source tag precondition');
                   const tagResult = tagged._or('selected');
                   check(!Object.hasOwn(tagResult, '__pure')
                     && !Object.hasOwn(tagResult, '__pure_loc'),
                     'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const falseLiteral = pure(false)._or('selected');
                   const truthyLiteral = pure('source')._or('selected');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const falseHaps = falseLiteral.query(state(0, 1));
                   const truthyHaps = truthyLiteral.query(state(0, 1));
                   check(parserCalls === 0 && falseHaps.length === 1
                     && falseHaps[0].value === 'selected'
                     && truthyHaps.length === 1
                     && truthyHaps[0].value === 'source',
                     'parser reached at query');
                   setStringParser(undefined);

                   phase = 'unsupported-transport';
                   const zeroBigInt = pure(0n)._or('selected-zero-bigint')
                     .query(state(0, 1));
                   check(zeroBigInt.length === 1
                     && zeroBigInt[0].value === 'selected-zero-bigint',
                     'falsey BigInt source was not skipped');
                   const selectedBigInt = 9007199254740993n;
                   const selectedSymbol = Symbol('raw-or-selected');
                   for (const selectedValue of [
                     selectedBigInt, selectedSymbol, rev
                   ]) {
                     const haps = pure(false)._or(selectedValue)
                       .query(state(0, 1));
                     check(haps.length === 1
                       && !Object.is(haps[0].value, selectedValue),
                       'selected unsupported identity residual changed');
                   }
                   check(!Object.hasOwn(globalThis, 'or')
                     && !Object.hasOwn(rustelScope, 'or')
                     && !Object.hasOwn(Pattern.prototype, '_func')
                     && !Object.hasOwn(Pattern.prototype, 'func'),
                     'public destination or next raw residual changed');

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure(true);
                   nativeSource.query = pure(false).query;
                   const reassigned = nativeSource._or(selected)
                     .query(state(0, 1));
                   check(reassigned.length === 1
                     && reassigned[0].value === true,
                     'native own-query residual changed');

                   globalThis.rawOrSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawOrSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawOrSemanticError"), None);
    assert_eq!(runtime.get_number("rawOrSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw or lazy truthiness semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const haps = pure(patternValue)._or('selected').query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('Pattern-source residual changed');
                 }
                 globalThis.rawOrPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawOrPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawOrSourceCalls = 0;
                 globalThis.rawOrSourceMapped = 0;
                 const sentinel = { marker: 'source-query-throw' };
                 const source = new Pattern(() => {
                   globalThis.rawOrSourceCalls++;
                   throw sentinel;
                 });
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         globalThis.rawOrSourceMapped++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     }),
                   ]);
                 };
                 const result = source._or(sentinel);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('source throw produced haps');
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawOrSourceCalls"), Some(1.0));
    assert_eq!(runtime.get_number("rawOrSourceMapped"), Some(0.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "source query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw or source-query throw cutoff");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawOrThrowingMapperCalls = 0;
                 const sentinel = { marker: 'mapper-query-throw' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawOrThrowingMapperCalls++;
                         throw sentinel;
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(true), pure(false))
                   ._or('selected');
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 globalThis.rawOrThrowingMapperHaps = haps.length;
                 globalThis.rawOrThrowingMapperUndefined =
                   haps.every(hap => hap.value === undefined) ? 1 : -1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawOrThrowingMapperCalls"), Some(2.0));
    assert_eq!(runtime.get_number("rawOrThrowingMapperHaps"), Some(2.0));
    assert_eq!(
        runtime.get_number("rawOrThrowingMapperUndefined"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "mapper query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw or direct mapper-query throw residual");

    let mapper_product = semantic_runtime(
        r#"(() => {
             globalThis.rawOrProductMapperCalls = 0;
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               return Reflect.apply(originalFmap, this, [
                 new Proxy(mapper, {
                   apply() {
                     globalThis.rawOrProductMapperCalls++;
                     throw new Error('product mapper query throw');
                   },
                 }),
               ]);
             };
             const result = fastcat(pure(true), pure(false))
               ._or('selected');
             Pattern.prototype.fmap = originalFmap;
             return result;
           })()"#,
    );
    assert!(
        query_active(&mapper_product, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mapper_product.get_number("rawOrProductMapperCalls"),
        Some(2.0)
    );
    assert_clean(&mapper_product, "raw or outer mapper-query throw");
    mapper_product.clear_active();

    // strudel.cc preserves exact truthy Pattern sources and exact selected
    // Pattern/BigInt/Symbol/tagged-native values. Native joins Pattern-valued
    // sources before the mapper and materialises unsupported selected values.
    // Generic throwing fmap callbacks run twice natively and direct query
    // returns two undefined haps; the product empties only after both calls,
    // whereas pinned Node cuts off empty after the first call. Public
    // destinations, query replacement, cross-realm/browser exotica, exact
    // errors/stacks, raw toString and complete order remain residuals.
}

#[test]
fn raw_or_retains_operands_prunes_transients_and_keeps_nested_accounting() {
    let truthy_branch = semantic_runtime(
        r#"(() => {
             let sourceValue = { marker: 'raw-or-source' };
             let rightQueries = 0;
             let right = new Pattern(() => {
               rightQueries++;
               throw new Error('raw or truthy branch queried rhs');
             });
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-or-truthy-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawOrTruthyMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawOrSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawOrRightRef = new WeakRef(right);
             globalThis.rawOrRightQueries = () => rightQueries;
             globalThis.rawOrTruthyDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, source, [right, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             sourceValue = right = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        truthy_branch.active_needs_host(),
        "raw or truthy mapper lost its JavaScript query owner"
    );
    assert_eq!(
        truthy_branch
            .active_pattern()
            .expect("raw or truthy graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        truthy_branch.run_gc();
    }
    truthy_branch
        .eval(
            r#"globalThis.rawOrSourceValueAlive = Number(
                 rawOrSourceValueRef.deref()?.marker === 'raw-or-source'
               );
               globalThis.rawOrRightAlive = Number(
                 rawOrRightRef.deref() instanceof Pattern
               );
               globalThis.rawOrTruthyMapperAlive = Number(
                 typeof rawOrTruthyMapperRef.deref() === 'function'
               );
               globalThis.rawOrTruthyTransientsPruned = Number(
                 rawOrTruthyDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(truthy_branch.get_number("rawOrSourceValueAlive"), Some(1.0));
    assert_eq!(truthy_branch.get_number("rawOrRightAlive"), Some(1.0));
    assert_eq!(
        truthy_branch.get_number("rawOrTruthyMapperAlive"),
        Some(1.0)
    );
    assert_eq!(
        truthy_branch.get_number("rawOrTruthyTransientsPruned"),
        Some(1.0)
    );
    let truthy_values = query_active(&truthy_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(truthy_values.len(), 4);
    assert!(
        truthy_values
            .iter()
            .all(|hap| hap.value.show() == "marker:raw-or-source")
    );
    truthy_branch
        .eval("globalThis.rawOrRightQueryCount = rawOrRightQueries();")
        .unwrap();
    assert_eq!(truthy_branch.get_number("rawOrRightQueryCount"), Some(0.0));
    assert_clean(&truthy_branch, "raw or truthy-branch ownership");
    truthy_branch.clear_active();
    drop(truthy_branch);

    let selected_branch = semantic_runtime(
        r#"(() => {
             let selected = { marker: 'raw-or-selected' };
             let source = pure(false).fast(4).setSteps(5);
             let extra = { marker: 'raw-or-selected-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawOrSelectedMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawOrSelectedRef = new WeakRef(selected);
             globalThis.rawOrSelectedDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, source, [selected, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             selected = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        selected_branch.active_needs_host(),
        "raw or selected mapper lost its JavaScript query owner"
    );
    assert_eq!(
        selected_branch
            .active_pattern()
            .expect("raw or selected graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        selected_branch.run_gc();
    }
    selected_branch
        .eval(
            r#"globalThis.rawOrSelectedAlive = Number(
                 rawOrSelectedRef.deref()?.marker === 'raw-or-selected'
               );
               globalThis.rawOrSelectedMapperAlive = Number(
                 typeof rawOrSelectedMapperRef.deref() === 'function'
               );
               globalThis.rawOrSelectedTransientsPruned = Number(
                 rawOrSelectedDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(selected_branch.get_number("rawOrSelectedAlive"), Some(1.0));
    assert_eq!(
        selected_branch.get_number("rawOrSelectedMapperAlive"),
        Some(1.0)
    );
    assert_eq!(
        selected_branch.get_number("rawOrSelectedTransientsPruned"),
        Some(1.0)
    );
    let selected_values = query_active(&selected_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(selected_values.len(), 4);
    assert!(
        selected_values
            .iter()
            .all(|hap| hap.value.show() == "marker:raw-or-selected")
    );
    assert_clean(&selected_branch, "raw or selected-branch ownership");
    selected_branch.clear_active();
    drop(selected_branch);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-or-js-terminal' };
             let selected = { marker: 'raw-or-js-selected' };
             let target = {};
             let extra = { marker: 'raw-or-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawOrJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawOrJsKeptRef = new WeakRef(kept);
             globalThis.rawOrJsDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, target, [selected, extra]
             );
             kept = selected = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawOrJsKeptAlive = Number(
                 rawOrJsKeptRef.deref()?.marker === 'raw-or-js-terminal'
               );
               globalThis.rawOrJsMapperPruned = Number(
                 rawOrJsMapperRef.deref() === undefined
               );
               globalThis.rawOrJsTransientsPruned = Number(
                 rawOrJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawOrJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawOrJsMapperPruned"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawOrJsTransientsPruned"), Some(1.0));
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-or-js-terminal"
    );
    assert_clean(&js_terminal, "raw or JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let selected = { marker: 'raw-or-unused-selected' };
             let target = {};
             let extra = { marker: 'raw-or-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawOrTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-or-terminal');
             };
             target.fmap = helper;
             globalThis.rawOrTerminalDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, target, [selected, extra]
             );
             selected = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawOrTerminalTransientsPruned = Number(
                 rawOrTerminalMapperRef.deref() === undefined
                 && rawOrTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawOrTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-or-terminal"
    );
    assert_clean(&terminal, "raw or custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(true).fast(4).setSteps(5)._or('selected')");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw or charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw or charged query-time stepwise work"
    );
    assert_clean(&direct, "raw or direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(false).polyBind(() => gap({limit}).shrink(0))._or('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw or suppressed or added nested source work"
    );
    assert_clean(&exact, "raw or nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(false).polyBind(() => gap({}).shrink(0))._or('selected')",
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw or nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(false).polyBind(() => gap({limit}).shrink(0))._or('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw or nested recovery");
    recovery.clear_active();

    // The direct logical mapper adds no operation or shared-stepwise charge.
    // Exotic fmap/terminal behavior, aggregate ownership lineage, async
    // settlement, arbitrary callback graphs, density and allocation, general
    // resources, exact stacks/toString and absolute order remain residuals.
}
