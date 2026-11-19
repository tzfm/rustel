use super::*;

#[test]
fn public_swing_preserves_scalar_steps_and_keeps_zero_no_step_and_patterned_boundaries() {
    let cases = [
        (
            "scalar swing",
            "pure(0).swing(4)",
            Some(Fraction::ONE),
            8usize,
        ),
        (
            "scalar swingBy",
            "pure(0).setSteps(8).swingBy(1/3, 4)",
            Some(Fraction::int(8)),
            8,
        ),
        (
            "fractional swing",
            "pure(0).setSteps(8).swing(.5)",
            Some(Fraction::int(8)),
            1,
        ),
        (
            "negative swing",
            "pure(0).setSteps(8).swing(-2)",
            Some(Fraction::int(8)),
            0,
        ),
        (
            "zero swing",
            "pure(0).setSteps(8).swing(0)",
            Some(Fraction::ONE),
            0,
        ),
        (
            "no-step swing",
            "pure(0).setSteps(undefined).swing(4)",
            None,
            8,
        ),
        (
            "patterned swing boundary",
            "pure(0).setSteps(8).swing(fastcat(2, 4))",
            None,
            6,
        ),
    ];

    for (label, source, expected_steps, expected_haps) in cases {
        let runtime = semantic_runtime(source);
        let pattern = runtime.active_pattern().expect("active public swing");
        assert_eq!(pattern.steps, expected_steps, "{label}: steps");
        assert!(pattern.is_pure(), "{label}: native graph lost purity");
        assert!(
            !runtime.active_needs_host(),
            "{label}: native graph retained JavaScript"
        );
        let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(haps.len(), expected_haps, "{label}: hap count");
        if label == "patterned swing boundary" {
            assert_eq!(
                haps.iter()
                    .map(|hap| (hap.part.begin, hap.part.end))
                    .collect::<Vec<_>>(),
                [
                    (Fraction::ZERO, Fraction::new(1, 4)),
                    (Fraction::new(1, 4), Fraction::new(1, 2)),
                    (Fraction::new(1, 2), Fraction::new(5, 8)),
                    (Fraction::new(5, 8), Fraction::new(3, 4)),
                    (Fraction::new(3, 4), Fraction::new(7, 8)),
                    (Fraction::new(7, 8), Fraction::ONE),
                ],
                "patterned public swing timing changed"
            );
        }
        assert_clean(&runtime, label);
        runtime.clear_active();
    }
}

