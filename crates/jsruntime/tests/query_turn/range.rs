use super::*;

#[test]
fn raw_range_pair_keep_scalar_timing_steps_and_tag_loss() {
    let third = Fraction::new(1, 3);
    let two_thirds = Fraction::new(2, 3);
    for (label, expression, expected_values) in [
        (
            "range forward",
            "fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(10, 20)",
            ["10", "15", "20"],
        ),
        (
            "range reversed",
            "fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(20, 10)",
            ["20", "15", "10"],
        ),
        (
            "range equal",
            "fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(7, 7)",
            ["7", "7", "7"],
        ),
        (
            "range2 bipolar",
            "fastcat(pure(-1), pure(0), pure(1)).setSteps(3)._range2(10, 20)",
            ["10", "15", "20"],
        ),
    ] {
        let runtime = semantic_runtime(&format!(
            r#"(() => {{
                 const result = {expression};
                 globalThis.rawRangePairTagless = Number(
                   !Object.prototype.hasOwnProperty.call(result, '__pure')
                   && !Object.prototype.hasOwnProperty.call(
                     result, '__pure_loc'
                   )
                 );
                 return result;
               }})()"#
        ));
        assert_eq!(
            runtime.get_number("rawRangePairTagless"),
            Some(1.0),
            "{label}: register fast-path tags survived"
        );
        let pattern = runtime.active_pattern().expect("active raw range result");
        assert_eq!(pattern.steps(), Some(Fraction::int(3)), "{label}: steps");
        assert!(pattern.is_pure(), "{label}: native purity");
        assert!(
            pattern.as_pure().is_none(),
            "{label}: structural pure fast path survived"
        );
        assert!(
            !runtime.active_needs_host(),
            "{label}: native scalar result retained JavaScript"
        );
        let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            haps.iter().map(|hap| hap.value.show()).collect::<Vec<_>>(),
            expected_values,
            "{label}: values"
        );
        assert_eq!(
            haps.iter()
                .map(|hap| (
                    hap.part().begin,
                    hap.part().end,
                    hap.whole().expect("raw range whole").begin,
                    hap.whole().expect("raw range whole").end,
                ))
                .collect::<Vec<_>>(),
            [
                (Fraction::ZERO, third, Fraction::ZERO, third),
                (third, two_thirds, third, two_thirds),
                (two_thirds, Fraction::ONE, two_thirds, Fraction::ONE),
            ],
            "{label}: timing"
        );
        assert_clean(&runtime, label);
        runtime.clear_active();
    }

    let tagged = semantic_runtime(
        r#"(() => {
             const rangeSource = pure(.5);
             const range2Source = pure(0);
             rangeSource.__pure_loc = { start: 10, end: 11 };
             range2Source.__pure_loc = { start: 20, end: 21 };
             const ranged = rangeSource._range(10, 20);
             const ranged2 = range2Source._range2(10, 20);
             globalThis.rawRangePureTagLoss = Number(
               Object.prototype.hasOwnProperty.call(rangeSource, '__pure')
               && Object.prototype.hasOwnProperty.call(range2Source, '__pure')
               && Object.prototype.hasOwnProperty.call(
                 rangeSource, '__pure_loc'
               )
               && Object.prototype.hasOwnProperty.call(
                 range2Source, '__pure_loc'
               )
               && !Object.prototype.hasOwnProperty.call(ranged, '__pure')
               && !Object.prototype.hasOwnProperty.call(ranged, '__pure_loc')
               && !Object.prototype.hasOwnProperty.call(ranged2, '__pure')
               && !Object.prototype.hasOwnProperty.call(ranged2, '__pure_loc')
             );
             return stack(ranged, ranged2);
           })()"#,
    );
    assert_eq!(tagged.get_number("rawRangePureTagLoss"), Some(1.0));
    assert!(
        !tagged.active_needs_host(),
        "tag-loss controls retained JavaScript"
    );
    assert_eq!(
        query_active(&tagged, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["15", "15"]
    );
    assert_clean(&tagged, "raw range pair pure-tag loss");
    tagged.clear_active();
}

