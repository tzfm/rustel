use super::*;

#[test]
fn raw_signal_quartet_keeps_surface_strict_forwarding_dynamic_dispatch_and_constructors() {
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
                   const entries = [
                     ['often', '_often', 0.75],
                     ['rarely', '_rarely', 0.25],
                     ['almostNever', '_almostNever', 0.1],
                     ['almostAlways', '_almostAlways', 0.9],
                   ];
                   const raws = entries.map(([, rawName]) =>
                     Pattern.prototype[rawName]
                   );

                   phase = 'surface';
                   check(new Set(raws).size === entries.length,
                     'raw identities collapsed');
                   for (const [name, rawName] of entries) {
                     const raw = Pattern.prototype[rawName];
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, rawName
                     );
                     check(typeof raw === 'function'
                       && raw !== Pattern.prototype[name],
                       `${rawName} identity`);
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${rawName} descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${rawName} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && hasOwn.call(raw, 'prototype'),
                       `${rawName} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${rawName} constructibility`);
                     check(hasOwn.call(Pattern.prototype, rawName)
                       && !hasOwn.call(globalThis, rawName)
                       && !hasOwn.call(rustelScope, rawName),
                       `${rawName} publication`);
                   }
                   const projected = [
                     'often', '_often', 'rarely', '_rarely',
                     'almostNever', '_almostNever',
                     'almostAlways', '_almostAlways',
                     'never', '_never', 'always', '_always',
                   ];
                   const expectedOrder = projected.join('|');
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === expectedOrder, 'filtered own-key order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => projected.includes(name)).join('|')
                     === expectedOrder, 'filtered enumerable order');

                   phase = 'strict-forwarding';
                   const callback = function callback() {};
                   let zeroReceiverHits = 0;
                   const zeroReceiver = new Proxy(callback, {
                     get() { zeroReceiverHits++; throw new Error('read receiver'); },
                   });
                   let zeroError;
                   try { apply(raws[0], zeroReceiver, []); }
                   catch (error) { zeroError = error; }
                   check(zeroError instanceof TypeError
                     && zeroReceiverHits === 0, 'zero-argument shift');
                   for (const receiver of [null, undefined]) {
                     let error;
                     try { apply(raws[0], receiver, [callback]); }
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
                       prototype, 'sometimesBy'
                     );
                     Object.defineProperty(prototype, 'sometimesBy', {
                       configurable: true,
                       value: function (threshold, received) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         check(threshold === 0.75 && received === callback,
                           'primitive arguments changed');
                         return receiver;
                       },
                     });
                     try {
                       check(Object.is(
                         apply(raws[0], receiver, [callback]), receiver
                       ), 'primitive terminal changed');
                     } finally {
                       if (prior === undefined) {
                         delete prototype.sometimesBy;
                       } else {
                         Object.defineProperty(
                           prototype, 'sometimesBy', prior
                         );
                       }
                     }
                   }

                   phase = 'dynamic-dispatch';
                   for (const [index, [name, rawName, threshold]]
                        of entries.entries()) {
                     const raw = raws[index];
                     const terminal = { rawName };
                     const target = {};
                     let getterHits = 0;
                     let callHits = 0;
                     const method = new Proxy(function () {}, {
                       apply(_function, thisArg, args) {
                         callHits++;
                         check(thisArg === target,
                           `${rawName} method receiver`);
                         check(args.length === 2
                           && Object.is(args[0], threshold)
                           && args[1] === callback,
                           `${rawName} arguments`);
                         return terminal;
                       },
                     });
                     Object.defineProperty(target, 'sometimesBy', {
                       configurable: true,
                       get() { getterHits++; return method; },
                     });
                     check(apply(raw, target, [callback]) === terminal,
                       `${rawName} one-argument target`);
                     const outer = new Proxy({}, {
                       get() { throw new Error('read outer receiver'); },
                       ownKeys() { throw new Error('listed outer receiver'); },
                     });
                     const ignored = new Proxy({}, {
                       get() { throw new Error('read ignored extra'); },
                     });
                     check(apply(raw, outer, [callback, target, ignored, 9])
                       === terminal, `${rawName} explicit target`);
                     check(getterHits === 2 && callHits === 2,
                       `${rawName} getter/call count`);

                     const getterSentinel = { rawName, phase: 'getter' };
                     const cutoffTarget = {};
                     Object.defineProperty(cutoffTarget, 'sometimesBy', {
                       get() { throw getterSentinel; },
                     });
                     let getterCaught;
                     try { apply(raw, null, [callback, cutoffTarget]); }
                     catch (error) { getterCaught = error; }
                     check(getterCaught === getterSentinel,
                       `${rawName} getter throw identity`);
                     const callSentinel = { rawName, phase: 'call' };
                     let callCaught;
                     try {
                       apply(raw, null, [callback, {
                         sometimesBy() { throw callSentinel; },
                       }]);
                     } catch (error) { callCaught = error; }
                     check(callCaught === callSentinel,
                       `${rawName} call throw identity`);

                     const originalRaw = Pattern.prototype[rawName];
                     const originalPublic = Pattern.prototype[name];
                     const originalGlobal = globalThis[name];
                     const scopeOwned = hasOwn.call(rustelScope, name);
                     const originalScope = rustelScope[name];
                     Pattern.prototype[rawName] = () => {
                       throw new Error('poisoned raw');
                     };
                     Pattern.prototype[name] = () => {
                       throw new Error('poisoned public');
                     };
                     globalThis[name] = () => {
                       throw new Error('poisoned global');
                     };
                     rustelScope[name] = () => {
                       throw new Error('poisoned scope');
                     };
                     check(apply(raw, null, [callback, target]) === terminal,
                       `${rawName} saved dispatch followed poison`);
                     Pattern.prototype[rawName] = originalRaw;
                     Pattern.prototype[name] = originalPublic;
                     globalThis[name] = originalGlobal;
                     if (scopeOwned) rustelScope[name] = originalScope;
                     else delete rustelScope[name];

                     let oneArgumentConstruction;
                     try { new raw(callback); }
                     catch (error) { oneArgumentConstruction = error; }
                     check(oneArgumentConstruction instanceof TypeError,
                       `${rawName} one-argument construction`);
                     const objectTerminal = {};
                     const constructorTarget = {
                       object: true,
                       sometimesBy(receivedThreshold, receivedCallback) {
                         check(this === constructorTarget
                           && Object.is(receivedThreshold, threshold)
                           && receivedCallback === callback,
                           `${rawName} constructor forwarding`);
                         return this.object ? objectTerminal : 7;
                       },
                     };
                     check(new raw(callback, constructorTarget)
                       === objectTerminal,
                       `${rawName} object constructor terminal`);
                     constructorTarget.object = false;
                     const primitiveConstruction = new raw(
                       callback, constructorTarget
                     );
                     check(primitiveConstruction instanceof raw,
                       `${rawName} primitive constructor fallback`);
                   }

                   phase = 'parser-cutoffs';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`unexpected parser call: ${value}`);
                   });
                   check(pure('ordinary')._often(callback) instanceof Pattern
                     && parserCalls === 0,
                     'ordinary callable entered parser');
                   const customStringTarget = {
                     sometimesBy(threshold, received) {
                       check(threshold === 0.75 && received === 'raw-string',
                         'custom string identity changed');
                       return 3;
                     },
                   };
                   check(apply(raws[0], null, [
                     'raw-string', customStringTarget
                   ]) === 3 && parserCalls === 0,
                     'custom dispatch entered parser');
                   const parserCutoff = {};
                   Object.defineProperty(parserCutoff, 'sometimesBy', {
                     get() { throw new Error('parser cutoff'); },
                   });
                   try {
                     apply(raws[0], null, ['raw-string', parserCutoff]);
                   } catch (error) {
                     check(error.message === 'parser cutoff',
                       'parser cutoff error changed');
                   }
                   check(parserCalls === 0,
                     'getter cutoff entered parser');
                   setStringParser(undefined);

                   globalThis.rawSignalSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawSignalSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSignalSurfaceError"), None);
    assert_eq!(runtime.get_number("rawSignalSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw signal surface and dispatch");
}

