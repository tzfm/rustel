use super::*;

#[test]
fn raw_when_keeps_surface_strict_truthiness_and_terminal_semantics() {
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
                   const raw = Pattern.prototype._when;

                   phase = 'surface';
                   check(typeof raw === 'function'
                     && raw !== Pattern.prototype.when,
                     'raw identity');
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_when'
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'prototype descriptor');
                   check(Reflect.ownKeys(raw).join('|')
                     === 'length|name|prototype', 'function own keys');
                   check(raw.name === '' && raw.length === 0
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'function reflection');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'constructibility');
                   check(!('_when' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_when'
                     ), 'raw publication');
                   const ownNames = Reflect.ownKeys(Pattern.prototype);
                   check(ownNames.indexOf('_when')
                     === ownNames.indexOf('when') + 1,
                     'own-key interleave');
                   const enumerableNames = Object.keys(Pattern.prototype);
                   check(enumerableNames.indexOf('_when')
                     === enumerableNames.indexOf('when') + 1,
                     'enumerable interleave');

                   phase = 'strict-receivers';
                   for (const receiver of [
                     null, undefined, 7, 'x', false, 9n, Symbol('receiver')
                   ]) {
                     let trapHits = 0;
                     const callback = new Proxy(function () {
                       throw new Error('proxy target body reached');
                     }, {
                       apply(_target, thisArg, args) {
                         trapHits++;
                         check(thisArg === undefined,
                           'callback thisArg was not undefined');
                         check(args.length === 1
                           && Object.is(args[0], receiver),
                           'wrapper receiver was boxed or replaced');
                         return args[0];
                       },
                     });
                     check(Object.is(
                       apply(raw, receiver, [true, callback]), receiver
                     ) && trapHits === 1,
                     'strict receiver forwarding changed');
                   }

                   phase = 'argument-forwarding';
                   check(apply(raw, null, []) === undefined,
                     'zero-argument false receiver');
                   let zeroTruthyError;
                   try { apply(raw, {}, []); }
                   catch (error) { zeroTruthyError = error.name; }
                   check(zeroTruthyError === 'TypeError',
                     'zero-argument truthy receiver did not call undefined');

                   let oneFalseHits = 0;
                   function oneFalseReceiver() { oneFalseHits++; }
                   check(apply(raw, oneFalseReceiver, [false]) === undefined
                     && oneFalseHits === 0,
                     'one-argument false branch touched receiver callback');
                   let oneTrueHits = 0;
                   function oneTrueReceiver(received) {
                     'use strict';
                     oneTrueHits++;
                     check(this === undefined && received === undefined,
                       'one-argument true forwarding');
                     return received;
                   }
                   check(apply(raw, oneTrueReceiver, [true]) === undefined
                     && oneTrueHits === 1,
                     'one-argument true branch count');

                   const receiver = { marker: 'receiver' };
                   const target = { marker: 'target' };
                   const terminal = { marker: 'terminal' };
                   let normalHits = 0;
                   function normalCallback(received) {
                     'use strict';
                     normalHits++;
                     check(this === undefined, 'normal callback this');
                     check(received === (normalHits === 1 ? receiver : target),
                       'normal callback target');
                     return terminal;
                   }
                   check(apply(raw, receiver, [true, normalCallback])
                     === terminal, 'two-argument terminal');
                   check(apply(raw, receiver, [
                     true, normalCallback, target
                   ]) === terminal, 'three-argument retarget');
                   check(apply(raw, receiver, [
                     true, normalCallback, target, Symbol('ignored'),
                     { get poison() { throw new Error('ignored extra'); } }
                   ]) === terminal, 'extra-argument terminal');
                   check(normalHits === 3,
                     'two/three/extra callback count');

                   phase = 'truthiness';
                   let falseHits = 0;
                   const falseCallback = new Proxy(function () {}, {
                     apply() {
                       falseHits++;
                       throw new Error('false callback reached');
                     },
                   });
                   for (const on of [
                     undefined, null, false, 0, -0, 0n, NaN, ''
                   ]) {
                     check(apply(raw, receiver, [on, falseCallback])
                       === receiver, 'falsey value did not return receiver');
                   }
                   const revocable = Proxy.revocable(function () {}, {});
                   revocable.revoke();
                   check(apply(raw, receiver, [false, revocable.proxy])
                     === receiver && falseHits === 0,
                     'false branch validated or called func');
                   let nonCallableReads = 0;
                   const hostileNonCallable = new Proxy({}, {
                     get() {
                       nonCallableReads++;
                       throw new Error('false non-callable was inspected');
                     },
                   });
                   for (const func of [
                     undefined, null, 7, Symbol('not-callable'),
                     hostileNonCallable
                   ]) {
                     check(apply(raw, receiver, [false, func, target])
                       === target,
                       'false branch validated a non-callable func');
                   }
                   check(nonCallableReads === 0,
                     'false branch inspected non-callable func');

                   let coercionReads = 0;
                   const objectCondition = new Proxy({}, {
                     get() {
                       coercionReads++;
                       throw new Error('condition coercion reached');
                     },
                   });
                   let truthyHits = 0;
                   const truthyValues = [
                     true, 1, -1, 1n, '0', Symbol('on'), {}, [],
                     new Boolean(false), objectCondition
                   ];
                   for (const on of truthyValues) {
                     const result = apply(raw, receiver, [on, value => {
                       truthyHits++;
                       check(value === receiver,
                         'truthy callback target changed');
                       return value;
                     }]);
                     check(result === receiver,
                       'truthy value did not invoke callback');
                   }
                   check(truthyHits === truthyValues.length
                     && coercionReads === 0,
                     'ToBoolean coerced an object or changed count');

                   phase = 'guards-and-parser';
                   let targetReads = 0;
                   const guardedTarget = new Proxy({}, {
                     get() {
                       targetReads++;
                       throw new Error('raw when read target');
                     },
                   });
                   check(apply(raw, receiver, [
                     false, falseCallback, guardedTarget
                   ]) === guardedTarget && targetReads === 0,
                   'false branch inspected target');
                   const guardedTerminal = {};
                   Object.defineProperties(guardedTerminal, {
                     query: {
                       get() { throw new Error('raw when read result query'); },
                     },
                     _steps: {
                       get() { throw new Error('raw when read result steps'); },
                       set() { throw new Error('raw when wrote result steps'); },
                     },
                   });
                   check(apply(raw, receiver, [
                     true, () => guardedTerminal, guardedTarget
                   ]) === guardedTerminal && targetReads === 0,
                   'true branch inspected target or terminal');
                   const parserCalls = [];
                   setStringParser(value => {
                     parserCalls.push(value);
                     return pure(value);
                   });
                   check(apply(raw, receiver, [
                     false, falseCallback, 'false text'
                   ]) === 'false text', 'false string target');
                   check(apply(raw, receiver, [
                     true, value => value, 'true text'
                   ]) === 'true text', 'true string target');
                   setStringParser(undefined);
                   check(parserCalls.length === 0,
                     'raw when invoked configured parser');

                   phase = 'terminals-and-throws';
                   const symbol = Symbol('raw-when-terminal');
                   const object = { marker: 'object-terminal' };
                   const callable = function returnedFunction() {};
                   const promise = Promise.resolve('promise-terminal');
                   const terminals = [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ];
                   for (const value of terminals) {
                     check(Object.is(apply(raw, receiver, [
                       true, () => value, target
                     ]), value), 'arbitrary terminal was transformed');
                   }
                   const sentinel = { marker: 'raw-when-throw' };
                   let caught;
                   try {
                     apply(raw, receiver, [
                       true, () => { throw sentinel; }, target
                     ]);
                   } catch (error) {
                     caught = error;
                   }
                   check(caught === sentinel,
                     'callback throw was translated');
                   let nonCallableError;
                   try { apply(raw, receiver, [true, 7, target]); }
                   catch (error) { nonCallableError = error.name; }
                   check(nonCallableError === 'TypeError',
                     'truthy non-callable did not throw TypeError');

                   phase = 'constructors';
                   const objectTerminal = { marker: 'constructed-object' };
                   let objectPat;
                   const objectConstruction = Reflect.construct(raw, [
                     true,
                     function (pat) {
                       'use strict';
                       check(this === undefined,
                         'constructor callback this');
                       objectPat = pat;
                       return objectTerminal;
                     },
                   ]);
                   check(objectConstruction === objectTerminal
                     && objectPat instanceof raw,
                     'object constructor return did not replace instance');
                   let primitivePat;
                   const primitiveConstruction = Reflect.construct(raw, [
                     true,
                     function (pat) {
                       'use strict';
                       primitivePat = pat;
                       return 7;
                     },
                   ]);
                   check(primitiveConstruction === primitivePat
                     && primitiveConstruction instanceof raw,
                     'primitive constructor return did not fall back');
                   let falseConstructorHits = 0;
                   const falseTarget = { marker: 'false-constructor-target' };
                   check(Reflect.construct(raw, [
                     false, () => { falseConstructorHits++; }, falseTarget
                   ]) === falseTarget,
                   'false object target did not replace instance');
                   const falsePrimitive = Reflect.construct(raw, [
                     false, () => { falseConstructorHits++; }, 7
                   ]);
                   check(falsePrimitive instanceof raw
                     && falseConstructorHits === 0,
                     'false primitive constructor fallback');

                   phase = 'poisoning';
                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned when surface reached');
                   };
                   const poisonedTarget = {};
                   Object.defineProperties(poisonedTarget, {
                     when: {
                       configurable: true,
                       get() {
                         poisonHits++;
                         throw new Error('target when getter reached');
                       },
                     },
                     _when: {
                       configurable: true,
                       get() {
                         poisonHits++;
                         throw new Error('target raw when getter reached');
                       },
                     },
                   });
                   const saved = {
                     method: Pattern.prototype.when,
                     raw: Pattern.prototype._when,
                     global: globalThis.when,
                     scope: rustelScope.when,
                   };
                   const poisonTerminal = {};
                   try {
                     Pattern.prototype.when = poison;
                     Pattern.prototype._when = poison;
                     globalThis.when = poison;
                     rustelScope.when = poison;
                     check(apply(raw, null, [
                       true,
                       value => {
                         check(value === poisonedTarget,
                           'poison target identity');
                         return poisonTerminal;
                       },
                       poisonedTarget,
                     ]) === poisonTerminal && poisonHits === 0,
                     'saved raw routed through poisoned surface');
                   } finally {
                     Pattern.prototype.when = saved.method;
                     Pattern.prototype._when = saved.raw;
                     globalThis.when = saved.global;
                     rustelScope.when = saved.scope;
                   }

                   globalThis.rawWhenSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawWhenSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawWhenSurfaceError"), None);
    assert_eq!(runtime.get_number("rawWhenSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw when surface and forwarding");
}

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
            .steps,
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
            .steps,
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
