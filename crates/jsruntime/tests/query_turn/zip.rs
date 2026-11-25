use super::*;

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
    assert_eq!(runtime.active_pattern().expect("empty zip").steps(), None);
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
    assert_eq!(
        runtime.active_pattern().expect("filtered zip").steps(),
        None
    );
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
        runtime.active_pattern().expect("singleton zip").steps(),
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
        exact.active_pattern().expect("exact zip").steps(),
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
