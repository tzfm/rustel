use super::*;

#[test]
fn raw_take_drop_keep_direct_coercion_and_mutation_order() {
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
                   const rawTake = Pattern.prototype._take;
                   const rawDrop = Pattern.prototype._drop;
                   const base = sequence('a', 'b', 'c', 'd');

                   phase = 'surface';
                   check(Pattern.prototype.s_add === Pattern.prototype.take
                     && Pattern.prototype.s_sub === Pattern.prototype.drop,
                     'raw install replaced a copied canonical method');
                   check(rawTake !== Pattern.prototype.take
                     && rawDrop !== Pattern.prototype.drop
                     && rawTake !== rawDrop, 'raw identities');
                   for (const [name, raw] of [
                     ['_take', rawTake], ['_drop', rawDrop]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} descriptor`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(raw, 'prototype'),
                       `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'take', '_take', 'drop', '_drop', 's_add', 's_sub'
                     ].includes(name)).join('|');
                   check(rawOrder === 'take|_take|drop|_drop|s_add|s_sub',
                     `raw take/drop order: ${rawOrder}`);
                   check(['_s_add', '_s_sub', '_pace'].every(name =>
                       !(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       )
                       && !(name in Pattern.prototype)
                     ), 'adjacent raw surface expanded');

                   phase = 'omission';
                   for (const [name, raw] of [
                     ['_take', rawTake], ['_drop', rawDrop]
                   ]) {
                     let omitted;
                     try { apply(raw, base, []); }
                     catch (error) { omitted = error; }
                     check(omitted instanceof TypeError,
                       `${name} omission did not leave pat undefined`);
                   }

                   phase = 'undefined-and-pair';
                   check(apply(rawTake, base, [undefined]) === nothing,
                     '_take(undefined) did not return lexical nothing');
                   check(apply(rawTake, base, [0]) === nothing
                     && apply(rawTake, base, [4]) === base,
                     '_take lost its direct-body return identities');
                   for (const [amount, steps] of [
                     [undefined, '4/1'], [0, '4/1'], [4, '0/1'],
                     [1, '3/1'], [-1, '3/1'], [6, '2/1'], [-6, '2/1']
                   ]) {
                     const result = apply(rawDrop, base, [amount]);
                     check(result !== base && result !== nothing
                       && result._steps.show() === steps,
                       `_drop(${amount}) finalizer identity/steps: ${
                         result === base
                       }:${result === nothing}:${result?._steps?.show?.()}`);
                   }
                   const zeroSource = gap(0);
                   const zeroDrop = apply(rawDrop, zeroSource, [1]);
                   check(zeroDrop !== zeroSource && zeroDrop !== nothing
                     && zeroDrop._steps.show() === '0/1',
                     '_drop zero-step source lost registered finalization');
                   check(apply(rawTake, base, [[1, 2]])._steps.show()
                     === '1/2', '_take pair was not Fraction 1/2');
                   check(apply(rawDrop, base, [[1, 2]])._steps.show()
                     === '7/2', '_drop pair was not Fraction 1/2');

                   phase = 'pattern-rejection';
                   const patterned = sequence(1, 2);
                   for (const [name, raw] of [
                     ['_take', rawTake], ['_drop', rawDrop]
                   ]) {
                     let rejected;
                     try { apply(raw, base, [patterned]); }
                     catch (error) { rejected = error; }
                     check(rejected?.name === 'Error'
                       && rejected.message === 'Invalid argument',
                       `${name} patternified its raw amount`);
                   }

                   phase = 'retarget';
                   const decoy = sequence('decoy');
                   Object.defineProperty(decoy, 'hasSteps', {
                     configurable: true,
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   const target = sequence('x', 'y', 'z');
                   check(apply(rawTake, decoy, [1, target])._steps.show()
                     === '1/1', '_take did not retarget to arg two');
                   check(apply(rawDrop, decoy, [1, target, 'ignored'])
                     ._steps.show() === '2/1',
                     '_drop did not ignore arguments after its target');
                   check(new rawTake(1) === nothing,
                     'constructed _take did not return lexical nothing');
                   check(new rawDrop(1, target)._steps.show() === '2/1',
                     'constructed _drop did not retarget');

                   phase = 'early-returns';
                   const noSteps = new Pattern(() => []);
                   const poisonPair = [];
                   for (const index of [0, 1]) {
                     Object.defineProperty(poisonPair, index, {
                       get() { throw new Error('no-step pair read'); },
                     });
                   }
                   check(apply(rawTake, noSteps, [poisonPair]) === nothing
                     && apply(rawDrop, noSteps, [poisonPair]) === nothing,
                     'no-step target did not bypass coercion');
                   check(apply(rawTake, gap(-1), [Symbol('ignored')])
                     === nothing,
                     '_take did not guard nonpositive steps before coercion');
                   let nonpositiveDrop;
                   try { apply(rawDrop, gap(-1), [Symbol('observed')]); }
                   catch (error) { nonpositiveDrop = error; }
                   check(nonpositiveDrop?.name === 'Error'
                     && nonpositiveDrop.message === 'Invalid argument',
                     '_drop incorrectly inherited the take guard');

                   phase = 'take-order';
                   const takeLog = [];
                   const takeSteps = Fraction(4);
                   const takePair = [-1, 2];
                   Object.defineProperty(takePair, 0, {
                     configurable: true,
                     get() { takeLog.push('pair[0]'); return -1; },
                   });
                   Object.defineProperty(takePair, 1, {
                     configurable: true,
                     get() { takeLog.push('pair[1]'); return 2; },
                   });
                   const takeToken = {};
                   const takeTarget = {
                     get hasSteps() { takeLog.push('hasSteps'); return true; },
                     get _steps() { takeLog.push('_steps'); return takeSteps; },
                     get zoom() {
                       takeLog.push('zoom.get');
                       return function (begin, end) {
                         takeLog.push(`zoom.call:${this === takeTarget}`
                           + `:${begin.show()}:${end}`);
                         return takeToken;
                       };
                     },
                   };
                   const fractionPrototype = Fraction._original.prototype;
                   const takeOriginals = {};
                   for (const name of ['lte', 'eq', 'abs', 'div', 'gte', 'sub']) {
                     takeOriginals[name] = fractionPrototype[name];
                     fractionPrototype[name] = function (...args) {
                       takeLog.push(`fraction.${name}`);
                       return apply(takeOriginals[name], this, args);
                     };
                   }
                   let takeOrdered;
                   try {
                     takeOrdered = apply(rawTake, null, [takePair, takeTarget]);
                   } finally {
                     for (const name of Object.keys(takeOriginals)) {
                       fractionPrototype[name] = takeOriginals[name];
                     }
                   }
                   check(takeOrdered === takeToken, '_take replaced zoom result');
                   check(takeLog.join('|') === [
                     'hasSteps', '_steps', 'fraction.lte',
                     'pair[0]', 'pair[1]', 'fraction.eq', 'fraction.abs',
                     '_steps', 'fraction.div', 'fraction.lte', 'fraction.gte',
                     'zoom.get', 'fraction.sub', 'zoom.call:true:7/8:1'
                   ].join('|'), `_take order: ${takeLog}`);

                   phase = 'drop-order';
                   const dropLog = [];
                   const dropSteps = Fraction(4);
                   const dropPair = [1, 2];
                   Object.defineProperty(dropPair, 0, {
                     configurable: true,
                     get() { dropLog.push('pair[0]'); return 1; },
                   });
                   Object.defineProperty(dropPair, 1, {
                     configurable: true,
                     get() { dropLog.push('pair[1]'); return 2; },
                   });
                   const dropToken = {};
                   const dropTarget = {
                     get hasSteps() { dropLog.push('hasSteps'); return true; },
                     get _steps() { dropLog.push('_steps'); return dropSteps; },
                     get take() {
                       dropLog.push('take.get');
                       return function (amount) {
                         dropLog.push(`take.call:${this === dropTarget}`
                           + `:${amount.show()}`);
                         return dropToken;
                       };
                     },
                   };
                   const dropOriginals = {};
                   for (const name of ['lt', 'add', 'sub']) {
                     dropOriginals[name] = fractionPrototype[name];
                     fractionPrototype[name] = function (...args) {
                       dropLog.push(`fraction.${name}`);
                       return apply(dropOriginals[name], this, args);
                     };
                   }
                   let dropOrdered;
                   try {
                     dropOrdered = apply(rawDrop, null, [dropPair, dropTarget]);
                   } finally {
                     for (const name of Object.keys(dropOriginals)) {
                       fractionPrototype[name] = dropOriginals[name];
                     }
                   }
                   check(dropOrdered === dropToken, '_drop replaced take result');
                   check(dropLog.join('|') === [
                     'hasSteps', 'pair[0]', 'pair[1]', 'fraction.lt',
                     'take.get', '_steps', 'fraction.sub', 'fraction.sub',
                     'take.call:true:-7/2'
                   ].join('|'), `_drop order: ${dropLog}`);

                   phase = 'dynamic-public-take';
                   const savedTakeMethod = Pattern.prototype.take;
                   const savedAddAlias = Pattern.prototype.s_add;
                   let dynamicTakeHits = 0;
                   let dynamicDrop;
                   Pattern.prototype.take = function (amount) {
                     dynamicTakeHits++;
                     check(this === base && amount.show() === '-3/1',
                       'dynamic take receiver or amount');
                     return gap(2);
                   };
                   Pattern.prototype.s_add = function () {
                     throw new Error('s_add decoy reached');
                   };
                   try {
                     dynamicDrop = apply(rawDrop, base, [1]);
                   } finally {
                     Pattern.prototype.take = savedTakeMethod;
                     Pattern.prototype.s_add = savedAddAlias;
                   }
                   check(dynamicTakeHits === 1
                     && dynamicDrop._steps.show() === '2/1',
                     '_drop did not use the current public take once');

                   phase = 'lexical-poison';
                   const savedGlobals = {
                     Fraction: globalThis.Fraction,
                     nothing: globalThis.nothing,
                     take: globalThis.take,
                     drop: globalThis.drop,
                   };
                   const savedScope = {
                     take: rustelScope.take,
                     drop: rustelScope.drop,
                   };
                   const lexicalNothing = nothing;
                   const poison = function () {
                     throw new Error('public lexical decoy reached');
                   };
                   let lexicalTake;
                   let lexicalDrop;
                   let lexicalNoSteps;
                   globalThis.Fraction = poison;
                   globalThis.nothing = poison;
                   globalThis.take = poison;
                   globalThis.drop = poison;
                   rustelScope.take = poison;
                   rustelScope.drop = poison;
                   try {
                     lexicalTake = apply(rawTake, base, [1]);
                     lexicalDrop = apply(rawDrop, base, [1]);
                     lexicalNoSteps = apply(rawTake, noSteps, [Symbol('x')]);
                   } finally {
                     Object.assign(globalThis, savedGlobals);
                     Object.assign(rustelScope, savedScope);
                   }
                   check(lexicalTake._steps.show() === '1/1'
                     && lexicalDrop._steps.show() === '3/1'
                     && lexicalNoSteps === lexicalNothing,
                     'raw take/drop lost lexical Fraction or nothing');
                   globalThis.rawTakeDropRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawTakeDropRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawTakeDropRoutingError"), None);
    assert_eq!(runtime.get_number("rawTakeDropRoutingOkay"), Some(1.0));
    assert_clean(&runtime, "raw take/drop routing");
}

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
