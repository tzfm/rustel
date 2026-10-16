use super::*;

#[test]
fn raw_keepif_keeps_direct_surface_truthiness_and_dynamic_routing() {
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
                   const raw = Pattern.prototype._keepif;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_keepif'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'keepif'
                   );
                   check(typeof raw === 'function'
                     && descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     'raw descriptor');
                   check(raw.name === '' && raw.length === 1
                     && Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype'
                     && Object.prototype.hasOwnProperty.call(
                       raw, 'prototype'
                     ), 'raw function shape');
                   const lengthDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'length'
                   );
                   const nameDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'name'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable,
                     'name/length descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public keepif getter descriptor');
                   const local = [
                     '_set', 'set', '_keep', 'keep', '_keepif', 'keepif'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|set|_keep|keep|_keepif|keepif',
                     'prototype composer order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|_keep|_keepif',
                     'enumerable composer order');
                   check(!('_keepif' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_keepif'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [true]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-keepif-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const source = {};
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(source), mapper(source, 'extra')];
                       },
                     });
                     try {
                       const truthy = apply(raw, receiver, [true]);
                       const falsey = apply(raw, receiver, [false]);
                       check(truthy.length === 2
                         && truthy.every(value => value === source)
                         && falsey.length === 2
                         && falsey.every(value => value === undefined),
                         'primitive receiver mapper values');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap-and-toboolean';
                   let gets = 0;
                   let calls = 0;
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('mapper inspected source'); },
                     ownKeys() {
                       throw new Error('mapper enumerated source');
                     },
                   });
                   const target = {};
                   const helper = new Proxy(function () {}, {
                     apply(_helper, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.prototype.hasOwnProperty.call(
                           mapper, 'prototype'
                         ), 'mapper reflection');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper(source, 'ignored-mapper-extra');
                     },
                   });
                   Object.defineProperty(target, 'fmap', {
                     configurable: true,
                     get() { gets++; return helper; },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('condition coercion hook read');
                       },
                     });
                   }
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   const falsey = [
                     undefined, null, false, 0, -0, 0n, NaN, ''
                   ];
                   const truthy = [
                     true, 1, -1, 1n, '0', Symbol('truthy'), hostile,
                     revoked
                   ];
                   for (const condition of falsey) {
                     check(apply(raw, target, [condition]) === undefined,
                       'falsey condition kept mapper input');
                   }
                   for (const condition of truthy) {
                     check(apply(raw, target, [condition]) === source,
                       'truthy condition discarded mapper input');
                   }
                   check(gets === falsey.length + truthy.length
                     && calls === gets && hookReads === 0
                     && proxyTraps === 0,
                     'dynamic fmap/ToBoolean phase count');
                   check(new Set(mappers).size === mappers.length,
                     'mapper callback was reused across calls');
                   check(apply(raw, target, [true, {}, 'extra']) === source,
                     'extra arguments affected direct composer');
                   check(apply(raw, target, []) === undefined,
                     'zero-argument condition was not undefined');

                   phase = 'delayed-closures';
                   const delayed = [];
                   const delayedTerminal = {};
                   const delayedTarget = {
                     fmap(mapper) {
                       delayed.push(mapper);
                       return delayedTerminal;
                     },
                   };
                   check(apply(raw, delayedTarget, [true])
                     === delayedTerminal
                     && apply(raw, delayedTarget, [false])
                       === delayedTerminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](source) === source
                     && delayed[1](source) === undefined,
                     'delayed mapper capture/freshness');

                   phase = 'cutoffs-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [hostile]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel && hookReads === 0,
                     'fmap getter cutoff/throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [hostile]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel && hookReads === 0,
                     'fmap call cutoff/throw identity');
                   const symbol = Symbol('raw-keepif-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return terminal; },
                     }, [true]), terminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [true]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorTerminal;
                   let constructorThis;
                   let constructorMapped;
                   const constructorSource = {};
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper(constructorSource);
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [false]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === undefined,
                     'object constructor terminal/false mapper');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [true]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === constructorSource,
                     'primitive constructor fallback/true mapper');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(constructorSource) === constructorSource,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [true], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'dynamic-native-and-poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   const replacementSource = {};
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper(replacementSource);
                     },
                   });
                   check(apply(raw, pure(0), [true])
                     === replacementSource && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned keepif surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_keepif', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'keepif', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.keepif = surfacePoison;
                   rustelScope.keepif = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(replacementSource); },
                   }, [true]) === replacementSource && poisonHits === 0,
                   'saved raw routed through poisoned keepif surface');

                   globalThis.rawKeepifSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepifSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepifSurfaceError"), None);
    assert_eq!(runtime.get_number("rawKeepifSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw keepif surface and direct routing");
}

