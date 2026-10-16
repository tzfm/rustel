use super::*;

#[test]
fn raw_apply_keeps_surface_strict_forwarding_and_terminal_semantics() {
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
                   const raw = Pattern.prototype._apply;

                   phase = 'surface';
                   check(typeof raw === 'function'
                     && raw !== Pattern.prototype.apply,
                     'raw identity');
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_apply'
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
                   check(!('_apply' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_apply'
                     ), 'raw publication');
                   const ownNames = Reflect.ownKeys(Pattern.prototype);
                   check(ownNames.indexOf('_apply')
                     === ownNames.indexOf('apply') + 1,
                     'own-key interleave');
                   const enumerableNames = Object.keys(Pattern.prototype);
                   check(enumerableNames.indexOf('_apply')
                     === enumerableNames.indexOf('apply') + 1,
                     'enumerable interleave');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined, 7, 'x']) {
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
                     check(Object.is(apply(raw, receiver, [callback]), receiver)
                       && trapHits === 1,
                       'strict receiver forwarding changed');
                   }

                   phase = 'argument-forwarding';
                   let forwardHits = 0;
                   const zeroTerminal = {};
                   function zeroCallback(received) {
                     'use strict';
                     forwardHits++;
                     check(this === undefined && received === undefined,
                       'zero-argument forwarding');
                     return zeroTerminal;
                   }
                   check(apply(raw, zeroCallback, []) === zeroTerminal,
                     'zero-argument terminal');

                   const wrapperTarget = {};
                   const oneTerminal = {};
                   function oneCallback(received) {
                     'use strict';
                     forwardHits++;
                     check(this === undefined && received === wrapperTarget,
                       'one-argument forwarding');
                     return oneTerminal;
                   }
                   check(apply(raw, wrapperTarget, [oneCallback])
                     === oneTerminal, 'one-argument terminal');

                   const explicitTarget = {};
                   const wrapperDecoy = {};
                   const explicitTerminal = {};
                   function explicitCallback(received) {
                     'use strict';
                     forwardHits++;
                     check(this === undefined && received === explicitTarget,
                       'explicit target forwarding');
                     return explicitTerminal;
                   }
                   check(apply(raw, wrapperDecoy, [
                     explicitCallback, explicitTarget
                   ]) === explicitTerminal, 'two-argument terminal');
                   check(apply(raw, wrapperDecoy, [
                     explicitCallback, explicitTarget, 'ignored', {}
                   ]) === explicitTerminal, 'extra-argument terminal');
                   check(forwardHits === 4,
                     'callback was skipped or invoked repeatedly');

                   phase = 'throw-identity';
                   const sentinel = { marker: 'raw-apply-throw' };
                   let caught;
                   try {
                     apply(raw, wrapperDecoy, [() => { throw sentinel; }]);
                   } catch (error) {
                     caught = error;
                   }
                   check(caught === sentinel, 'callback throw was translated');

                   phase = 'arbitrary-terminals';
                   const symbol = Symbol('raw-apply-terminal');
                   const object = { marker: 'object-terminal' };
                   const callable = function terminal() {};
                   const promise = Promise.resolve('promise-terminal');
                   const terminals = [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ];
                   for (const terminal of terminals) {
                     const result = apply(raw, wrapperDecoy, [
                       () => terminal, explicitTarget
                     ]);
                     check(Object.is(result, terminal),
                       'arbitrary terminal was transformed');
                   }

                   phase = 'constructors';
                   const constructedObjectTerminal = {};
                   let objectPat;
                   const objectConstruction = Reflect.construct(raw, [
                     function (pat) {
                       'use strict';
                       check(this === undefined, 'constructor callback this');
                       objectPat = pat;
                       return constructedObjectTerminal;
                     },
                   ]);
                   check(objectConstruction === constructedObjectTerminal
                     && objectPat instanceof raw,
                     'object constructor return did not replace instance');
                   let primitivePat;
                   const primitiveConstruction = Reflect.construct(raw, [
                     function (pat) {
                       'use strict';
                       primitivePat = pat;
                       return 7;
                     },
                   ]);
                   check(primitiveConstruction === primitivePat
                     && primitiveConstruction instanceof raw
                     && Reflect.ownKeys(primitiveConstruction).length === 0,
                     'primitive constructor return did not fall back');
                   check(Object.is(apply(raw, null, [() => -0]), -0),
                     'ordinary call changed primitive return');

                   phase = 'poisoning';
                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned apply surface reached');
                   };
                   const poisonedTarget = {};
                   Object.defineProperty(poisonedTarget, 'apply', {
                     configurable: true,
                     get() {
                       poisonHits++;
                       throw new Error('target apply getter reached');
                     },
                   });
                   Pattern.prototype.apply = poison;
                   Pattern.prototype._apply = poison;
                   globalThis.apply = poison;
                   rustelScope.apply = poison;
                   const poisonTerminal = {};
                   check(apply(raw, null, [
                     received => {
                       check(received === poisonedTarget,
                         'poison target identity');
                       return poisonTerminal;
                     },
                     poisonedTarget,
                   ]) === poisonTerminal && poisonHits === 0,
                   'saved raw routed through poisoned surface');

                   globalThis.rawApplySurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawApplySurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawApplySurfaceError"), None);
    assert_eq!(runtime.get_number("rawApplySurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw apply surface and forwarding");
}

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
            .steps,
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
