use super::*;

fn enter_supervised_child(test_name: &str, marker: &str, timeout: Duration) -> bool {
    if std::env::var_os(marker).is_some() {
        return true;
    }

    let mut child =
        std::process::Command::new(std::env::current_exe().expect("the test binary path"))
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

#[test]
fn kabelsalat_query_refusals_leave_the_heap_ready_for_the_next_score() {
    if !enter_supervised_child(
        "query_turn::deadlines::kabelsalat_query_refusals_leave_the_heap_ready_for_the_next_score",
        "RUSTEL_KABEL_QUERY_CHILD",
        Duration::from_secs(10),
    ) {
        return;
    }
    let runtime = semantic_runtime("worklet('n(1).out()')");
    let cancelled = AtomicBool::new(false);
    for (source, message) in [
        ("worklet('midifreq().out()')", Some("live control input")),
        (
            r#"worklet('n(1).raw("unknown(t)").out()')"#,
            Some("unknown"),
        ),
        ("worklet('n(Infinity).out()')", Some("finite")),
        ("worklet('while (true) {}')", None),
        (
            r#"worklet('const x = n(1); x.out(); Object.defineProperty(x, "ins", {get() { while (true) {} }});')"#,
            None,
        ),
        (
            "worklet('n(1).out()').fmap(value => Object.defineProperty(value, 'freq', {enumerable: true, get() { while (true) {} }}))",
            None,
        ),
    ] {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .unwrap_or_else(|error| panic!("install {source}: {error}"));
        let budget = if message.is_some() {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(40)
        };
        let start = Instant::now();
        let result = runtime.query_cancellable(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::ONE,
            budget,
            &cancelled,
        );
        if let Some(message) = message {
            assert!(
                result.expect("contained graph error").is_empty(),
                "{source}"
            );
            let thrown = runtime.take_query_throw().expect("graph diagnostic");
            assert!(thrown.contains(message), "{source}: {thrown}");
        } else {
            assert!(
                matches!(
                    result,
                    Err(QueryError::Limit(QueryLimit::JsCpuDeadline { millis: 40 }))
                ),
                "{source}: {result:?}"
            );
            assert!(start.elapsed() < Duration::from_secs(3), "{source}");
        }
        assert_clean(&runtime, source);
        runtime
            .evaluate_score(
                "worklet('n(pat[0]).out()', 2).worklet('n(pat[0] * 10 + pat[1]).out()', 3)",
                &TranspileOptions::default(),
            )
            .expect("install recovery graph");
        let haps =
            query_active(&runtime, Fraction::ZERO, Fraction::ONE).expect("query recovery graph");
        assert_eq!(haps.len(), 1, "{source}");
        let graph = haps[0]
            .value
            .get("workletGraph")
            .and_then(Value::as_str)
            .expect("compiled recovery graph");
        let program = rustel_kabel::Program::from_json(graph).expect("native recovery program");
        let output = rustel_kabel::Graph::new(&program, 48_000.0)
            .expect("recovery state")
            .process([0.0; 2], 1.0, 0.0);
        assert_eq!(output, [23.0; 2], "{source}");
        runtime.run_gc();
        assert_clean(&runtime, "KabelSalat recovery");
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
            .arg("query_turn::deadlines::callback_deadline_is_typed_and_the_same_heap_recovers")
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
        .expect("the limit is accepted");
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
        "query_turn::deadlines::legacy_query_apis_default_deadlines_are_exact_and_heaps_recover",
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
    let runtime = JsRuntime::new().expect("the runtime builds");
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
        .expect("the activation registers");
    runtime
        .eval("globalThis.queueLegacyPendingJob = true;")
        .expect("the activation registers");
    runtime
}

fn exercise_typed_legacy_pending_job_entry(
    label: &str,
    invoke: impl Fn(&JsRuntime) -> Result<Vec<rustel_core::Hap>, QueryError>,
) {
    let runtime = legacy_pending_job_runtime();
    let error = invoke(&runtime).expect_err("the deadline error is the test");
    assert!(
        matches!(error, QueryError::Limit(QueryLimit::JsPendingJobs)),
        "{label}: callback-created job lost its typed refusal: {error:?}"
    );
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(1.0));
    assert_eq!(runtime.get_number("legacyPendingJobRan"), None);
    assert_clean(&runtime, &format!("{label} callback-created job"));

    runtime
        .eval("globalThis.queueLegacyPendingJob = false;")
        .expect("the script evaluates");
    let recovered = invoke(&runtime).expect("same heap must recover after job refusal");
    assert_eq!(recovered[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(2.0));

    runtime
        .eval("queueMicrotask(() => { globalThis.staleLegacyJobRan = 1; });")
        .expect("the script evaluates");
    assert!(runtime.jobs_pending(), "{label}: stale job was not queued");
    let stale = invoke(&runtime).expect_err("the deadline error is the test");
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
    let error = invoke(&runtime).expect_err("the deadline error is the test");
    assert_eq!(error, expected, "{label}: wrong compatibility error");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(1.0));
    assert_eq!(runtime.get_number("legacyPendingJobRan"), None);
    assert_clean(&runtime, &format!("{label} callback-created job"));

    runtime
        .eval("globalThis.queueLegacyPendingJob = false;")
        .expect("the script evaluates");
    let recovered = invoke(&runtime).expect("same heap must recover after job refusal");
    assert_eq!(recovered[0].value.show(), "legacy!");
    assert_eq!(runtime.get_number("legacyPendingCalls"), Some(2.0));

    runtime
        .eval("queueMicrotask(() => { globalThis.staleLegacyJobRan = 1; });")
        .expect("the script evaluates");
    assert!(runtime.jobs_pending(), "{label}: stale job was not queued");
    let stale = invoke(&runtime).expect_err("the deadline error is the test");
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
