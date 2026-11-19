use super::*;

#[test]
fn raw_keep_keeps_direct_composer_surface_routing_and_constructors() {
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
                   const raw = Pattern.prototype._keep;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_keep'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'keep'
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
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public keep getter descriptor');
                   const local = ['_set', 'set', '_keep', 'keep'];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|set|_keep|keep',
                     'prototype composer order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_set|_keep', 'enumerable composer order');
                   check(!('_keep' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_keep'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [{ ignored: true }]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-keep-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const source = new Proxy({}, {
                       get() { throw new Error('mapper inspected source'); },
                     });
                     const ignored = new Proxy({}, {
                       get() { throw new Error('mapper inspected value'); },
                       ownKeys() {
                         throw new Error('mapper enumerated value');
                       },
                     });
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return mapper(source);
                       },
                     });
                     try {
                       check(apply(raw, receiver, [ignored]) === source,
                         'primitive receiver mapper did not keep x');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap';
                   let gets = 0;
                   let calls = 0;
                   let maps = 0;
                   const mappers = [];
                   const source = new Proxy({}, {
                     get() { throw new Error('lexical mapper read source'); },
                     ownKeys() {
                       throw new Error('lexical mapper enumerated source');
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
                       maps++;
                       return mapper(source);
                     },
                   });
                   Object.defineProperty(target, 'fmap', {
                     configurable: true,
                     get() { gets++; return helper; },
                   });
                   let opPoisonHits = 0;
                   const opPoison = () => {
                     opPoisonHits++;
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = opPoison;
                   target.op = opPoison;
                   const ignored = new Proxy({ marker: 'ignored' }, {
                     get() { throw new Error('ignored value was read'); },
                     ownKeys() {
                       throw new Error('ignored value was enumerated');
                     },
                   });
                   check(apply(raw, target, []) === source,
                     'zero-argument mapper changed x');
                   check(apply(raw, target, [ignored]) === source,
                     'one-argument mapper selected value');
                   check(apply(raw, target, [ignored, {}, 'extra'])
                     === source, 'extras retargeted direct composer');
                   check(gets === 3 && calls === 3 && maps === 3,
                     'dynamic fmap phase count');
                   check(new Set(mappers).size === 3,
                     'mapper callback was reused across calls');
                   check(opPoisonHits === 0,
                     'mapper resolved op outside its lexical factory');

                   phase = 'delayed-closure';
                   let delayedMapper;
                   const delayedTerminal = {};
                   const delayedTarget = {
                     fmap(mapper) {
                       delayedMapper = mapper;
                       return delayedTerminal;
                     },
                   };
                   const delayedIgnored = new Proxy({}, {
                     get() { throw new Error('delayed value was read'); },
                   });
                   check(apply(raw, delayedTarget, [delayedIgnored])
                     === delayedTerminal, 'delayed terminal');
                   const delayedSource = {};
                   check(delayedMapper(delayedSource, 'extra')
                     === delayedSource, 'delayed mapper did not keep x');

                   phase = 'cutoffs-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [ignored]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [ignored]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-keep-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return terminal; },
                     }, [ignored]), terminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [ignored]); }
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
                   const objectResult = Reflect.construct(raw, [ignored]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === constructorSource,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [ignored]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === constructorSource,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(constructorSource) === constructorSource,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [ignored], Alternate
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
                   check(apply(raw, pure(0), [ignored])
                     === replacementSource && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );

                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned keep surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_keep', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   Object.defineProperty(Pattern.prototype, 'keep', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   globalThis.keep = poison;
                   rustelScope.keep = poison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(replacementSource); },
                   }, [ignored]) === replacementSource && poisonHits === 0,
                   'saved raw routed through poisoned keep surface');

                   globalThis.rawKeepSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepSurfaceError"), None);
    assert_eq!(runtime.get_number("rawKeepSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw keep surface and direct routing");
}

