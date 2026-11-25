use super::*;

#[test]
fn raw_expand_contract_keep_timing_purity_ownership_and_pool_neutrality() {
    let with_steps = semantic_runtime("pure('with-steps').withSteps(t => t.mul(2))");
    let with_steps_pattern = with_steps
        .active_pattern()
        .expect("active withSteps result");
    assert_eq!(with_steps_pattern.steps(), Some(Fraction::int(2)));
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
                .steps(),
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
        no_steps
            .active_pattern()
            .expect("no-step raw expand")
            .steps(),
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
