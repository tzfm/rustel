use super::*;

#[test]
fn raw_set_keeps_direct_composer_surface_routing_and_constructors() {
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
                   const raw = Pattern.prototype._set;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_set'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'set'
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
                     'public set getter descriptor');
                   const pair = ['_set', 'set'];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => pair.includes(name)).join('|')
                       === '_set|set', 'prototype pair order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => pair.includes(name)).join('|')
                       === '_set', 'enumerable pair order');
                   check(!('_set' in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, '_set'
                     ), 'raw escaped prototype');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [1]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-set-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     const expected = { receiver };
                     const source = new Proxy({}, {
                       get() { throw new Error('mapper inspected source'); },
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
                       check(apply(raw, receiver, [expected]) === expected,
                         'primitive receiver result changed');
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
                   const exact = new Proxy({ marker: 'exact' }, {});
                   check(apply(raw, target, []) === undefined,
                     'zero-argument value');
                   check(apply(raw, target, [exact]) === exact,
                     'one-argument exact value');
                   check(apply(raw, target, [exact, {}, 'ignored']) === exact,
                     'extras changed direct value capture');
                   check(gets === 3 && calls === 3 && maps === 3,
                     'dynamic fmap phase count');
                   check(new Set(mappers).size === 3,
                     'mapper callback was reused across calls');

                   phase = 'delayed-closure';
                   let delayedMapper;
                   const delayedTerminal = {};
                   const delayedTarget = {
                     fmap(mapper) {
                       delayedMapper = mapper;
                       return delayedTerminal;
                     },
                   };
                   const delayedValue = { version: 1 };
                   check(apply(raw, delayedTarget, [delayedValue])
                     === delayedTerminal, 'delayed terminal');
                   delayedValue.version = 2;
                   check(delayedMapper(new Proxy({}, {
                     get() { throw new Error('delayed mapper read source'); },
                   })) === delayedValue && delayedValue.version === 2,
                   'delayed value capture');

                   phase = 'cutoffs-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [exact]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [exact]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-set-terminal');
                   const object = {};
                   const callable = function terminal() {};
                   const promise = Promise.resolve('terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return terminal; },
                     }, [exact]), terminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let constructorTerminal;
                   let constructorThis;
                   let constructorMapped;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     constructorMapped = mapper('source');
                     return constructorTerminal;
                   };
                   const constructorValue = {};
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(
                     raw, [constructorValue]
                   );
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw
                     && constructorMapped === constructorValue,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(
                     raw, [constructorValue]
                   );
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw
                     && constructorMapped === constructorValue,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper('alternate') === constructorValue,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [constructorValue], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'dynamic-native-and-poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return mapper('replacement-source');
                     },
                   });
                   const replacementValue = {};
                   check(apply(raw, pure(0), [replacementValue])
                     === replacementValue && replacementHits === 1,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );

                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned set surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_set', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   Object.defineProperty(Pattern.prototype, 'set', {
                     configurable: true, writable: true,
                     enumerable: true, value: poison,
                   });
                   globalThis.set = poison;
                   rustelScope.set = poison;
                   const poisonValue = {};
                   check(apply(raw, {
                     fmap(mapper) { return mapper('source'); },
                   }, [poisonValue]) === poisonValue && poisonHits === 0,
                   'saved raw routed through poisoned set surface');

                   globalThis.rawSetSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawSetSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSetSurfaceError"), None);
    assert_eq!(runtime.get_number("rawSetSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw set surface and direct routing");
}