#[test]
fn raw_signal_quartet_keeps_rng_lazy_callbacks_steps_tags_and_throw_cutoff() {
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
                   const entries = [
                     ['_often', 55],
                     ['_rarely', 18],
                     ['_almostNever', 7],
                     ['_almostAlways', 63],
                   ];
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   for (const [rawName, transformed] of entries) {
                     phase = `${rawName}-rng`;
                     let calls = 0;
                     const result = pure(0).fast(64)[rawName](pat => {
                       calls++;
                       return pat.add(1);
                     });
                     check(calls === 0, `${rawName} callback was eager`);
                     const first = result.query(state(0, 1));
                     check(calls === 1 && first.length === 64
                       && first.filter(hap => hap.value === 1).length
                         === transformed,
                       `${rawName} first-cycle discriminator`);
                     const second = result.query(state(1, 2));
                     check(calls === 2 && second.length === 64,
                       `${rawName} crossing callback count`);

                     const source = pure(rawName).setSteps(11);
                     source.__pure_loc = { start: 10, end: 11 };
                     check(Object.prototype.hasOwnProperty.call(
                       source, '__pure'
                     ) && Object.prototype.hasOwnProperty.call(
                       source, '__pure_loc'
                     ), `${rawName} source tag precondition`);
                     const identity = source[rawName](pat => pat);
                     check(identity !== source
                       && identity.query !== source.query
                       && identity._steps === undefined
                       && !Object.prototype.hasOwnProperty.call(
                         identity, '__pure'
                       )
                       && !Object.prototype.hasOwnProperty.call(
                         identity, '__pure_loc'
                       ), `${rawName} steps/tag loss`);
                   }
                   globalThis.rawSignalSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawSignalSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSignalSemanticError"), None);
    assert_eq!(runtime.get_number("rawSignalSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw signal scalar semantics");

    let throwing = semantic_runtime(
        r#"(() => {
             globalThis.rawSignalThrowCalls = 0;
             return pure('throw')._often(() => {
               globalThis.rawSignalThrowCalls++;
               throw new Error('raw signal callback sentinel');
             });
           })()"#,
    );
    let thrown = query_active(&throwing, Fraction::ZERO, Fraction::int(2)).unwrap();
    assert!(
        thrown.is_empty(),
        "query-time raw signal throw was not converted to silence"
    );
    assert_eq!(throwing.get_number("rawSignalThrowCalls"), Some(1.0));
    assert_clean(&throwing, "raw signal first-throw cutoff");
    throwing.clear_active();

    // Explicitly pin, rather than hide, the remaining public-helper gap:
    // pinned Node constructs lazily for `_often(17)` and queryArc yields zero
    // after `func is not a function`; native `sometimesBy` currently maps a
    // non-callable FunctionRef to the identity and therefore emits the source.
    let invalid = semantic_runtime("pure('invalid').fast(4)._often(17)");
    let invalid_haps = query_active(&invalid, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(invalid_haps.len(), 4, "invalid-callback residual drifted");
    assert!(
        invalid_haps.iter().all(|hap| hap.value.show() == "invalid"),
        "invalid-callback residual stopped selecting the source"
    );
    assert_clean(&invalid, "raw signal invalid-callback residual");
    invalid.clear_active();
}