#[test]
fn raw_keep_keeps_lazy_source_values_steps_tags_and_explicit_residuals() {
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

                   phase = 'lazy-exact-proxy';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   let mapperShape = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     mapperShape = Number(mapper.name === ''
                       && mapper.length === 1
                       && !Object.prototype.hasOwnProperty.call(
                         mapper, 'prototype'
                       ));
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
                   const ignored = new Proxy({ marker: 'ignored' }, {
                     get() { throw new Error('ignored value was read'); },
                     ownKeys() {
                       throw new Error('ignored value was enumerated');
                     },
                   });
                   const result = source._keep(ignored);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0 && mapperShape === 1,
                     'mapper was eager or malformed');
                   check(result !== source, 'result identity');
                   check(result.query !== sourceQuery,
                     'result query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     `result steps:${result._steps}:${sourceSteps}`);
                   exact.marker = 'after';
                   const sourceHaps = source.query(state(0, 1));
                   const resultHaps = result.query(state(0, 1));
                   check(resultHaps.length === sourceHaps.length
                     && resultHaps.length === 2
                     && resultHaps.every(hap => hap.value === exact
                       && hap.value.marker === 'after')
                     && resultHaps.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === resultHaps.length,
                     'lazy exact source/timing query');

                   phase = 'step-matrix';
                   const stepSources = [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ];
                   for (const stepSource of stepSources) {
                     const stepResult = stepSource._keep('ignored');
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

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   tagged.__pure_loc = { start: 7, end: 8 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._keep('ignored');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   const parserSource = pure('source');
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literal = parserSource._keep('ignored-literal');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalHaps = literal.query(state(0, 1));
                   check(parserCalls === 0 && literalHaps.length === 1
                     && literalHaps[0].value === 'source',
                     'parser reached during query');
                   setStringParser(undefined);

                   phase = 'exact-source-matrix';
                   const fn = function exactFunction() {};
                   const object = { marker: 'exact-object' };
                   const proxy = new Proxy({ marker: 'exact-proxy' }, {});
                   const array = [object, proxy];
                   const values = [
                     undefined, null, -0, 'exact-string', fn, object,
                     proxy, array
                   ];
                   for (const value of values) {
                     const haps = pure(value)._keep('ignored').query(
                       state(0, 1)
                     );
                     check(haps.length === 1
                       && Object.is(haps[0].value, value),
                       'exact source value changed');
                   }

                   phase = 'ignored-pattern-cutoff';
                   let ignoredPatternQueries = 0;
                   const ignoredPattern = new Pattern(() => {
                     ignoredPatternQueries++;
                     throw new Error('ignored Pattern was queried');
                   });
                   const ignoredPatternHaps = pure('kept-source')
                     ._keep(ignoredPattern).query(state(0, 1));
                   check(ignoredPatternHaps.length === 1
                     && ignoredPatternHaps[0].value === 'kept-source'
                     && ignoredPatternQueries === 0,
                     'ignored Pattern was inspected or queried');

                   phase = 'unsupported-identity-residuals';
                   for (const value of [
                     1n, Symbol('raw-keep-symbol'), rev
                   ]) {
                     const haps = pure(value)._keep('ignored').query(
                       state(0, 1)
                     );
                     check(haps.length === 1
                       && !Object.is(haps[0].value, value),
                       'unsupported identity unexpectedly matched');
                   }
                   phase = 'query-reassignment-residual';
                   const nativeSource = pure('native-source');
                   const reassigned = pure('pinned-reassigned');
                   nativeSource.query = reassigned.query;
                   const reassignedHaps = nativeSource._keep().query(
                     state(0, 1)
                   );
                   check(reassignedHaps.length === 1
                     && reassignedHaps[0].value === 'native-source',
                     'native own-query residual changed');

                   phase = 'source-throw-cutoff';
                   let sourceCalls = 0;
                   let mappedCalls = 0;
                   const sentinel = { marker: 'source-query-throw' };
                   const throwingSource = new Pattern(() => {
                     sourceCalls++;
                     throw sentinel;
                   });
                   const throwingOriginal = Pattern.prototype.fmap;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(throwingOriginal, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mappedCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const stopped = throwingSource._keep('ignored');
                   Pattern.prototype.fmap = throwingOriginal;
                   const stoppedHaps = stopped.query(state(0, 1));
                   check(stoppedHaps.length === 0 && sourceCalls === 1
                     && mappedCalls === 0,
                     'source throw did not stop mapping');

                   globalThis.rawKeepSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawKeepSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawKeepSemanticError"), None);
    assert_eq!(runtime.get_number("rawKeepSemanticOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "raw keep source throw did not retain its query error"
    );
    assert_clean(&runtime, "raw keep lazy source-value semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const patternHaps = pure(patternValue)
                   ._keep('ignored').query({
                     span: { begin: 0, end: 1 }, controls: {},
                   });
                 if (patternHaps.length !== 0) {
                   throw new Error('direct Pattern-valued residual changed');
                 }
                 globalThis.rawKeepPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawKeepPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "direct Pattern-valued residual lost its join-only query error"
    );

    // strudel.cc follows an own-reassigned `query` and preserves exact
    // BigInt, Symbol, tagged-native callable, and Pattern identities. The
    // executable witnesses above deliberately pin the first native gaps;
    // exotic `fmap` and later query replacement stay residual rather than
    // broadening this direct-composer checkpoint.
}

#[test]
fn raw_keep_retains_mapper_inputs_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let ignored = { marker: 'raw-keep-captured' };
             let sourceValue = { marker: 'raw-keep-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-keep-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawKeepMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawKeepCapturedRef = new WeakRef(ignored);
             globalThis.rawKeepSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawKeepDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keep, source, [ignored, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             ignored = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw keep mapper lost its JavaScript query owner"
    );
    assert_eq!(
        owned.active_pattern().expect("raw keep owned graph").steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawKeepCapturedAlive = Number(
                 rawKeepCapturedRef.deref()?.marker === 'raw-keep-captured'
               );
               globalThis.rawKeepSourceValueAlive = Number(
                 rawKeepSourceValueRef.deref()?.marker === 'raw-keep-source'
               );
               globalThis.rawKeepMapperAlive = Number(
                 typeof rawKeepMapperRef.deref() === 'function'
               );
               globalThis.rawKeepTransientsPruned = Number(
                 rawKeepDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawKeepCapturedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepMapperAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawKeepTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(
        values
            .iter()
            .all(|hap| matches!(&hap.value, Value::Object(_))),
        "outer query left JavaScript-owned source values opaque"
    );
    assert_clean(&owned, "raw keep captured mapper ownership");
    owned.clear_active();
    drop(owned);
    assert!(
        values
            .iter()
            .all(|hap| hap.value.show().contains("marker:raw-keep-source"))
    );

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-keep-js-terminal' };
             let target = {};
             let ignored = { marker: 'raw-keep-js-ignored' };
             let extra = { marker: 'raw-keep-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawKeepJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawKeepJsKeptRef = new WeakRef(kept);
             globalThis.rawKeepJsDroppedRefs = [
               new WeakRef(target), new WeakRef(ignored),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keep, target, [ignored, extra]
             );
             kept = target = ignored = extra = helper = null;
             return result;
           })()"#,
    );
    assert!(
        js_terminal.active_needs_host(),
        "custom JavaScript terminal lost its query owner"
    );
    assert_eq!(
        js_terminal
            .active_pattern()
            .expect("raw keep JavaScript terminal")
            .steps,
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        js_terminal.run_gc();
    }
    js_terminal
        .eval(
            r#"globalThis.rawKeepJsKeptAlive = Number(
                 rawKeepJsKeptRef.deref()?.marker
                   === 'raw-keep-js-terminal'
               );
               globalThis.rawKeepJsMapperPruned = Number(
                 rawKeepJsMapperRef.deref() === undefined
               );
               globalThis.rawKeepJsTransientsPruned = Number(
                 rawKeepJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawKeepJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawKeepJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawKeepJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-keep-js-terminal"
    );
    assert_clean(&js_terminal, "raw keep JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let target = {};
             let ignored = { marker: 'raw-keep-unused-value' };
             let extra = { marker: 'raw-keep-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawKeepTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-keep-terminal');
             };
             target.fmap = helper;
             globalThis.rawKeepTerminalDroppedRefs = [
               new WeakRef(target), new WeakRef(ignored),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._keep, target, [ignored, extra]
             );
             target = ignored = extra = helper = null;
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
            r#"globalThis.rawKeepTerminalTransientsPruned = Number(
                 rawKeepTerminalMapperRef.deref() === undefined
                 && rawKeepTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawKeepTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-keep-terminal"
    );
    assert_clean(&terminal, "raw keep custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(0).fast(4).setSteps(5)._keep('ignored')");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw keep charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw keep charged query-time stepwise work"
    );
    assert_clean(&direct, "raw keep direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure('kept')._keep('ignored');
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw keep consumed shared stepwise allowance"
    );
    assert_clean(&exact, "raw keep nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure('kept')._keep('ignored');
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
    assert_clean(&over, "raw keep nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure('kept')._keep('ignored');
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw keep nested recovery");
    recovery.clear_active();

    // strudel.cc retains the source wrapper through its dynamic query
    // closure, while the native graph retains the source value but prunes the
    // wrapper. Pattern-valued identity, aggregate lineage, exotic `fmap`,
    // density/general allocation, exact V8 stacks/toString/absolute order,
    // and broad deadline/cancellation bounds remain residuals. `_keep` adds no
    // resource operation or shared-stepwise charge.
}
