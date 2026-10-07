use rustel_core::{QueryLimit, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Vec<rustel_core::Hap> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .expect("evaluate");
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query")
}

fn values(runtime: &JsRuntime, source: &str) -> Vec<Value> {
    evaluate(runtime, source)
        .into_iter()
        .map(|hap| hap.value)
        .collect()
}

#[test]
fn numeric_generators_keep_their_values_and_timing() {
    let runtime = runtime();
    assert_eq!(
        values(&runtime, "run(4)"),
        [0.0, 1.0, 2.0, 3.0].map(Value::F64)
    );
    assert_eq!(
        values(&runtime, "binaryN(5, 4)"),
        [0.0, 1.0, 0.0, 1.0].map(Value::F64)
    );
    assert_eq!(
        values(&runtime, "binary(5)"),
        [1.0, 0.0, 1.0].map(Value::F64)
    );
    assert_eq!(
        values(&runtime, "binaryNL(5, 4)"),
        [Value::List(
            [0.0, 1.0, 0.0, 1.0].into_iter().map(Value::F64).collect(),
        )]
    );
    assert_eq!(
        values(&runtime, "binaryL(5)"),
        [Value::List(
            [1.0, 0.0, 1.0].into_iter().map(Value::F64).collect(),
        )]
    );
    let base = evaluate(&runtime, "base(255, 16)");
    assert_eq!(
        base.iter().map(|hap| hap.value.clone()).collect::<Vec<_>>(),
        [Value::F64(15.0), Value::F64(15.0)]
    );
    assert_eq!(
        base.iter()
            .map(|hap| hap.whole.expect("whole").duration())
            .collect::<Vec<_>>(),
        [Fraction::new(1, 2), Fraction::new(1, 2)]
    );
    assert_eq!(
        values(&runtime, "base(10, 2)"),
        [1.0, 0.0, 1.0, 0.0].map(Value::F64)
    );
    assert_eq!(
        values(&runtime, "base(1234, 10, 2)"),
        [3.0, 4.0].map(Value::F64)
    );
    assert_eq!(
        values(&runtime, "base('10', 2)"),
        [1.0, 0.0, 1.0, 0.0].map(Value::F64)
    );
    assert!(values(&runtime, "base('c3', 10)").is_empty());
}

#[test]
fn ref_calls_its_accessor_without_arguments_and_survives_collection() {
    let runtime = runtime();
    runtime
        .evaluate_score(
            r#"
            globalThis.refCalls = 0;
            ref(function () {
              if (arguments.length !== 0) throw new Error('ref arguments');
              return ++globalThis.refCalls;
            })
            "#,
            &TranspileOptions::default(),
        )
        .expect("ref pattern");
    runtime.run_gc();
    let first = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("first ref query");
    let second = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("second ref query");
    assert_eq!(first[0].value, Value::F64(1.0));
    assert_eq!(second[0].value, Value::F64(2.0));
    assert!(runtime.active_needs_host());
}

#[test]
fn pick_currying_and_compatibility_order_survive_collection() {
    let runtime = runtime();
    runtime
        .eval("globalThis.savedPickmod = pickmod(['a', 'b'])")
        .expect("partial pickmod");
    runtime.run_gc();
    assert_eq!(
        values(&runtime, "savedPickmod(3)"),
        [Value::Str("b".into())]
    );
    assert_eq!(
        values(&runtime, "pick(0, ['a', 'b'])"),
        [Value::Str("a".into())]
    );
    assert_eq!(
        values(&runtime, "pick(['a', 'b'], 0)"),
        [Value::Str("a".into())]
    );
}

#[test]
fn native_numeric_generators_leave_no_query_time_javascript() {
    for source in [
        "run(4)",
        "binaryN(5, 4)",
        "binary(5)",
        "binaryNL(5, 4)",
        "binaryL(5)",
        "base(255, 16)",
    ] {
        let runtime = runtime();
        evaluate(&runtime, source);
        assert!(
            !runtime.active_needs_host(),
            "{source} retained a JavaScript query dependency"
        );
    }
}

#[test]
fn native_list_generators_refuse_unbounded_expansion() {
    let runtime = runtime();
    runtime
        .evaluate_score("binaryNL(1, 1e9)", &TranspileOptions::default())
        .expect("binaryNL pattern");
    let binary_error = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect_err("oversized binaryNL query");
    assert!(
        matches!(
            &binary_error,
            QueryError::Limit(QueryLimit::StepwiseExpansion {
                operation: "binaryNL",
                minimum_entries,
                limit,
            }) if minimum_entries > limit
        ),
        "unexpected binaryNL error: {binary_error:?}"
    );

    runtime
        .evaluate_score("base(1, 1)", &TranspileOptions::default())
        .expect("base pattern");
    assert!(matches!(
        runtime.query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "base",
            minimum_entries,
            limit,
        })) if minimum_entries == limit + 1
    ));
}

