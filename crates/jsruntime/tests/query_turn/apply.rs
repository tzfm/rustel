use super::*;

#[test]
fn raw_apply_is_eager_host_free_and_preserves_only_returned_ownership() {
    let native = semantic_runtime(
        r#"(() => {
             globalThis.rawApplyNativeHits = 0;
             globalThis.rawApplyNativeMismatch = 0;
             let callback = function (pat) {
               'use strict';
               rawApplyNativeHits++;
               if (this !== undefined || pat !== source) {
                 rawApplyNativeMismatch++;
               }
               return pat;
             };
             globalThis.rawApplyNativeCallbackRef = new WeakRef(callback);
             const source = pure('raw-apply-native').setSteps(3);
             const location = { start: 4, end: 9 };
             source.__pure_loc = location;
             const query = source.query;
             const result = source._apply(callback);
             globalThis.rawApplyNativeIdentity = Number(
               result === source
               && result.query === query
               && result.__pure_loc === location
             );
             callback = null;
             return result;
           })()"#,
    );
    assert_eq!(native.get_number("rawApplyNativeHits"), Some(1.0));
    assert_eq!(native.get_number("rawApplyNativeMismatch"), Some(0.0));
    assert_eq!(native.get_number("rawApplyNativeIdentity"), Some(1.0));
    assert!(
        !native.active_needs_host(),
        "an eager native raw-apply return retained its callback"
    );
    assert_eq!(
        native
            .active_pattern()
            .expect("raw apply native pattern")
            .steps(),
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        native.run_gc();
    }
    native
        .eval(
            r#"globalThis.rawApplyNativeCallbackPruned = Number(
                 rawApplyNativeCallbackRef.deref() === undefined
               )"#,
        )
        .unwrap();
    assert_eq!(native.get_number("rawApplyNativeCallbackPruned"), Some(1.0));
    assert_eq!(
        query_active(&native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-apply-native"
    );
    assert_eq!(
        native.get_number("rawApplyNativeHits"),
        Some(1.0),
        "raw apply callback was retained until query"
    );
    assert_clean(&native, "raw apply eager native return");
    native.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawApplyOwnedHits = 0;
             globalThis.rawApplyOwnedQueryHits = 0;
             globalThis.rawApplyOwnedMismatch = 0;
             const makePattern = kept => new Pattern(state => {
               rawApplyOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-apply-owned' };
             let outer = { marker: 'outer' };
             let target = { marker: 'target' };
             let transient = { marker: 'captured-transient' };
             let extra = { marker: 'ignored-extra' };
             let callback = function (received) {
               'use strict';
               rawApplyOwnedHits++;
               if (this !== undefined || received !== target
                   || transient.marker !== 'captured-transient') {
                 rawApplyOwnedMismatch++;
               }
               return makePattern(kept);
             };
             globalThis.rawApplyKeptRef = new WeakRef(kept);
             globalThis.rawApplyDroppedRefs = [
               new WeakRef(outer), new WeakRef(target),
               new WeakRef(transient), new WeakRef(extra),
               new WeakRef(callback)
             ];
             const result = Reflect.apply(Pattern.prototype._apply, outer, [
               callback, target, extra
             ]);
             callback = outer = target = transient = extra = kept = null;
             return result;
           })()"#,
    );
    assert_eq!(owned.get_number("rawApplyOwnedHits"), Some(1.0));
    assert_eq!(owned.get_number("rawApplyOwnedQueryHits"), Some(0.0));
    assert_eq!(owned.get_number("rawApplyOwnedMismatch"), Some(0.0));
    assert!(
        owned.active_needs_host(),
        "a returned JavaScript Pattern/value lost its ownership sidecar"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawApplyKeptAlive = Number(
                 rawApplyKeptRef.deref()?.marker === 'raw-apply-owned'
               );
               globalThis.rawApplyTransientsPruned = Number(
                 rawApplyDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawApplyKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawApplyTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values.iter().any(|value| value.contains("raw-apply-owned")),
        "returned JavaScript value was not queryable: {values:?}"
    );
    assert_eq!(owned.get_number("rawApplyOwnedHits"), Some(1.0));
    assert_eq!(owned.get_number("rawApplyOwnedQueryHits"), Some(1.0));
    assert_clean(&owned, "raw apply returned ownership");
    owned.clear_active();
}

#[test]
fn raw_apply_is_pool_neutral_while_nested_work_keeps_its_accounting() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure('raw-apply')._apply(pat => pat)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw apply charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw apply charged query-time stepwise work"
    );
    assert_clean(&direct, "raw apply pool-neutral direct return");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => pure('x')._apply(() => gap({limit}).shrink(0)))"
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "nested raw apply callback ran before query"
    );
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw apply hid or duplicated nested shrink accounting"
    );
    assert_clean(&exact, "raw apply nested exact accounting");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => pure('x')._apply(() => gap({}).shrink(0)))",
        limit + 1
    ));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
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
    assert_clean(&over, "raw apply nested +1 refusal");
    over.clear_active();

    // Callback-created arbitrary graphs remain governed by their own
    // operation-specific limits; this bounded raw slot adds no policy.
}
