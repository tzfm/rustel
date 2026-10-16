use super::*;

#[test]
fn raw_shrink_grow_keep_direct_body_routing_under_surface_mutation() {
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
                   const rawShrink = Pattern.prototype._shrink;
                   const rawGrow = Pattern.prototype._grow;
                   const receiver = sequence('a', 'b', 'c', 'd');

                   // The raw wrapper appends its receiver after the caller's
                   // rest arguments. Omission therefore leaves `pat`
                   // undefined, while an explicit undefined is still an
                   // amount and leaves the receiver in the `pat` position.
                   phase = 'omission';
                   for (const [name, raw] of [
                     ['_shrink', rawShrink], ['_grow', rawGrow]
                   ]) {
                     let omitted;
                     try { apply(raw, receiver, []); }
                     catch (error) { omitted = error; }
                     check(omitted instanceof TypeError,
                       `${name} omission did not reach undefined pat`);
                   }
                   phase = 'explicit-undefined';
                   check(apply(rawShrink, receiver, [undefined])._steps.show()
                     === '16/1', '_shrink(undefined) collapsed to omission');
                   check(apply(rawGrow, receiver, [undefined])._steps.show()
                     === '16/1', '_grow(undefined) collapsed to omission');

                   phase = 'pattern-rejection';
                   const patternedAmount = sequence(1, 2);
                   for (const [name, raw] of [
                     ['_shrink', rawShrink], ['_grow', rawGrow]
                   ]) {
                     let rejected;
                     try { apply(raw, receiver, [patternedAmount]); }
                     catch (error) { rejected = error; }
                     check(rejected?.name === 'Error'
                       && rejected.message === 'Invalid argument',
                       `${name} patternified its raw amount`);
                   }

                   // Two or more explicit arguments retarget the body's
                   // second parameter; later values and the wrapper receiver
                   // are ignored by the two-parameter canonical closure.
                   phase = 'retarget';
                   const decoy = sequence('decoy-a', 'decoy-b');
                   decoy.shrinklist = function () {
                     throw new Error('wrapper receiver reached');
                   };
                   const target = sequence('target-a', 'target-b');
                   const targetCalls = [];
                   target.shrinklist = function (amount) {
                     targetCalls.push({ self: this, amount });
                     return [gap(3)];
                   };
                   const pair = [1, 2];
                   check(apply(rawShrink, decoy, [pair, target])._steps.show()
                     === '3/1', '_shrink did not retarget to arg two');
                   check(apply(rawGrow, decoy, [pair, target, 'ignored'])
                     ._steps.show() === '3/1',
                     '_grow did not ignore arguments after its target');
                   check(targetCalls.length === 2
                     && targetCalls.every(call => call.self === target)
                     && targetCalls[0].amount === pair
                     && targetCalls[1].amount instanceof Fraction._original
                     && targetCalls[1].amount !== pair
                     && targetCalls[1].amount.show() === '-1/2',
                     'retargeted helper arguments');

                   phase = 'no-steps';
                   const lexicalNothing = nothing;
                   const noSteps = new Pattern(() => []);
                   check(apply(rawShrink, decoy, [Symbol('ignored'), noSteps])
                     === lexicalNothing, '_shrink no-step shortcut');
                   check(apply(rawGrow, decoy, [Symbol('ignored'), noSteps])
                     === lexicalNothing, '_grow no-step shortcut');

                   phase = 'shrink-helper-identity';
                   const shrinkSource = sequence('a', 'b', 'c', 'd');
                   const shrinkPair = [1, 2];
                   let shrinkArgument;
                   let shrinkThis;
                   shrinkSource.shrinklist = function (amount) {
                     shrinkThis = this;
                     shrinkArgument = amount;
                     return [gap(2), gap(3)];
                   };
                   check(apply(rawShrink, shrinkSource, [shrinkPair])
                     ._steps.show() === '5/1', 'raw shrink result steps');
                   check(shrinkThis === shrinkSource
                     && shrinkArgument === shrinkPair,
                     'raw shrink changed helper receiver or pair identity');

                   // Member lookup precedes grow's Fraction negation; the
                   // helper then returns the exact Array reversed in place
                   // before that same Array performs the step reduction.
                   phase = 'grow-order';
                   const growSource = sequence('a', 'b', 'c', 'd');
                   const left = gap(2);
                   const right = gap(3);
                   const returned = [left, right];
                   const order = [];
                   const savedReverse = Array.prototype.reverse;
                   const savedReduce = Array.prototype.reduce;
                   const savedSub = Fraction._original.prototype.sub;
                   let growArgument;
                   Object.defineProperty(growSource, 'shrinklist', {
                     configurable: true,
                     get() {
                       order.push('get');
                       return function (amount) {
                         order.push(`call:${this === growSource}`);
                         growArgument = amount;
                         return returned;
                       };
                     },
                   });
                   returned.reverse = function (...args) {
                     order.push(`reverse:${this === returned}:${this[0] === left}`);
                     return apply(savedReverse, this, args);
                   };
                   returned.reduce = function (...args) {
                     order.push(`reduce:${this === returned}:${this[0] === right}`);
                     return apply(savedReduce, this, args);
                   };
                   Fraction._original.prototype.sub = function (...args) {
                     order.push('fraction');
                     return apply(savedSub, this, args);
                   };
                   let orderedGrow;
                   try {
                     orderedGrow = apply(rawGrow, growSource, [[1, 2]]);
                   } finally {
                     Fraction._original.prototype.sub = savedSub;
                   }
                   check(order.join('|') ===
                     'get|fraction|call:true|reverse:true:true|reduce:true:true',
                     `raw grow order: ${order}`);
                   check(returned[0] === right && returned[1] === left,
                     'raw grow did not reverse the helper Array in place');
                   check(growArgument instanceof Fraction._original
                     && growArgument.show() === '-1/2'
                     && orderedGrow._steps.show() === '5/1',
                     'raw grow Fraction or result steps');

                   // The registered body and raw wrapper are lexical. Public
                   // free functions, public methods, and same-named globals
                   // are not redispatched after the raw method is captured.
                   phase = 'lexical-poison';
                   const savedGlobals = {
                     Fraction: globalThis.Fraction,
                     stepcat: globalThis.stepcat,
                     nothing: globalThis.nothing,
                   };
                   const savedMethods = {
                     shrink: Pattern.prototype.shrink,
                     grow: Pattern.prototype.grow,
                   };
                   const lexicalSource = sequence('a', 'b');
                   let lexicalCalls = 0;
                   lexicalSource.shrinklist = function () {
                     lexicalCalls++;
                     return [gap(2)];
                   };
                   const poison = function () {
                     throw new Error('poisoned public surface');
                   };
                   let lexicalShrink;
                   let lexicalGrow;
                   let lexicalNoSteps;
                   globalThis.Fraction = poison;
                   globalThis.stepcat = poison;
                   globalThis.nothing = poison;
                   Pattern.prototype.shrink = poison;
                   Pattern.prototype.grow = poison;
                   try {
                     lexicalShrink = apply(rawShrink, lexicalSource, [1]);
                     lexicalGrow = apply(rawGrow, lexicalSource, [1]);
                     lexicalNoSteps = apply(
                       rawShrink, lexicalSource, [Symbol('ignored'), noSteps]
                     );
                   } finally {
                     globalThis.Fraction = savedGlobals.Fraction;
                     globalThis.stepcat = savedGlobals.stepcat;
                     globalThis.nothing = savedGlobals.nothing;
                     Pattern.prototype.shrink = savedMethods.shrink;
                     Pattern.prototype.grow = savedMethods.grow;
                   }
                   check(lexicalCalls === 2
                     && lexicalShrink._steps.show() === '2/1'
                     && lexicalGrow._steps.show() === '2/1'
                     && lexicalNoSteps === lexicalNothing,
                     'raw bodies redispatched through a public surface');
                   globalThis.rawDirectRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawDirectRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawDirectRoutingError"), None);
    assert_eq!(runtime.get_number("rawDirectRoutingOkay"), Some(1.0));
    assert_clean(&runtime, "raw direct routing");
}

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
fn canonical_fast_path_stays_private_and_falls_back_for_observable_lists() {
    rustel_core::reset_stepwise_entries_materialised();
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
                   const savedHelper = Pattern.prototype.shrinklist;
                   const savedReverse = Array.prototype.reverse;
                   const savedReduce = Array.prototype.reduce;

                   // A wrapper around the saved canonical helper is itself an
                   // observable helper. It must receive a fully materialised
                   // list, and grow must reverse and reduce that same Array.
                   phase = 'wrapped-helper';
                   const wrappedSource = sequence('a', 'b', 'c', 'd');
                   let wrappedList;
                   let wrappedFirst;
                   let wrappedLast;
                   let reverseThis;
                   let reduceThis;
                   let beforeReverse;
                   let beforeReduce;
                   wrappedSource.shrinklist = function (amount) {
                     wrappedList = savedHelper.call(this, amount);
                     wrappedFirst = wrappedList[0];
                     wrappedLast = wrappedList[wrappedList.length - 1];
                     wrappedList.reverse = function (...args) {
                       reverseThis = this;
                       beforeReverse = this[0];
                       return apply(savedReverse, this, args);
                     };
                     wrappedList.reduce = function (...args) {
                       reduceThis = this;
                       beforeReduce = this[0];
                       return apply(savedReduce, this, args);
                     };
                     return wrappedList;
                   };
                   const wrappedGrow = wrappedSource.grow(1);
                   check(wrappedList.length === 4, 'wrapped helper saw a sentinel');
                   check(reverseThis === wrappedList && reduceThis === wrappedList,
                     'grow changed the helper Array identity');
                   check(beforeReverse === wrappedFirst
                     && beforeReduce === wrappedLast
                     && wrappedList[0] === wrappedLast,
                     'grow did not reverse the helper Array before reducing it');
                   check(wrappedGrow._steps.show() === '10/1',
                     'wrapped helper changed grow steps');

                   // Mutating the exact Array primitives observed by the
                   // canonical bodies must disable substitution and expose a
                   // real four-entry helper list to those mutations.
                   phase = 'reverse-mutation';
                   const reverseLengths = [];
                   Array.prototype.reverse = function (...args) {
                     reverseLengths.push(this.length);
                     return apply(savedReverse, this, args);
                   };
                   let reverseGrow;
                   try {
                     reverseGrow = sequence('a', 'b', 'c', 'd').grow(1);
                   } finally {
                     Array.prototype.reverse = savedReverse;
                   }
                   check(reverseLengths.includes(4),
                     `mutated reverse missed helper list: ${reverseLengths}`);
                   check(reverseGrow._steps.show() === '10/1',
                     'reverse fallback changed grow steps');

                   phase = 'reduce-mutation';
                   const reduceLengths = [];
                   Array.prototype.reduce = function (...args) {
                     reduceLengths.push(this.length);
                     return apply(savedReduce, this, args);
                   };
                   let reduceShrink;
                   try {
                     reduceShrink = sequence('a', 'b', 'c', 'd').shrink(1);
                   } finally {
                     Array.prototype.reduce = savedReduce;
                   }
                   check(reduceLengths.includes(4),
                     `mutated reduce missed helper list: ${reduceLengths}`);
                   check(reduceShrink._steps.show() === '10/1',
                     'reduce fallback changed shrink steps');

                   // An accessor zoom is another observable surface and must
                   // be read and called once for every materialised entry.
                   phase = 'zoom-accessor';
                   const zoomSource = sequence('a', 'b', 'c', 'd');
                   const savedZoom = zoomSource.zoom;
                   let zoomGets = 0;
                   let zoomCalls = 0;
                   Object.defineProperty(zoomSource, 'zoom', {
                     configurable: true,
                     get() {
                       zoomGets++;
                       return function (...args) {
                         zoomCalls++;
                         return apply(savedZoom, this, args);
                       };
                     },
                   });
                   const zoomShrink = zoomSource.shrink(1);
                   check(zoomGets === 4 && zoomCalls === 4,
                     `zoom fallback counts: ${zoomGets}/${zoomCalls}`);
                   check(zoomShrink instanceof Pattern,
                     'zoom fallback did not return a Pattern');

                   // The helper's strudel.cc Array.isArray(amount) lookup stays
                   // dynamic and sees the exact pair. Private preflight uses
                   // its captured intrinsic and must not expose the empty
                   // handoff Array as a second argument to this observer.
                   phase = 'array-is-array-poison';
                   const arrayCheckSource = sequence('a', 'b', 'c', 'd');
                   const pair = [1, 4];
                   const savedArrayIsArray = Array.isArray;
                   const arrayCheckArguments = [];
                   let arrayCheckShrink;
                   Array.isArray = function (value) {
                     arrayCheckArguments.push(value);
                     return savedArrayIsArray(value);
                   };
                   try {
                     arrayCheckShrink = arrayCheckSource.shrink(pair);
                   } finally {
                     Array.isArray = savedArrayIsArray;
                   }
                   check(arrayCheckArguments.length === 1
                     && arrayCheckArguments[0] === pair,
                     `private preflight reached dynamic Array.isArray: ${arrayCheckArguments.length}`);
                   check(arrayCheckShrink._steps.show() === '10/1',
                     'Array.isArray poison changed canonical shrink');

                   // The private marker uses captured WeakMap operations, so
                   // post-install prototype poisoning cannot observe or alter
                   // the otherwise eligible empty handoff Array.
                   phase = 'weakmap-poison';
                   const savedWeakGet = WeakMap.prototype.get;
                   const savedWeakSet = WeakMap.prototype.set;
                   let weakGets = 0;
                   let weakSets = 0;
                   let privateShrink;
                   let poisonError;
                   WeakMap.prototype.get = function () {
                     weakGets++;
                     throw new Error('dynamic WeakMap.get reached');
                   };
                   WeakMap.prototype.set = function () {
                     weakSets++;
                     throw new Error('dynamic WeakMap.set reached');
                   };
                   try {
                     privateShrink = sequence('a', 'b', 'c', 'd').shrink(1);
                   } catch (error) {
                     poisonError = error;
                   } finally {
                     WeakMap.prototype.get = savedWeakGet;
                     WeakMap.prototype.set = savedWeakSet;
                   }
                   check(poisonError === undefined,
                     `private marker escaped: ${poisonError}`);
                   check(weakGets === 0 && weakSets === 0,
                     `dynamic WeakMap operations: ${weakGets}/${weakSets}`);
                   check(privateShrink._steps.show() === '10/1',
                     'WeakMap poison changed canonical shrink');
                   globalThis.canonicalFastPathFallbackOkay = 1;
                 } catch (error) {
                   globalThis.canonicalFastPathFallbackError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("canonicalFastPathFallbackError"), None);
    assert_eq!(
        runtime.get_number("canonicalFastPathFallbackOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "canonical fast-path fallbacks");
    rustel_core::reset_stepwise_entries_materialised();
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
        runtime.active_pattern().expect("patterned shrink").steps,
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
fn patterned_shrink_observes_a_post_construction_prototype_replacement() {
    let runtime = semantic_runtime(
        r#"(() => {
             const source = sequence('a', 'b', 'c', 'd');
             const result = source.shrink(sequence(1, 2));
             globalThis.patternedShrinkSavedPrototype = Pattern.prototype.shrinklist;
             globalThis.patternedShrinkPrototypeHits = 0;
             globalThis.patternedShrinkPrototypeMismatch = 0;
             Pattern.prototype.shrinklist = function (amount) {
               globalThis.patternedShrinkPrototypeHits++;
               if (this !== source) globalThis.patternedShrinkPrototypeMismatch++;
               return [pure(`prototype-${amount}`).setSteps(3)];
             };
             return result;
           })()"#,
    );
    assert!(runtime.active_needs_host());
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(
        runtime.get_number("patternedShrinkPrototypeHits"),
        Some(2.0)
    );
    assert_eq!(
        runtime.get_number("patternedShrinkPrototypeMismatch"),
        Some(0.0)
    );
    assert!(values.iter().any(|value| value == "prototype-1"));
    assert!(values.iter().any(|value| value == "prototype-2"));
    runtime
        .eval("Pattern.prototype.shrinklist = patternedShrinkSavedPrototype;")
        .unwrap();
    assert_clean(&runtime, "patterned shrink prototype replacement");
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

#[test]
fn canonical_shrink_shares_one_query_pool_without_default_helper_double_charge() {
    let exact = semantic_runtime("fastcat(pure(0), pure(1)).polyBind(() => gap(8192).shrink(0))");
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "two canonical helper arrays did not consume exactly one shared pool"
    );
    assert_clean(&exact, "canonical shrink cumulative exact side");
    exact.clear_active();

    let refused = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(value =>
             value === 0
               ? stack(...gap(8000).shrinklist(0))
               : gap(8385).shrink(0)
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&refused, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_000,
        "canonical shrink materialised after the mixed shared-pool refusal"
    );
    assert_clean(&refused, "mixed helper/canonical shrink refusal");

    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_000);
    assert_clean(&refused, "mixed helper/canonical shrink recovery");
    refused.clear_active();
}
