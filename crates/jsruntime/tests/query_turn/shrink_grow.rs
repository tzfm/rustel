use super::*;

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
        runtime.active_pattern().expect("patterned shrink").steps(),
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