#[test]
fn raw_swing_keeps_surface_strict_forwarding_dynamic_dispatch_parser_scalars_and_constructors() {
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
                   const hasOwn = Object.prototype.hasOwnProperty;
                   const raw = Pattern.prototype._swing;

                   phase = 'surface';
                   check(typeof raw === 'function'
                     && raw !== Pattern.prototype.swing
                     && raw !== Pattern.prototype.swingBy,
                     'raw identity');
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_swing'
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(Reflect.ownKeys(raw).join('|')
                     === 'length|name|prototype', 'raw own keys');
                   check(raw.name === '' && raw.length === 0
                     && hasOwn.call(raw, 'prototype'), 'raw reflection');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw constructibility');
                   check(!hasOwn.call(globalThis, '_swing')
                     && !hasOwn.call(rustelScope, '_swing')
                     && hasOwn.call(Pattern.prototype, '_swing'),
                     'raw publication');
                   check(!hasOwn.call(Pattern.prototype, '_swingBy')
                     && !hasOwn.call(globalThis, '_swingBy')
                     && !hasOwn.call(rustelScope, '_swingBy'),
                     'raw swingBy residual leaked');
                   const projected = [
                     'swingBy', '_swingBy', 'swing', '_swing'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === 'swingBy|swing|_swing',
                     'filtered own-key order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === 'swingBy|swing|_swing',
                     'filtered enumerable order');

                   phase = 'strict-forwarding';
                   const factor = new Proxy(Object.create(null), {
                     get() { throw new Error('raw swing inspected factor'); },
                     set() { throw new Error('raw swing wrote factor'); },
                     ownKeys() { throw new Error('raw swing listed factor'); },
                   });
                   let zeroHits = 0;
                   const zeroReceiver = {
                     swingBy() { zeroHits++; return null; },
                   };
                   let zeroError;
                   try { apply(raw, zeroReceiver, []); }
                   catch (error) { zeroError = error; }
                   check(zeroError instanceof TypeError && zeroHits === 0,
                     'zero-argument shift');
                   for (const receiver of [null, undefined]) {
                     let error;
                     try { apply(raw, receiver, [factor]); }
                     catch (caught) { error = caught; }
                     check(error instanceof TypeError,
                       'nullish strict receiver did not throw');
                   }
                   for (const [receiver, prototype] of [
                     [7, Number.prototype],
                     ['x', String.prototype],
                     [true, Boolean.prototype],
                     [1n, BigInt.prototype],
                     [Symbol('receiver'), Symbol.prototype],
                   ]) {
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'swingBy'
                     );
                     Object.defineProperty(prototype, 'swingBy', {
                       configurable: true,
                       value: function (swing, received) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         check(swing === 1 / 3 && received === factor,
                           'primitive forwarding changed');
                         return receiver;
                       },
                     });
                     try {
                       check(Object.is(
                         apply(raw, receiver, [factor]), receiver
                       ), 'primitive terminal changed');
                     } finally {
                       if (prior === undefined) {
                         delete prototype.swingBy;
                       } else {
                         Object.defineProperty(
                           prototype, 'swingBy', prior
                         );
                       }
                     }
                   }

                   phase = 'dynamic-dispatch';
                   const target = {};
                   const terminal = { marker: 'terminal' };
                   let getterHits = 0;
                   let callHits = 0;
                   const callable = new Proxy(function () {}, {
                     apply(_function, thisArg, args) {
                       callHits++;
                       check(thisArg === target,
                         'dynamic method receiver changed');
                       check(args.length === 2
                         && args[0] === 1 / 3
                         && args[1] === factor,
                         'dynamic arguments changed');
                       return terminal;
                     },
                   });
                   Object.defineProperty(target, 'swingBy', {
                     configurable: true,
                     get() { getterHits++; return callable; },
                   });
                   check(apply(raw, target, [factor]) === terminal,
                     'one-argument dispatch');
                   const outerReceivers = [
                     null, undefined, 7, 'outer', 1n, Symbol('outer')
                   ];
                   for (const receiver of outerReceivers) {
                     check(apply(raw, receiver, [factor, target])
                       === terminal, 'explicit target dispatch');
                   }
                   const ignored = new Proxy({}, {
                     get() { throw new Error('read ignored extra'); },
                   });
                   check(apply(raw, null, [factor, target, ignored, 9])
                     === terminal, 'ignored extras changed dispatch');
                   check(getterHits === outerReceivers.length + 2
                     && callHits === getterHits,
                     'dynamic getter/call count');

                   const getterSentinel = { marker: 'getter-stop' };
                   const cutoffTarget = {};
                   Object.defineProperty(cutoffTarget, 'swingBy', {
                     get() { throw getterSentinel; },
                   });
                   let getterCaught;
                   try { apply(raw, null, [factor, cutoffTarget]); }
                   catch (error) { getterCaught = error; }
                   check(getterCaught === getterSentinel,
                     'getter throw identity');
                   const callSentinel = { marker: 'call-stop' };
                   let callCaught;
                   try {
                     apply(raw, null, [factor, {
                       swingBy() { throw callSentinel; },
                     }]);
                   } catch (error) { callCaught = error; }
                   check(callCaught === callSentinel,
                     'call throw identity');

                   phase = 'parser-and-scalars';
                   const parserLog = [];
                   setStringParser(value => {
                     parserLog.push(value);
                     return pure(Number(value));
                   });
                   const parsed = apply(raw, pure(0), ['4']);
                   check(parserLog.join('|') === '4'
                     && Number(parsed._steps) === 1,
                     `configured parser phase: ${parserLog}`);
                   const state = {
                     span: { begin: 0, end: 1 }, controls: {},
                   };
                   check(parsed.query(state).length === 8
                     && parserLog.join('|') === '4',
                     'configured parser reread');
                   const parserGetterSentinel = {
                     marker: 'parser-getter-stop',
                   };
                   const parserGetterTarget = {};
                   Object.defineProperty(parserGetterTarget, 'swingBy', {
                     get() { throw parserGetterSentinel; },
                   });
                   let parserGetterCaught;
                   try {
                     apply(raw, null, ['5', parserGetterTarget]);
                   } catch (error) { parserGetterCaught = error; }
                   check(parserGetterCaught === parserGetterSentinel
                     && parserLog.join('|') === '4',
                     'dynamic getter did not cut off parser');
                   const customTerminal = {};
                   check(apply(raw, null, ['custom', {
                     swingBy(swing, received) {
                       check(this !== undefined
                         && swing === 1 / 3
                         && received === 'custom',
                         'custom string dispatch');
                       return customTerminal;
                     },
                   }]) === customTerminal
                     && parserLog.join('|') === '4',
                     'custom dispatch entered parser');
                   for (const [n, steps, count] of [
                     [4, 8, 8], [.5, 8, 1], [-2, 8, 0], [0, 1, 0],
                   ]) {
                     const result = apply(
                       raw, pure(0).setSteps(8), [n]
                     );
                     check(Number(result._steps) === steps
                       && result.query(state).length === count,
                       `scalar ${n} changed`);
                   }
                   check(parserLog.join('|') === '4',
                     'numeric scalar entered parser');
                   setStringParser(undefined);

                   phase = 'saved-and-constructors';
                   const originalSlot = Pattern.prototype._swing;
                   const originalSwing = Pattern.prototype.swing;
                   const originalGlobalSwing = globalThis.swing;
                   const originalScopeSwing = rustelScope.swing;
                   Pattern.prototype._swing = () => {
                     throw new Error('poisoned raw slot');
                   };
                   Pattern.prototype.swing = () => {
                     throw new Error('poisoned public swing');
                   };
                   globalThis.swing = () => {
                     throw new Error('poisoned global swing');
                   };
                   rustelScope.swing = () => {
                     throw new Error('poisoned scope swing');
                   };
                   const savedTerminal = {};
                   const savedTarget = {
                     swingBy(swing, received) {
                       check(this === savedTarget
                         && swing === 1 / 3
                         && received === factor,
                         'saved raw dispatch changed');
                       return savedTerminal;
                     },
                   };
                   check(apply(raw, null, [factor, savedTarget])
                     === savedTerminal, 'saved raw followed poison');
                   Pattern.prototype._swing = originalSlot;
                   Pattern.prototype.swing = originalSwing;
                   globalThis.swing = originalGlobalSwing;
                   rustelScope.swing = originalScopeSwing;

                   let oneArgumentConstruction;
                   try { new raw(4); }
                   catch (error) { oneArgumentConstruction = error; }
                   check(oneArgumentConstruction instanceof TypeError,
                     'one-argument construction did not dispatch to instance');
                   const objectTerminal = {};
                   const constructorTarget = {
                     mode: 'object',
                     swingBy(swing, received) {
                       check(this === constructorTarget
                         && swing === 1 / 3 && received === 4,
                         'constructor forwarding changed');
                       return this.mode === 'object' ? objectTerminal : 7;
                     },
                   };
                   check(new raw(4, constructorTarget) === objectTerminal,
                     'object constructor terminal');
                   constructorTarget.mode = 'primitive';
                   const primitiveConstruction = new raw(
                     4, constructorTarget
                   );
                   check(primitiveConstruction instanceof raw,
                     'primitive constructor fallback');
                   function Redirect() {}
                   const redirected = Reflect.construct(
                     raw, [4, constructorTarget], Redirect
                   );
                   check(redirected instanceof Redirect
                     && !(redirected instanceof raw),
                     'redirected primitive constructor fallback');
                   constructorTarget.mode = 'object';
                   check(Reflect.construct(
                     raw, [4, constructorTarget], Redirect
                   ) === objectTerminal,
                   'redirected object constructor terminal');

                   globalThis.rawSwingSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawSwingSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSwingSurfaceError"), None);
    assert_eq!(runtime.get_number("rawSwingSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw swing surface and dispatch");
}

