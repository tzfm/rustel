use super::*;

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
        assert_eq!(pattern.steps(), expected_steps, "{label}: steps");
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
    assert_eq!(native_pattern.steps(), Some(Fraction::int(8)));
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