#[test]
fn raw_signal_quartet_preserves_owners_prunes_transients_and_keeps_nested_accounting() {
    let tagged = semantic_runtime("pure('tagged')._often(rev)");
    let tagged_pattern = tagged.active_pattern().expect("tagged raw signal");
    assert!(
        tagged_pattern.is_pure(),
        "tagged raw signal lost native purity"
    );
    assert!(
        !tagged.active_needs_host(),
        "tagged raw signal retained a JavaScript query owner"
    );
    let detached = tagged_pattern.clone();
    tagged.clear_active();
    drop(tagged);
    let detached_values = detached
        .query_arc_sorted(Fraction::ZERO, Fraction::ONE)
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(detached_values, ["tagged"]);

    let owned = semantic_runtime(
        r#"(() => {
             globalThis.rawSignalCallbackCalls = 0;
             let callbackMarker = { marker: 'raw-signal-callback' };
             let valueMarker = { marker: 'raw-signal-value' };
             let outer = { marker: 'raw-signal-outer' };
             let extra = { marker: 'raw-signal-extra' };
             let callback = ((ownedCallbackMarker, ownedValueMarker) =>
               pat => {
                 globalThis.rawSignalCallbackCalls++;
                 void ownedCallbackMarker;
                 return pat.withValue(() => ownedValueMarker);
               }
             )(callbackMarker, valueMarker);
             globalThis.rawSignalCallbackMarkerRef = new WeakRef(
               callbackMarker
             );
             globalThis.rawSignalValueMarkerRef = new WeakRef(valueMarker);
             globalThis.rawSignalCallbackRef = new WeakRef(callback);
             globalThis.rawSignalTransientRefs = [
               new WeakRef(outer), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._often, outer,
               [callback, pure('source').fast(8), extra]
             );
             callbackMarker = valueMarker = outer = extra = callback = null;
             return result;
           })()"#,
    );
    assert!(owned.active_needs_host(), "JS callback route lost its host");
    assert_eq!(owned.get_number("rawSignalCallbackCalls"), Some(0.0));
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawSignalCallbackOwnerAlive = Number(
                 rawSignalCallbackMarkerRef.deref()?.marker
                   === 'raw-signal-callback'
                 && typeof rawSignalCallbackRef.deref() === 'function'
               );
               globalThis.rawSignalValueOwnerAlive = Number(
                 rawSignalValueMarkerRef.deref()?.marker
                   === 'raw-signal-value'
               );
               globalThis.rawSignalTransientsPruned = Number(
                 rawSignalTransientRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawSignalCallbackOwnerAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSignalValueOwnerAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSignalTransientsPruned"), Some(1.0));
    let first_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(owned.get_number("rawSignalCallbackCalls"), Some(1.0));
    assert!(
        first_values
            .iter()
            .any(|value| value.contains("raw-signal-value"))
    );
    query_active(&owned, Fraction::ONE, Fraction::int(2)).unwrap();
    assert_eq!(owned.get_number("rawSignalCallbackCalls"), Some(2.0));
    assert_clean(&owned, "raw signal callback ownership");
    owned.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-signal-terminal' };
             let outer = { marker: 'raw-signal-terminal-outer' };
             let target = {};
             let callback = function callback() {};
             let extra = { marker: 'raw-signal-terminal-extra' };
             let method = function (threshold, received) {
               if (this !== target || threshold !== 0.25
                   || received !== callback) {
                 throw new Error('terminal dispatch changed');
               }
               return pure(kept);
             };
             target.sometimesBy = method;
             globalThis.rawSignalTerminalKeptRef = new WeakRef(kept);
             globalThis.rawSignalTerminalDroppedRefs = [
               new WeakRef(outer), new WeakRef(target),
               new WeakRef(callback), new WeakRef(extra),
               new WeakRef(method),
             ];
             const result = Reflect.apply(
               Pattern.prototype._rarely, outer,
               [callback, target, extra]
             );
             kept = outer = target = callback = extra = method = null;
             return result;
           })()"#,
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawSignalTerminalKeptAlive = Number(
                 rawSignalTerminalKeptRef.deref()?.marker
                   === 'raw-signal-terminal'
               );
               globalThis.rawSignalTerminalTransientsPruned = Number(
                 rawSignalTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(terminal.get_number("rawSignalTerminalKeptAlive"), Some(1.0));
    assert_eq!(
        terminal.get_number("rawSignalTerminalTransientsPruned"),
        Some(1.0)
    );
    assert!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show()
            .contains("raw-signal-terminal")
    );
    assert_clean(&terminal, "raw signal custom-terminal ownership");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure('neutral')._almostAlways(rev)");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw signal quartet charged stepwise work"
    );
    assert_clean(&direct, "raw signal direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure('nested')._often(() => gap({limit}).shrink(0))"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested shrink accounting was not preserved"
    );
    assert_clean(&exact, "raw signal nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure('nested')._often(() => gap({}).shrink(0))",
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
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert_clean(&over, "raw signal nested refusal attribution");
    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .unwrap();
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "raw signal nested refusal recovery");
    over.clear_active();

    // Parser-produced/function-Pattern routing, invalid/non-callable and
    // callable exotica, native own/custom-query replacement, aggregate opaque
    // lineage, exact V8 stacks/toString/absolute order, arbitrary callback
    // graphs/allocation/resources and asynchronous terminals remain residual.
}
