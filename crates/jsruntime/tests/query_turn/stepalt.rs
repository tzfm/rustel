use super::*;

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
        // The retained query and its final event compiler each own one cell.
        baseline + 2,
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
