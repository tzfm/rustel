use super::*;

#[test]
fn raw_take_drop_are_eager_and_preserve_only_returned_ownership() {
    for (expression, expected) in [
        ("_take(1)", vec!["a"]),
        ("_take(-1)", vec!["d"]),
        ("_drop(1)", vec!["b", "c", "d"]),
        ("_drop(-1)", vec!["a", "b", "c"]),
    ] {
        let runtime = semantic_runtime(&format!("sequence('a', 'b', 'c', 'd').{expression}"));
        assert!(
            !runtime.active_needs_host(),
            "{expression} retained JavaScript on a scalar native path"
        );
        let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>();
        assert_eq!(values, expected, "{expression} selected the wrong steps");
        assert_clean(&runtime, expression);
        runtime.clear_active();
    }

    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawTakeEagerHits = 0;
             globalThis.rawTakeLateHits = 0;
             globalThis.rawDropEagerHits = 0;
             globalThis.rawDropLateHits = 0;
             globalThis.rawEagerMismatch = 0;

             const takeSource = sequence('a', 'b', 'c', 'd');
             takeSource.zoom = function (begin, end) {
               globalThis.rawTakeEagerHits++;
               if (this !== takeSource || begin !== 0
                   || !(end instanceof Fraction._original)
                   || end.show() !== '1/4') {
                 globalThis.rawEagerMismatch++;
               }
               return pure('raw-take-eager').setSteps(2);
             };
             const taken = takeSource._take(1);
             takeSource.zoom = function () {
               globalThis.rawTakeLateHits++;
               return pure('raw-take-late').setSteps(9);
             };

             const dropSource = sequence('a', 'b', 'c', 'd');
             dropSource.take = function (amount) {
               globalThis.rawDropEagerHits++;
               if (this !== dropSource
                   || !(amount instanceof Fraction._original)
                   || amount.show() !== '-3/1') {
                 globalThis.rawEagerMismatch++;
               }
               return pure('raw-drop-eager').setSteps(3);
             };
             const dropped = dropSource._drop(1);
             dropSource.take = function () {
               globalThis.rawDropLateHits++;
               return pure('raw-drop-late').setSteps(9);
             };
             return stack(taken, dropped);
           })()"#,
    );
    assert!(
        !eager.active_needs_host(),
        "eager raw overrides returning native graphs became query callbacks"
    );
    assert_eq!(eager.get_number("rawTakeEagerHits"), Some(1.0));
    assert_eq!(eager.get_number("rawDropEagerHits"), Some(1.0));
    assert_eq!(eager.get_number("rawEagerMismatch"), Some(0.0));
    let eager_values = query_active(&eager, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(eager_values.iter().any(|value| value == "raw-take-eager"));
    assert!(eager_values.iter().any(|value| value == "raw-drop-eager"));
    assert_eq!(eager.get_number("rawTakeLateHits"), Some(0.0));
    assert_eq!(eager.get_number("rawDropLateHits"), Some(0.0));
    assert_clean(&eager, "raw take/drop eager overrides");
    eager.clear_active();

    // Canonical public take finalization is load-bearing for ownership. An
    // equal-count drop wraps lexical nothing and prunes the source callback;
    // a zero drop wraps the full source and must retain it.
    let pruned = semantic_runtime(
        r#"(() => {
             globalThis.rawDropPrunedHits = 0;
             const source = new Pattern(state => {
               globalThis.rawDropPrunedHits++;
               return pure('raw-drop-pruned').query(state);
             }, 4);
             return source._drop(4);
           })()"#,
    );
    assert!(
        !pruned.active_needs_host(),
        "equal-count raw drop retained an unreachable receiver callback"
    );
    assert!(
        query_active(&pruned, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(pruned.get_number("rawDropPrunedHits"), Some(0.0));
    assert_clean(&pruned, "raw drop pruned ownership");
    pruned.clear_active();

    let retained = semantic_runtime(
        r#"(() => {
             globalThis.rawDropRetainedHits = 0;
             const source = new Pattern(state => {
               globalThis.rawDropRetainedHits++;
               return pure('raw-drop-retained').query(state);
             }, 4);
             return source._drop(0);
           })()"#,
    );
    assert!(
        retained.active_needs_host(),
        "zero raw drop lost the retained receiver callback"
    );
    retained.run_gc();
    retained.run_gc();
    retained.run_gc();
    let retained_values = query_active(&retained, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(retained_values, ["raw-drop-retained"]);
    assert_eq!(retained.get_number("rawDropRetainedHits"), Some(1.0));
    assert_clean(&retained, "raw drop retained ownership");
    retained.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let takeMarker = { marker: 'raw-take-owned' };
             let dropMarker = { marker: 'raw-drop-owned' };
             globalThis.rawTakeOwnedRef = new WeakRef(takeMarker);
             globalThis.rawDropOwnedRef = new WeakRef(dropMarker);
             let takeSource = sequence('a', 'b', 'c', 'd');
             let dropSource = sequence('a', 'b', 'c', 'd');
             takeSource.zoom = () => pure(takeMarker).setSteps(1);
             dropSource.take = () => pure(dropMarker).setSteps(1);
             const result = stack(
               takeSource._take(1),
               dropSource._drop(1)
             );
             takeMarker = null;
             dropMarker = null;
             takeSource = null;
             dropSource = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw override results lost their JS-owned values"
    );
    owned.run_gc();
    owned.run_gc();
    owned.run_gc();
    owned
        .eval(
            r#"globalThis.rawTakeOwnedAlive = Number(
                 rawTakeOwnedRef.deref()?.marker === 'raw-take-owned'
               );
               globalThis.rawDropOwnedAlive = Number(
                 rawDropOwnedRef.deref()?.marker === 'raw-drop-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawTakeOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawDropOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-take-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-drop-owned"))
    );
    assert_clean(&owned, "raw take/drop returned ownership");
    owned.clear_active();
}

#[test]
fn raw_take_drop_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b', 'c', 'd')._take([1, 2]),
             sequence('a', 'b', 'c', 'd')._drop([1, 2])
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw take/drop charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw take/drop charged query-time stepwise work"
    );
    assert_clean(&direct, "raw take/drop pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return sequence('a', 'b', 'c', 'd')._take([1, 2]);
             }}
             if (value === 1) {{
               return sequence('a', 'b', 'c', 'd')._drop([1, 2]);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw take/drop consumed any of the shared stepwise allowance"
    );
    assert_clean(&shared, "raw take/drop shared-pool neutrality");
    shared.clear_active();
}
