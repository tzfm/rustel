use super::*;

#[test]
fn raw_extend_replicate_are_eager_and_preserve_returned_ownership() {
    let extend = semantic_runtime("slowcat(pure('a'), pure('b')).setSteps(2)._extend(2)");
    assert!(
        !extend.active_needs_host(),
        "scalar raw extend retained JavaScript"
    );
    assert_eq!(
        extend.active_pattern().expect("active raw extend").steps(),
        Some(Fraction::int(4))
    );
    let extend_values = query_active(&extend, Fraction::ZERO, Fraction::int(2))
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(extend_values, ["a", "b", "a", "b"]);
    assert_clean(&extend, "scalar raw extend");
    extend.clear_active();

    let replicate = semantic_runtime("slowcat(pure('a'), pure('b')).setSteps(2)._replicate(2)");
    assert!(
        !replicate.active_needs_host(),
        "scalar raw replicate retained JavaScript"
    );
    assert_eq!(
        replicate
            .active_pattern()
            .expect("active raw replicate")
            .steps(),
        Some(Fraction::int(4))
    );
    let replicate_values = query_active(&replicate, Fraction::ZERO, Fraction::int(2))
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(replicate_values, ["a", "a", "b", "b"]);
    assert_clean(&replicate, "scalar raw replicate");
    replicate.clear_active();

    for (method, factor, expected_steps, expected_values) in [
        ("_extend", "1/2", Fraction::ONE, vec!["a"]),
        ("_replicate", "1/2", Fraction::ONE, vec!["a"]),
        ("_extend", "0", Fraction::ZERO, Vec::new()),
        ("_replicate", "0", Fraction::ZERO, Vec::new()),
    ] {
        let runtime = semantic_runtime(&format!("sequence('a', 'b').{method}({factor})"));
        assert_eq!(
            runtime
                .active_pattern()
                .expect("active raw numeric case")
                .steps(),
            Some(expected_steps),
            "{method}({factor}) step metadata"
        );
        assert!(
            !runtime.active_needs_host(),
            "{method}({factor}) retained JavaScript"
        );
        let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>();
        assert_eq!(values, expected_values, "{method}({factor}) values");
        assert_clean(&runtime, &format!("{method}({factor})"));
        runtime.clear_active();
    }

    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawExtendEagerHits = 0;
             globalThis.rawReplicateEagerHits = 0;
             globalThis.rawExtendReplicateLateHits = 0;
             globalThis.rawExtendReplicateMismatch = 0;
             const factor = { marker: 'raw-factor' };

             const extendSource = sequence('a', 'b');
             const extendMiddle = {};
             extendSource.fast = function (received) {
               globalThis.rawExtendEagerHits++;
               if (this !== extendSource || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return extendMiddle;
             };
             extendMiddle.expand = function (received) {
               globalThis.rawExtendEagerHits++;
               if (this !== extendMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return pure('raw-extend-eager').setSteps(2);
             };
             const extended = extendSource._extend(factor);
             extendSource.fast = extendMiddle.expand = function () {
               globalThis.rawExtendReplicateLateHits++;
               return pure('raw-extend-late');
             };

             const replicateSource = sequence('c', 'd');
             const repeatMiddle = {};
             const fastMiddle = {};
             replicateSource.repeatCycles = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== replicateSource || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return repeatMiddle;
             };
             repeatMiddle.fast = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== repeatMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return fastMiddle;
             };
             fastMiddle.expand = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== fastMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return pure('raw-replicate-eager').setSteps(3);
             };
             const replicated = replicateSource._replicate(factor);
             replicateSource.repeatCycles = repeatMiddle.fast
               = fastMiddle.expand = function () {
                 globalThis.rawExtendReplicateLateHits++;
                 return pure('raw-replicate-late');
               };
             return stack(extended, replicated);
           })()"#,
    );
    assert!(
        !eager.active_needs_host(),
        "eager raw chains returning native graphs retained JavaScript"
    );
    assert_eq!(eager.get_number("rawExtendEagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawReplicateEagerHits"), Some(3.0));
    assert_eq!(eager.get_number("rawExtendReplicateMismatch"), Some(0.0));
    let eager_values = query_active(&eager, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(eager_values.iter().any(|value| value == "raw-extend-eager"));
    assert!(
        eager_values
            .iter()
            .any(|value| value == "raw-replicate-eager")
    );
    assert_eq!(eager.get_number("rawExtendReplicateLateHits"), Some(0.0));
    assert_clean(&eager, "raw extend/replicate eager overrides");
    eager.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let extendMarker = { marker: 'raw-extend-owned' };
             let replicateMarker = { marker: 'raw-replicate-owned' };
             globalThis.rawExtendOwnedRef = new WeakRef(extendMarker);
             globalThis.rawReplicateOwnedRef = new WeakRef(replicateMarker);
             let extendSource = sequence('a', 'b');
             let replicateSource = sequence('c', 'd');
             extendSource.fast = () => ({
               expand: () => pure(extendMarker).setSteps(1),
             });
             replicateSource.repeatCycles = () => ({
               fast: () => ({
                 expand: () => pure(replicateMarker).setSteps(1),
               }),
             });
             const result = stack(
               extendSource._extend(2),
               replicateSource._replicate(2)
             );
             extendMarker = null;
             replicateMarker = null;
             extendSource = null;
             replicateSource = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw chain results lost their JavaScript-owned values"
    );
    owned.run_gc();
    owned.run_gc();
    owned.run_gc();
    owned
        .eval(
            r#"globalThis.rawExtendOwnedAlive = Number(
                 rawExtendOwnedRef.deref()?.marker === 'raw-extend-owned'
               );
               globalThis.rawReplicateOwnedAlive = Number(
                 rawReplicateOwnedRef.deref()?.marker === 'raw-replicate-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawExtendOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawReplicateOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-extend-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-replicate-owned"))
    );
    assert_clean(&owned, "raw extend/replicate returned ownership");
    owned.clear_active();
}

#[test]
fn raw_extend_replicate_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b')._extend(2),
             slowcat(pure('c'), pure('d')).setSteps(2)._replicate(2)
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw extend/replicate charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw extend/replicate charged query-time stepwise work"
    );
    assert_clean(&direct, "raw extend/replicate pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return sequence('a', 'b')._extend(2);
             }}
             if (value === 1) {{
               return slowcat(pure('c'), pure('d'))
                 .setSteps(2)._replicate(2);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw extend/replicate consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw extend/replicate shared-pool neutrality");
    shared.clear_active();
}