#[test]
fn raw_swing_preserves_selected_ownership_and_existing_resource_accounting() {
    let native = semantic_runtime(
        r#"(() => {
             const source = pure('native-swing').setSteps(8);
             source.__pure_loc = { start: 10, end: 11 };
             const result = source._swing(4);
             globalThis.rawSwingTagLoss = Number(
               Object.prototype.hasOwnProperty.call(source, '__pure')
               && Object.prototype.hasOwnProperty.call(
                 source, '__pure_loc'
               )
               && !Object.prototype.hasOwnProperty.call(result, '__pure')
               && !Object.prototype.hasOwnProperty.call(
                 result, '__pure_loc'
               )
             );
             return result;
           })()"#,
    );
    assert_eq!(native.get_number("rawSwingTagLoss"), Some(1.0));
    let native_pattern = native.active_pattern().expect("active raw swing");
    assert_eq!(native_pattern.steps, Some(Fraction::int(8)));
    assert!(native_pattern.is_pure(), "raw swing lost native purity");
    assert!(
        native_pattern.as_pure().is_none(),
        "raw swing retained the structural pure fast path"
    );
    assert!(
        !native.active_needs_host(),
        "raw native swing retained JavaScript"
    );
    assert_eq!(
        query_active(&native, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .len(),
        8
    );
    assert_clean(&native, "raw native swing ownership");
    native.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawSwingDispatchHits = 0;
             globalThis.rawSwingQueryHits = 0;
             globalThis.rawSwingLateHits = 0;
             globalThis.rawSwingOwnershipMismatch = 0;
             let kept = { marker: 'raw-swing-owned' };
             let factor = { marker: 'raw-swing-factor' };
             let target = {};
             let method = function (swing, received) {
               globalThis.rawSwingDispatchHits++;
               if (this !== target || swing !== 1 / 3
                   || received !== factor) {
                 globalThis.rawSwingOwnershipMismatch++;
               }
               const retained = kept;
               return new Pattern(state => {
                 globalThis.rawSwingQueryHits++;
                 return pure(retained).query(state);
               }, 1);
             };
             target.swingBy = method;
             globalThis.rawSwingKeptRef = new WeakRef(kept);
             globalThis.rawSwingDroppedRefs = [
               new WeakRef(factor), new WeakRef(target),
               new WeakRef(method)
             ];
             const result = Reflect.apply(
               Pattern.prototype._swing, null, [factor, target]
             );
             target.swingBy = function () {
               globalThis.rawSwingLateHits++;
               return pure('late');
             };
             kept = factor = target = method = null;
             return result;
           })()"#,
    );
    assert_eq!(owned.get_number("rawSwingDispatchHits"), Some(1.0));
    assert_eq!(owned.get_number("rawSwingQueryHits"), Some(0.0));
    assert_eq!(owned.get_number("rawSwingOwnershipMismatch"), Some(0.0));
    assert!(
        owned.active_needs_host(),
        "returned custom Pattern lost its query owner"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawSwingKeptAlive = Number(
                 rawSwingKeptRef.deref()?.marker === 'raw-swing-owned'
               );
               globalThis.rawSwingTransientsPruned = Number(
                 rawSwingDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawSwingKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSwingTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(values.iter().any(|value| value.contains("raw-swing-owned")));
    assert_eq!(owned.get_number("rawSwingQueryHits"), Some(1.0));
    assert_eq!(owned.get_number("rawSwingLateHits"), Some(0.0));
    assert_clean(&owned, "raw swing selected ownership");
    owned.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(0).setSteps(8)._swing(4)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw swing charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw swing charged query-time stepwise work"
    );
    assert_clean(&direct, "raw swing pool-neutral direct path");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._swing(4);
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw swing consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw swing shared-pool neutrality");
    shared.clear_active();

    let over = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._swing(4);
             return gap({}).shrink(0);
           }})"#,
        limit + 1
    ));
    rustel_core::reset_stepwise_entries_materialised();
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
    assert_clean(&over, "raw swing nested +1 refusal");
    over.clear_active();

    // Canonical own-query replacement, mutable `inside`, patterned/extreme
    // fractions, and general graph/allocation bounds remain explicit
    // residuals; this raw dispatch adds no resource operation of its own.
}