/// `binaryNL` builds a list up to the stepwise limit and refuses a longer one
/// with its exact bit count, or the least count over the limit past `u64`.
#[test]
fn binary_nl_refuses_counts_over_the_stepwise_limit() {
    let runtime = runtime();
    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    for (source, expected) in [
        (format!("binaryNL(0, {})", limit + 1), limit + 1),
        (format!("binaryNL(0, {})", 2 * limit), 2 * limit),
        ("binaryNL(0, 18446744073709551615)".to_owned(), limit + 1),
        ("binaryNL(0, 1e300)".to_owned(), limit + 1),
    ] {
        runtime
            .evaluate_score(&source, &TranspileOptions::default())
            .expect("binaryNL pattern");
        let error = runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect_err("oversized binaryNL query");
        assert!(
            matches!(
                &error,
                QueryError::Limit(QueryLimit::StepwiseExpansion {
                    operation: "binaryNL",
                    minimum_entries,
                    ..
                }) if *minimum_entries == expected
            ),
            "unexpected {source} error: {error:?}"
        );
    }

    let at_limit = values(&runtime, &format!("binaryNL(0, {limit})"));
    let [Value::List(bits)] = at_limit.as_slice() else {
        panic!("binaryNL(0, {limit}) is not one list");
    };
    assert_eq!(bits.len(), limit as usize);
}

/// `randrun` accepts a count up to the stepwise limit and refuses a larger or
/// infinite one with a typed limit, while a small count still permutes `0..n`.
#[test]
fn randrun_refuses_counts_over_the_stepwise_limit() {
    let max = rustel_core::MAX_STEPWISE_ENTRIES;
    // Querying only the first step samples the signal once, so an unrefused
    // count stays cheap.
    let first_step = |runtime: &JsRuntime, source: &str| {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("randrun pattern");
        runtime.query(
            Slot::Active,
            0,
            Fraction::ZERO,
            Fraction::new(1, i128::from(max) * 2),
        )
    };

    let limited = runtime();
    first_step(&limited, &format!("randrun({max})")).expect("randrun at the limit");
    for source in [format!("randrun({})", max + 1), "randrun(Infinity)".into()] {
        let error = first_step(&limited, &source).expect_err("oversized randrun query");
        assert!(
            matches!(
                &error,
                QueryError::Limit(QueryLimit::StepwiseExpansion {
                    operation: "randrun",
                    minimum_entries,
                    limit,
                }) if minimum_entries > limit
            ),
            "unexpected {source} error: {error:?}"
        );
    }

    let mut permutation: Vec<f64> = values(&limited, "randrun(4)")
        .into_iter()
        .map(|value| value.as_f64().expect("randrun value"))
        .collect();
    permutation.sort_by(|a, b| a.partial_cmp(b).expect("comparable randrun value"));
    assert_eq!(permutation, [0.0, 1.0, 2.0, 3.0]);
}

#[test]
fn rand_list_refuses_oversized_lists_before_sampling() {
    let runtime = runtime();
    for source in ["randL(1e12)", "s('saw').partials(randL(1e12))"] {
        runtime
            .evaluate_score(source, &TranspileOptions::default())
            .expect("randL pattern");
        let error = runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect_err("oversized randL query");
        assert!(
            matches!(
                &error,
                QueryError::Limit(QueryLimit::StepwiseExpansion {
                    operation: "randL",
                    minimum_entries,
                    limit,
                }) if minimum_entries > limit
            ),
            "unexpected {source} error: {error:?}"
        );
    }

    assert!(matches!(values(&runtime, "randL(4)").as_slice(), [Value::List(xs)] if xs.len() == 4));

    // Each list is checked by itself, so a long query is not refused.
    runtime
        .evaluate_score(
            r#"s("saw*64").partials(randL(64))"#,
            &TranspileOptions::default(),
        )
        .expect("randL pattern");
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::from(8))
        .expect("a multi-cycle randL query");
    assert_eq!(haps.len(), 64 * 8);
}

/// `signal(f)` makes one hap per query span valued `f` at the span's begin,
/// so `segment` samples it; a throwing or missing function yields no haps.
#[test]
fn a_score_written_signal_samples_time_at_query_spans() {
    let runtime = runtime();
    assert_eq!(
        values(&runtime, "signal(t => t * 10).segment(4)"),
        [0.0, 2.5, 5.0, 7.5].map(Value::F64)
    );

    for source in ["signal(t => { throw new Error('no') })", "signal(4)"] {
        let haps = evaluate(&runtime, source);
        assert!(haps.is_empty(), "{source} made haps: {haps:?}");
    }
}

/// A realm built with a pointer reads it for every mouse spelling at query
/// time, and `mousex` stays a value that `segment` samples.
#[test]
fn a_realm_with_a_pointer_reads_it_for_the_mouse_names() {
    let pointer = rustel_core::host_value::Pointer::default();
    let runtime = JsRuntime::with_pointer(pointer.clone()).expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    pointer.x.set(0.25);
    pointer.y.set(0.75);
    for (source, position) in [
        ("mousex", 0.25),
        ("mouseX", 0.25),
        ("mousey", 0.75),
        ("mouseY", 0.75),
    ] {
        assert_eq!(values(&runtime, source), [Value::F64(position)], "{source}");
    }
    assert_eq!(
        values(&runtime, "mousex.segment(4)"),
        [0.25; 4].map(Value::F64)
    );
    assert!(!runtime.active_pattern().expect("active").is_cacheable());

    pointer.x.set(0.5);
    let haps = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query");
    assert!(haps.iter().all(|hap| hap.value == Value::F64(0.5)));
}

/// A realm without a pointer reads the mouse as the cacheable constant 0.
#[test]
fn a_realm_without_a_pointer_reads_the_mouse_as_zero() {
    let runtime = runtime();
    for source in ["mousex", "mouseX", "mousey", "mouseY"] {
        assert_eq!(values(&runtime, source), [Value::F64(0.0)], "{source}");
        assert!(runtime.active_pattern().expect("active").is_cacheable());
    }
}
