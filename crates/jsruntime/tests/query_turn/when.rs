use super::*;

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
            .steps(),
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
            .steps(),
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
