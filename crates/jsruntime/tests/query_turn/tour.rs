use super::*;

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
