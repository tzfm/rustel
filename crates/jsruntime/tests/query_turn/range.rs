use super::*;

#[test]
fn raw_range_pair_keep_surface_forwarding_and_dynamic_body_order() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_prelude(
            r#"(() => {
                 let phase = 'setup';
                 try {
                   const check = (condition, message) => {
                     if (!condition) throw new Error(message);
                   };
                   const apply = Reflect.apply;
                   const rawRange = Pattern.prototype._range;
                   const rawRange2 = Pattern.prototype._range2;

                   phase = 'surface';
                   check(typeof rawRange === 'function'
                     && typeof rawRange2 === 'function'
                     && rawRange !== Pattern.prototype.range
                     && rawRange2 !== Pattern.prototype.range2
                     && rawRange !== rawRange2, 'raw identities');
                   for (const [name, raw] of [
                     ['_range', rawRange], ['_range2', rawRange2]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} prototype descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${name} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(
                         raw, 'prototype'
                       ), `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'range', '_range', 'rangex', '_rangex',
                       'range2', '_range2'
                     ].includes(name)).join('|');
                   check(rawOrder === 'range|_range|rangex|range2|_range2',
                     `raw range order: ${rawOrder}`);
                   check(!('_rangex' in Pattern.prototype)
                     && !('_rangex' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_rangex'
                     ), '_rangex was inferred');
                   check(typeof globalThis.range === 'function'
                     && typeof globalThis.rangex === 'function'
                     && typeof globalThis.range2 === 'function'
                     && typeof Pattern.prototype.range === 'function'
                     && typeof Pattern.prototype.rangex === 'function'
                     && typeof Pattern.prototype.range2 === 'function',
                     'raw install replaced public registrations');

                   phase = 'range-body-order';
                   const rangeLog = [];
                   const min = {
                     valueOf() {
                       rangeLog.push(`min.valueOf:${this === min}`);
                       return 2;
                     },
                   };
                   const max = {
                     valueOf() {
                       rangeLog.push(`max.valueOf:${this === max}`);
                       return 9;
                     },
                   };
                   const rangeTerminal = { marker: 'range-terminal' };
                   const afterMul = {};
                   Object.defineProperty(afterMul, 'add', {
                     configurable: true,
                     get() {
                       rangeLog.push('add.get');
                       return function (received) {
                         rangeLog.push(`add.call:${this === afterMul}`
                           + `:${received === min}`);
                         return rangeTerminal;
                       };
                     },
                   });
                   const rangeTarget = {};
                   Object.defineProperty(rangeTarget, 'mul', {
                     configurable: true,
                     get() {
                       rangeLog.push('mul.get');
                       return function (difference) {
                         rangeLog.push(`mul.call:${this === rangeTarget}`
                           + `:${difference}`);
                         return afterMul;
                       };
                     },
                   });
                   const decoy = {};
                   Object.defineProperty(decoy, 'mul', {
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   check(apply(rawRange, decoy, [
                     min, max, rangeTarget, 'ignored'
                   ]) === rangeTerminal, '_range replaced terminal result');
                   check(rangeLog.join('|') === [
                     'mul.get', 'max.valueOf:true', 'min.valueOf:true',
                     'mul.call:true:7', 'add.get', 'add.call:true:true'
                   ].join('|'), `_range body order: ${rangeLog}`);

                   phase = 'range2-body-order';
                   const range2Log = [];
                   const range2Terminal = { marker: 'range2-terminal' };
                   const bipolarResult = {};
                   Object.defineProperty(bipolarResult, '_range', {
                     configurable: true,
                     get() {
                       range2Log.push('_range.get');
                       return function (receivedMin, receivedMax) {
                         range2Log.push(`_range.call:${this === bipolarResult}`
                           + `:${receivedMin === min}:${receivedMax === max}`);
                         return range2Terminal;
                       };
                     },
                   });
                   const range2Target = {};
                   Object.defineProperty(range2Target, 'fromBipolar', {
                     configurable: true,
                     get() {
                       range2Log.push('fromBipolar.get');
                       return function () {
                         range2Log.push(
                           `fromBipolar.call:${this === range2Target}`
                         );
                         return bipolarResult;
                       };
                     },
                   });
                   Object.defineProperty(decoy, 'fromBipolar', {
                     get() {
                       throw new Error('range2 wrapper receiver reached');
                     },
                   });
                   check(apply(rawRange2, decoy, [
                     min, max, range2Target, 'ignored'
                   ]) === range2Terminal,
                   '_range2 replaced terminal result');
                   check(range2Log.join('|') === [
                     'fromBipolar.get', 'fromBipolar.call:true',
                     '_range.get', '_range.call:true:true:true'
                   ].join('|'), `_range2 body order: ${range2Log}`);

                   phase = 'receiver-and-constructors';
                   const receiverTerminal = { marker: 'receiver-terminal' };
                   const receiverTarget = {
                     mul(difference) {
                       check(this === receiverTarget && difference === 4,
                         'method receiver or difference changed');
                       return {
                         add(received) {
                           check(received === 2,
                             'method original min changed');
                           return receiverTerminal;
                         },
                       };
                     },
                   };
                   check(apply(rawRange, receiverTarget, [2, 6])
                     === receiverTerminal,
                     'two explicit arguments did not use wrapper receiver');
                   check(Reflect.construct(rawRange, [
                     2, 6, receiverTarget
                   ]) === receiverTerminal,
                   '_range constructor did not return object terminal');
                   const primitiveRange2Target = {
                     fromBipolar() {
                       return { _range() { return 7; } };
                     },
                   };
                   const primitiveConstruction = Reflect.construct(
                     rawRange2, [2, 6, primitiveRange2Target]
                   );
                   check(primitiveConstruction instanceof rawRange2,
                     '_range2 constructor returned primitive terminal');
                   let omitted;
                   try { apply(rawRange, receiverTarget, [2]); }
                   catch (error) { omitted = error; }
                   check(omitted instanceof TypeError,
                     'omission did not leave the body target undefined');

                   phase = 'throw-cutoffs';
                   let coercionHits = 0;
                   const cutoffMin = {
                     valueOf() { coercionHits++; return 2; },
                   };
                   const cutoffMax = {
                     valueOf() { coercionHits++; return 6; },
                   };
                   const getterStop = {};
                   Object.defineProperty(getterStop, 'mul', {
                     get() { throw new Error('mul-get-stop'); },
                   });
                   let getterError;
                   try {
                     apply(rawRange, null, [
                       cutoffMin, cutoffMax, getterStop
                     ]);
                   } catch (error) { getterError = error; }
                   check(getterError?.message === 'mul-get-stop'
                     && coercionHits === 0,
                     'mul getter did not precede subtraction coercion');

                   const cutoffLog = [];
                   const maxStop = {
                     valueOf() {
                       cutoffLog.push('max.valueOf');
                       throw new Error('max-stop');
                     },
                   };
                   const minAfterStop = {
                     valueOf() { cutoffLog.push('min.valueOf'); return 2; },
                   };
                   const callAfterStop = {};
                   Object.defineProperty(callAfterStop, 'mul', {
                     get() {
                       cutoffLog.push('mul.get');
                       return function () { cutoffLog.push('mul.call'); };
                     },
                   });
                   let coercionError;
                   try {
                     apply(rawRange, null, [
                       minAfterStop, maxStop, callAfterStop
                     ]);
                   } catch (error) { coercionError = error; }
                   check(coercionError?.message === 'max-stop'
                     && cutoffLog.join('|') === 'mul.get|max.valueOf',
                     `subtraction cutoff changed: ${cutoffLog}`);

                   let range2Calls = 0;
                   const range2GetterStop = {};
                   Object.defineProperty(range2GetterStop, 'fromBipolar', {
                     get() { throw new Error('from-bipolar-stop'); },
                   });
                   let range2Error;
                   try {
                     apply(rawRange2, null, [
                       { valueOf() { range2Calls++; return 2; } },
                       { valueOf() { range2Calls++; return 6; } },
                       range2GetterStop
                     ]);
                   } catch (error) { range2Error = error; }
                   check(range2Error?.message === 'from-bipolar-stop'
                     && range2Calls === 0,
                     'fromBipolar getter did not cut off later phases');

                   phase = 'configured-parser';
                   const parserLog = [];
                   setStringParser(value => {
                     parserLog.push(value);
                     return pure(Number(value));
                   });
                   const parsedRange = pure(.5)._range('2', '4');
                   check(parserLog.join('|') === '2',
                     `_range parser phase: ${parserLog}`);
                   const parsedRange2 = pure(0)._range2('2', '4');
                   check(parserLog.join('|') === '2|2',
                     `_range2 parser phase: ${parserLog}`);
                   const state = {
                     span: { begin: 0, end: 1 }, controls: {},
                   };
                   const parsedValues = [
                     parsedRange.query(state)[0]?.value,
                     parsedRange2.query(state)[0]?.value,
                   ];
                   check(parsedValues[0] === 3 && parsedValues[1] === 3
                     && parserLog.join('|') === '2|2',
                     `parser reread or values changed: ${parserLog}`
                       + `:${parsedValues}`);
                   globalThis.rawRangePairSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawRangePairSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawRangePairSurfaceError"), None);
    assert_eq!(runtime.get_number("rawRangePairSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw range pair surface and routing");
}

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
        assert_eq!(pattern.steps, Some(Fraction::int(3)), "{label}: steps");
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
                    hap.part.begin,
                    hap.part.end,
                    hap.whole.expect("raw range whole").begin,
                    hap.whole.expect("raw range whole").end,
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

/// The bounded raw pair is literal eager composition on stable native
/// receivers. Upstream query functions are receiver-independent closures, but
/// native public `mul`/`fromBipolar` still read the wrapper's stored graph.
/// Keep reassigned native and custom query functions executable as a named
/// residual instead of silently extending this slice to all public methods.
#[test]
fn raw_range_query_replacement_remains_an_explicit_residual() {
    let runtime = semantic_runtime(
        r#"(() => {
             const nativeSource = pure(.5);
             const nativeOwner = pure(.25);
             nativeSource.query = nativeOwner.query;

             const customSource = pure(.5);
             customSource.query = state => pure(.25).query(state);
             return stack(
               nativeSource._range(2, 6),
               nativeSource._range2(2, 6),
               customSource._range(2, 6),
               customSource._range2(2, 6)
             );
           })()"#,
    );
    assert!(
        !runtime.active_needs_host(),
        "the current residual unexpectedly retained replacement queries"
    );
    assert_eq!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>(),
        ["4", "5", "4", "5"],
        "native residual changed; pinned Node produces 3/4.5 for each pair"
    );
    assert_clean(&runtime, "raw range query replacement residual");
    runtime.clear_active();
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