#[test]
fn raw_keepif_preserves_undefined_haps_timing_metadata_and_explicit_residuals() {
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
                   const state = (begin, end) => ({
                     span: { begin, end }, controls: {},
                   });
                   const timing = hap => [
                     hap.whole.begin.show(), hap.whole.end.show(),
                     hap.part.begin.show(), hap.part.end.show(),
                   ].join(':');

                   phase = 'lazy-false-discriminator';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     const counted = new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         mapperCalls++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     });
                     return Reflect.apply(originalFmap, this, [counted]);
                   };
                   const exact = new Proxy({ marker: 'before' }, {});
                   const source = pure(exact).fast(2).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const rawFalse = source._keepif(false);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0,
                     'false mapper was eager');
                   check(rawFalse !== source
                     && rawFalse.query !== sourceQuery,
                     'raw false result/query identity');
                   check(rawFalse._steps !== sourceSteps
                     && rawFalse._steps.show() === sourceSteps.show(),
                     'raw false steps were not freshly preserved');
                   exact.marker = 'after';
                   const sourceHaps = source.query(state(0, 1));
                   const falseHaps = rawFalse.query(state(0, 1));
                   check(falseHaps.length === sourceHaps.length
                     && falseHaps.length === 2
                     && falseHaps.every(hap => hap.value === undefined)
                     && falseHaps.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === falseHaps.length,
                     'raw false removed haps or changed timing');
                   const publicFalse = source.keepif(false).query(state(0, 1));
                   check(publicFalse.length === 0,
                     'public false did not remove undefined haps');

                   phase = 'lazy-true';
                   let trueCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           trueCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const rawTrue = source._keepif(true);
                   Pattern.prototype.fmap = originalFmap;
                   check(trueCalls === 0, 'true mapper was eager');
                   const trueHaps = rawTrue.query(state(0, 1));
                   check(trueHaps.length === sourceHaps.length
                     && trueHaps.every(hap => hap.value === exact
                       && hap.value.marker === 'after')
                     && trueHaps.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && trueCalls === trueHaps.length,
                     'raw true changed values or timing');

                   phase = 'step-matrix';
                   for (const condition of [false, true]) {
                     const stepSources = [
                       pure('default'),
                       pure('none').setSteps(undefined),
                       pure('zero').setSteps(0),
                       pure('seven').setSteps(7),
                     ];
                     for (const stepSource of stepSources) {
                       const stepResult = stepSource._keepif(condition);
                       if (stepSource._steps === undefined) {
                         check(stepResult._steps === undefined,
                           'no-step source gained steps');
                       } else {
                         check(stepResult._steps !== stepSource._steps
                           && stepResult._steps.show()
                             === stepSource._steps.show(),
                           'defined steps were not freshly preserved');
                       }
                     }
                   }

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 9, end: 10 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   for (const condition of [false, true]) {
                     const tagResult = tagged._keepif(condition);
                     check(!Object.prototype.hasOwnProperty.call(
                       tagResult, '__pure'
                     ) && !Object.prototype.hasOwnProperty.call(
                       tagResult, '__pure_loc'
                     ), 'structural tags survived');
                   }

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literalTrue = pure('source')._keepif('truthy');
                   const literalFalse = pure('source')._keepif('');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalTrueHaps = literalTrue.query(state(0, 1));
                   const literalFalseHaps = literalFalse.query(state(0, 1));
                   check(parserCalls === 0
                     && literalTrueHaps.length === 1
                     && literalTrueHaps[0].value === 'source'
                     && literalFalseHaps.length === 1
                     && literalFalseHaps[0].value === undefined,
                     'parser reached or string truthiness changed at query');
                   setStringParser(undefined);

                   phase = 'exact-source-matrix';
                   const fn = function exactFunction() {};
                   const object = { marker: 'exact-object' };
                   const proxy = new Proxy({ marker: 'exact-proxy' }, {});
                   const array = [object, proxy];
                   for (const value of [
                     undefined, null, -0, 'exact-string', fn, object,
                     proxy, array
                   ]) {
                     const haps = pure(value)._keepif(true).query(state(0, 1));
                     check(haps.length === 1
                       && Object.is(haps[0].value, value),
                       'exact source value changed');
                   }

                   phase = 'pattern-control-cutoff';
                   let controlQueries = 0;
                   const patternControl = new Pattern(() => {
                     controlQueries++;
                     throw new Error('Pattern control was queried');
                   });
                   const controlled = pure('kept-source')
                     ._keepif(patternControl).query(state(0, 1));
                   check(controlled.length === 1
                     && controlled[0].value === 'kept-source'
                     && controlQueries === 0,
                     'Pattern control was coerced or queried');

                   phase = 'unsupported-identity-residuals';
                   for (const value of [
                     1n, Symbol('raw-keepif-symbol'), rev
                   ]) {
                     const haps = pure(value)._keepif(true).query(state(0, 1));
                     check(haps.length === 1
                       && !Object.is(haps[0].value, value),
                       'unsupported identity unexpectedly matched');
                   }

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure('native-source');
                   const reassigned = pure('pinned-reassigned');
                   nativeSource.query = reassigned.query;
                   const reassignedHaps = nativeSource._keepif(true).query(
                     state(0, 1)
                   );
                   check(reassignedHaps.length === 1
                     && reassignedHaps[0].value === 'native-source',
                     'native own-query residual changed');

                   phase = 'false-source-throw-cutoff';
                   let sourceCalls = 0;
                   let mappedCalls = 0;
                   const sentinel = { marker: 'source-query-throw' };
                   const throwingSource = new Pattern(() => {
                     sourceCalls++;
                     throw sentinel;
                   });
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mappedCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const stopped = throwingSource._keepif(false);
                   Pattern.prototype.fmap = originalFmap;
                   const stoppedHaps = stopped.query(state(0, 1));
                   check(stoppedHaps.length === 0 && sourceCalls === 1
                     && mappedCalls === 0,
                     'false condition skipped source or mapped after throw');

                   globalThis.rawKeepifSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepifSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepifSemanticError"), None);
    assert_eq!(runtime.get_number("rawKeepifSemanticOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "raw keepif false source throw did not retain its query error"
    );
    assert_clean(&runtime, "raw keepif lazy condition semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern-false');
                 const haps = pure(patternValue)._keepif(false).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('false Pattern-source residual changed');
                 }
                 globalThis.rawKeepifFalsePatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("rawKeepifFalsePatternResidualOkay"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "false Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern-true');
                 const haps = pure(patternValue)._keepif(true).query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('true Pattern-source residual changed');
                 }
                 globalThis.rawKeepifTruePatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("rawKeepifTruePatternResidualOkay"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "true Pattern-source residual lost its join-only query error"
    );

    // strudel.cc preserves one undefined hap for a false Pattern source
    // and one exact Pattern-valued hap for a true source. Native joins before
    // the mapper in both cases, as the executable residual witnesses pin.
    // Exact BigInt, Symbol, tagged-native callable, own/custom-query, exotic
    // `fmap`, and later query replacement remain bounded residuals.
}