#[test]
fn raw_set_keeps_lazy_exact_values_steps_tags_and_parser_cutoffs() {
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
                   const source = pure(0).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const exact = new Proxy({ marker: 'before' }, {});
                   const result = source._set(exact);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0 && mapperShape === 1,
                     'mapper was eager or malformed');
                   check(result !== source, 'result identity');
                   check(result.query !== sourceQuery,
                     'result query identity');
                   check(result._steps !== sourceSteps
                     && Number(result._steps) === Number(sourceSteps),
                     `result steps:${result._steps}:${sourceSteps}`);
                   exact.marker = 'after';
                   const first = result.query(state(0, 1));
                   check(first.length === 1 && first[0].value === exact
                     && first[0].value.marker === 'after'
                     && mapperCalls === 1,
                     'first exact Proxy query');
                   const second = result.query(state(1, 2));
                   check(second.length === 1 && second[0].value === exact
                     && mapperCalls === 2,
                     'second exact Proxy query');

                   phase = 'tag-loss';
                   const tagged = pure('tagged-source');
                   const location = { start: 7, end: 8 };
                   tagged.__pure_loc = location;
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._set('tagged-result');
                   check(!Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure'
                   ) && !Object.prototype.hasOwnProperty.call(
                     tagResult, '__pure_loc'
                   ), 'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const literal = pure('source')._set('literal-value');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const literalHaps = literal.query(state(0, 1));
                   check(parserCalls === 0 && literalHaps.length === 1
                     && literalHaps[0].value === 'literal-value',
                     'parser reached during query');
                   setStringParser(undefined);

                   phase = 'exact-value-matrix';
                   const fn = function exactFunction() {};
                   const values = [
                     undefined, null, -0, 'exact-string', fn
                   ];
                   for (const value of values) {
                     const haps = pure('source')._set(value).query(
                       state(0, 1)
                     );
                     check(haps.length === 1
                       && Object.is(haps[0].value, value),
                       'exact value matrix changed');
                   }

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
                   const stopped = throwingSource._set('unreachable');
                   Pattern.prototype.fmap = throwingOriginal;
                   const stoppedHaps = stopped.query(state(0, 1));
                   check(stoppedHaps.length === 0 && sourceCalls === 1
                     && mappedCalls === 0,
                     'source throw did not stop mapping');

                   globalThis.rawSetSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawSetSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawSetSemanticError"), None);
    assert_eq!(runtime.get_number("rawSetSemanticOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "raw set source throw did not retain its query error"
    );
    assert_clean(&runtime, "raw set lazy exact-value semantics");

    // Own/custom `query` replacement, exact Pattern/Symbol/BigInt values, and
    // tagged-native callable execution (including `rev`) remain outside this
    // direct native-fmap checkpoint; `_set` deliberately follows the
    // receiver's current `fmap`.
}

#[test]
fn raw_set_retains_values_prunes_transients_and_keeps_nested_accounting() {
    let owned = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-set-kept' };
             let sourceValue = { marker: 'raw-set-source' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-set-extra' };
             globalThis.rawSetKeptRef = new WeakRef(kept);
             globalThis.rawSetSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawSetDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._set, source, [kept, extra]
             );
             kept = sourceValue = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "captured JavaScript value lost its query owner"
    );
    assert_eq!(
        owned.active_pattern().expect("raw set owned graph").steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawSetKeptAlive = Number(
                 rawSetKeptRef.deref()?.marker === 'raw-set-kept'
               );
               globalThis.rawSetSourceValueAlive = Number(
                 rawSetSourceValueRef.deref()?.marker === 'raw-set-source'
               );
               globalThis.rawSetTransientsPruned = Number(
                 rawSetDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawSetKeptAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSetSourceValueAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawSetTransientsPruned"), Some(1.0));
    let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(values.len(), 4);
    assert!(
        values
            .iter()
            .all(|hap| matches!(&hap.value, Value::Object(_))),
        "outer query left JavaScript-owned values opaque"
    );
    assert_clean(&owned, "raw set captured-value ownership");
    owned.clear_active();
    drop(owned);
    assert!(
        values
            .iter()
            .all(|hap| hap.value.show().contains("marker:raw-set-kept"))
    );

    let terminal = semantic_runtime(
        r#"(() => {
             let target = {};
             let value = { marker: 'raw-set-unused-value' };
             let extra = { marker: 'raw-set-terminal-extra' };
             let helper = function () { return pure('raw-set-terminal'); };
             target.fmap = helper;
             globalThis.rawSetTerminalDroppedRefs = [
               new WeakRef(target), new WeakRef(value),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._set, target, [value, extra]
             );
             target = value = extra = helper = null;
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
            r#"globalThis.rawSetTerminalTransientsPruned = Number(
                 rawSetTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawSetTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-set-terminal"
    );
    assert_clean(&terminal, "raw set custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(0).fast(4).setSteps(5)._set(1)");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw set charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw set charged query-time stepwise work"
    );
    assert_clean(&direct, "raw set direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._set('mapped');
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw set consumed shared stepwise allowance"
    );
    assert_clean(&exact, "raw set nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1)).polyBind(value => {{
             if (value === 0) return pure(0)._set('mapped');
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
    assert_clean(&over, "raw set nested refusal attribution");
    over.clear_active();

    // Aggregate Pattern-valued lineage, exotic `fmap`, Rust/CLI Proxy
    // materialisation and trap phase, density/general allocation bounds,
    // exact V8 stacks/toString/absolute order, and native own/custom-query
    // replacement remain explicit residuals. `_set` itself adds no resource
    // operation or shared-stepwise charge.
}