#[test]
fn raw_range_pair_are_eager_and_preserve_only_returned_ownership() {
    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawRangeEagerHits = 0;
             globalThis.rawRange2EagerHits = 0;
             globalThis.rawRangePairLateHits = 0;
             globalThis.rawRangePairMismatch = 0;

             const rangeSource = pure(.5);
             const rangeMiddle = {};
             Object.defineProperty(rangeSource, 'mul', {
               configurable: true,
               value(difference) {
                 globalThis.rawRangeEagerHits++;
                 if (this !== rangeSource || difference !== 4) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return rangeMiddle;
               },
             });
             Object.defineProperty(rangeMiddle, 'add', {
               configurable: true,
               value(min) {
                 globalThis.rawRangeEagerHits++;
                 if (this !== rangeMiddle || min !== 2) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return pure('range-eager').setSteps(1);
               },
             });
             const ranged = rangeSource._range(2, 6);
             Object.defineProperty(rangeSource, 'mul', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range-late');
               },
             });
             Object.defineProperty(rangeMiddle, 'add', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range-late');
               },
             });

             const range2Source = pure(0);
             const bipolarMiddle = {};
             Object.defineProperty(range2Source, 'fromBipolar', {
               configurable: true,
               value() {
                 globalThis.rawRange2EagerHits++;
                 if (this !== range2Source) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return bipolarMiddle;
               },
             });
             Object.defineProperty(bipolarMiddle, '_range', {
               configurable: true,
               value(min, max) {
                 globalThis.rawRange2EagerHits++;
                 if (this !== bipolarMiddle || min !== 2 || max !== 6) {
                   globalThis.rawRangePairMismatch++;
                 }
                 return pure('range2-eager').setSteps(1);
               },
             });
             const ranged2 = range2Source._range2(2, 6);
             Object.defineProperty(range2Source, 'fromBipolar', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range2-late');
               },
             });
             Object.defineProperty(bipolarMiddle, '_range', {
               configurable: true,
               value() {
                 globalThis.rawRangePairLateHits++;
                 return pure('range2-late');
               },
             });
             return stack(ranged, ranged2);
           })()"#,
    );
    assert_eq!(eager.get_number("rawRangeEagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawRange2EagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawRangePairMismatch"), Some(0.0));
    assert!(
        !eager.active_needs_host(),
        "eager native range results retained chain functions"
    );
    assert_eq!(
        query_active(&eager, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["range-eager", "range2-eager"]
    );
    assert_eq!(eager.get_number("rawRangePairLateHits"), Some(0.0));
    assert_clean(&eager, "raw range pair eager mutation");
    eager.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             const apply = Reflect.apply;
             let rangeKept = { marker: 'raw-range-owned' };
             let rangeMin = { valueOf() { return 2; } };
             let rangeMax = { valueOf() { return 6; } };
             let rangeSource = {};
             let rangeMiddle = {};
             globalThis.rawRangeKeptRef = new WeakRef(rangeKept);
             globalThis.rawRangeDroppedRefs = [
               new WeakRef(rangeMin), new WeakRef(rangeMax),
               new WeakRef(rangeSource), new WeakRef(rangeMiddle)
             ];
             rangeSource.mul = () => rangeMiddle;
             rangeMiddle.add = () => pure(rangeKept).setSteps(1);
             const ranged = apply(Pattern.prototype._range, null, [
               rangeMin, rangeMax, rangeSource
             ]);

             let range2Kept = { marker: 'raw-range2-owned' };
             let range2Min = { marker: 'range2-min' };
             let range2Max = { marker: 'range2-max' };
             let range2Source = {};
             let bipolarMiddle = {};
             globalThis.rawRange2KeptRef = new WeakRef(range2Kept);
             globalThis.rawRange2DroppedRefs = [
               new WeakRef(range2Min), new WeakRef(range2Max),
               new WeakRef(range2Source), new WeakRef(bipolarMiddle)
             ];
             range2Source.fromBipolar = () => bipolarMiddle;
             bipolarMiddle._range = () => pure(range2Kept).setSteps(1);
             const ranged2 = apply(Pattern.prototype._range2, null, [
               range2Min, range2Max, range2Source
             ]);

             rangeKept = rangeMin = rangeMax = rangeSource = rangeMiddle = null;
             range2Kept = range2Min = range2Max = range2Source
               = bipolarMiddle = null;
             return stack(ranged, ranged2);
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw custom-chain results lost JavaScript-owned values"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawRangeKeptAlive = Number(
                 rawRangeKeptRef.deref()?.marker === 'raw-range-owned'
               );
               globalThis.rawRange2KeptAlive = Number(
                 rawRange2KeptRef.deref()?.marker === 'raw-range2-owned'
               );
               globalThis.rawRangeChainPruned = Number(
                 rawRangeDroppedRefs.every(ref => ref.deref() === undefined)
                 && rawRange2DroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawRangeKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawRange2KeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawRangeChainPruned"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-range-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-range2-owned"))
    );
    assert_clean(&owned, "raw range pair returned ownership");
    owned.clear_active();
}

#[test]
fn raw_range_pair_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             fastcat(pure(0), pure(.5), pure(1)).setSteps(3)._range(2, 6),
             fastcat(pure(-1), pure(0), pure(1)).setSteps(3)._range2(2, 6)
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw range pair charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw range pair charged query-time stepwise work"
    );
    assert_clean(&direct, "raw range pair pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return pure(.5)._range(2, 6);
             }}
             if (value === 1) {{
               return pure(0)._range2(2, 6);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw range pair consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw range pair shared-pool neutrality");
    shared.clear_active();
}
