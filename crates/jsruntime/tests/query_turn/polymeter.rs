use super::*;

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
        modern_exact.active_pattern().expect("modern exact").steps(),
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
        legacy_exact.active_pattern().expect("legacy exact").steps(),
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
            runtime
                .active_pattern()
                .expect("empty legacy stack")
                .steps(),
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
        silent
            .active_pattern()
            .expect("zero-target silence")
            .steps(),
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
                .steps(),
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