#[test]
fn raw_keepif_retains_controls_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let controlQueries = 0;
             let condition = new Pattern(() => {
               controlQueries++;
               throw new Error('raw keepif condition was queried');
             });
             let sourceValue = { marker: 'raw-keepif-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-keepif-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawKeepifMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawKeepifControlQueries = () => controlQueries;
             globalThis.rawKeepifControlRef = new WeakRef(condition);
             globalThis.rawKeepifSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawKeepifDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keepif, source, [condition, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             condition = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw keepif mapper lost its JavaScript query owner"
    );
    assert_eq!(
        owned
            .active_pattern()
            .expect("raw keepif owned graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawKeepifControlAlive = Number(
                 rawKeepifControlRef.deref() instanceof Pattern
               );
               globalThis.rawKeepifSourceValueAlive = Number(
                 rawKeepifSourceValueRef.deref()?.marker
                   === 'raw-keepif-source'
               );
               globalThis.rawKeepifMapperAlive = Number(
                 typeof rawKeepifMapperRef.deref() === 'function'
               );
               globalThis.rawKeepifTransientsPruned = Number(
                 rawKeepifDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawKeepifControlAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepifSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepifMapperAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepifTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(
        values
            .iter()
            .all(|hap| hap.value.show().contains("marker:raw-keepif-source"))
    );
    owned
        .eval("globalThis.rawKeepifControlQueryCount = rawKeepifControlQueries();")
        .unwrap();
    assert_eq!(owned.get_number("rawKeepifControlQueryCount"), Some(0.0));
    assert_clean(&owned, "raw keepif captured condition ownership");
    owned.clear_active();
    drop(owned);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-keepif-js-terminal' };
             let condition = { marker: 'raw-keepif-js-condition' };
             let target = {};
             let extra = { marker: 'raw-keepif-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawKeepifJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawKeepifJsKeptRef = new WeakRef(kept);
             globalThis.rawKeepifJsDroppedRefs = [
               new WeakRef(condition), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keepif, target, [condition, extra]
             );
             kept = condition = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawKeepifJsKeptAlive = Number(
                 rawKeepifJsKeptRef.deref()?.marker
                   === 'raw-keepif-js-terminal'
               );
               globalThis.rawKeepifJsMapperPruned = Number(
                 rawKeepifJsMapperRef.deref() === undefined
               );
               globalThis.rawKeepifJsTransientsPruned = Number(
                 rawKeepifJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawKeepifJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawKeepifJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawKeepifJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-keepif-js-terminal"
    );
    assert_clean(&js_terminal, "raw keepif JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let condition = { marker: 'raw-keepif-unused-condition' };
             let target = {};
             let extra = { marker: 'raw-keepif-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawKeepifTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-keepif-terminal');
             };
             target.fmap = helper;
             globalThis.rawKeepifTerminalDroppedRefs = [
               new WeakRef(condition), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keepif, target, [condition, extra]
             );
             condition = target = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        !terminal.active_needs_host(),
        "custom host-free terminal retained raw dispatch transients"
    );
    for _ in 0..3 {
        terminal.run_gc();
    }
    terminal
        .eval(
            r#"globalThis.rawKeepifTerminalTransientsPruned = Number(
                 rawKeepifTerminalMapperRef.deref() === undefined
                 && rawKeepifTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawKeepifTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-keepif-terminal"
    );
    assert_clean(&terminal, "raw keepif custom terminal pruning");
    terminal.clear_active();

    for condition in ["false", "true"] {
        rustel_core::reset_stepwise_entries_materialised();
        let direct = semantic_runtime(&format!("pure(0).fast(4).setSteps(5)._keepif({condition})"));
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "raw keepif charged construction-time stepwise work"
        );
        query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "raw keepif charged query-time stepwise work"
        );
        assert_clean(&direct, "raw keepif direct pool neutrality");
        direct.clear_active();
    }

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._keepif(false)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "false raw keepif suppressed or added nested source work"
    );
    assert_clean(&exact, "raw keepif nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({}).shrink(0))._keepif(false)",
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
    assert_clean(&over, "raw keepif nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._keepif(true)"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw keepif nested recovery");
    recovery.clear_active();

    // The direct raw adds no operation or shared-stepwise charge, but its
    // mapper remains one JavaScript callback per source hap. Aggregate
    // Pattern lineage, arbitrary callback graphs, exotic `fmap`, density and
    // general allocation, exact V8 stack/toString/absolute order, and broad
    // cancellation/deadline behavior remain explicit residuals.
}
