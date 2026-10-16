use super::*;

#[test]
fn raw_and_keeps_direct_surface_operand_routing_and_constructors() {
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
                   const raw = Pattern.prototype._and;

                   phase = 'surface';
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, '_and'
                   );
                   const publicDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'and'
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
                   const prototypeDescriptor = Object.getOwnPropertyDescriptor(
                     raw, 'prototype'
                   );
                   check(!lengthDescriptor.writable
                     && !lengthDescriptor.enumerable
                     && lengthDescriptor.configurable
                     && !nameDescriptor.writable
                     && !nameDescriptor.enumerable
                     && nameDescriptor.configurable
                     && prototypeDescriptor.writable
                     && !prototypeDescriptor.enumerable
                     && !prototypeDescriptor.configurable,
                     'function descriptors');
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, 'raw is not constructible');
                   check(typeof publicDescriptor.get === 'function'
                     && !('value' in publicDescriptor)
                     && !publicDescriptor.enumerable
                     && publicDescriptor.configurable,
                     'public and getter descriptor');
                   const local = [
                     '_net', 'net', '_and', 'and', '_or', 'or'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_net|net|_and|and|_or|or',
                     'filtered logical-and order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => local.includes(name)).join('|')
                       === '_net|_and|_or',
                     'enumerable logical-and order');
                   check(!Object.hasOwn(globalThis, '_and')
                     && !Object.hasOwn(rustelScope, '_and')
                     && Object.hasOwn(Pattern.prototype, '_or'),
                     'raw escaped prototype or failed to open _or');

                   phase = 'strict-receivers';
                   for (const receiver of [null, undefined]) {
                     let caught;
                     try { apply(raw, receiver, [receiver]); }
                     catch (error) { caught = error; }
                     check(caught instanceof TypeError,
                       'nullish receiver did not fail strictly');
                   }
                   const selected = { marker: 'selected' };
                   const truthy = { marker: 'truthy' };
                   for (const receiver of [
                     7, 'x', false, 1n, Symbol('raw-and-receiver')
                   ]) {
                     const prototype = Object.getPrototypeOf(Object(receiver));
                     const prior = Object.getOwnPropertyDescriptor(
                       prototype, 'fmap'
                     );
                     Object.defineProperty(prototype, 'fmap', {
                       configurable: true,
                       value: function (mapper) {
                         'use strict';
                         check(Object.is(this, receiver),
                           'primitive receiver was boxed');
                         return [mapper(false), mapper(truthy)];
                       },
                     });
                     try {
                       const result = apply(raw, receiver, [selected, {}]);
                       check(result.length === 2
                         && result[0] === false
                         && result[1] === selected,
                         'primitive receiver operand routing');
                     } finally {
                       if (prior) {
                         Object.defineProperty(prototype, 'fmap', prior);
                       } else {
                         delete prototype.fmap;
                       }
                     }
                   }

                   phase = 'dynamic-fmap-and-mutations';
                   let gets = 0;
                   let calls = 0;
                   let target;
                   const mappers = [];
                   const method = new Proxy(function () {}, {
                     apply(_method, thisArg, args) {
                       calls++;
                       check(thisArg === target && args.length === 1,
                         'fmap call receiver/arity');
                       const mapper = args[0];
                       mappers.push(mapper);
                       check(mapper.name === '' && mapper.length === 1
                         && Reflect.ownKeys(mapper).join('|')
                           === 'length|name'
                         && !Object.hasOwn(mapper, 'prototype'),
                         'mapper reflection');
                       let constructError;
                       try { Reflect.construct(mapper, []); }
                       catch (error) { constructError = error; }
                       check(constructError instanceof TypeError,
                         'mapper became constructible');
                       return mapper;
                     },
                   });
                   target = new Proxy({}, {
                     get(_target, key, receiver) {
                       gets++;
                       check(key === 'fmap' && receiver === target,
                         'unexpected receiver get');
                       return method;
                     },
                   });
                   const poison = () => {
                     throw new Error('lexical op decoy reached');
                   };
                   globalThis.op = poison;
                   target.op = poison;
                   const ignoredExtra = new Proxy({}, {
                     get() {
                       throw new Error('ignored argument was read');
                     },
                     ownKeys() {
                       throw new Error('ignored argument was enumerated');
                     },
                   });
                   const rightTruthy = { marker: 'right-truthy' };
                   const leftTruthy = { marker: 'left-truthy' };
                   const mapperTruthy = apply(
                     raw, target, [rightTruthy, ignoredExtra, ignoredExtra]
                   );
                   const mapperZero = apply(raw, target, [0]);
                   const mapperUndefined = apply(raw, target, []);
                   check(gets === 3 && calls === 3
                     && new Set(mappers).size === 3,
                     'dynamic fmap or mapper freshness');
                   check(mapperTruthy(leftTruthy) === rightTruthy,
                     'truthy lhs did not select rhs');
                   check(mapperTruthy(false) === false,
                     'false lhs was not preserved');
                   check(Object.is(mapperTruthy(-0), -0),
                     'signed-zero lhs was not preserved');
                   check(Object.is(mapperTruthy(NaN), NaN),
                     'NaN lhs was not preserved');
                   check(mapperZero(false) === false,
                     'swapped logical-and mutation survived');
                   check(mapperUndefined(leftTruthy) === undefined
                     && mapperUndefined(false) === false,
                     'zero-argument capture changed');

                   phase = 'delayed-capture';
                   const delayed = [];
                   const terminal = {};
                   const delayedTarget = {
                     fmap(mapper) { delayed.push(mapper); return terminal; },
                   };
                   const firstRight = { marker: 'first-right' };
                   const secondRight = { marker: 'second-right' };
                   check(apply(raw, delayedTarget, [firstRight]) === terminal
                     && apply(raw, delayedTarget, [secondRight]) === terminal,
                     'delayed terminals');
                   check(delayed.length === 2 && delayed[0] !== delayed[1]
                     && delayed[0](leftTruthy) === firstRight
                     && delayed[1](leftTruthy) === secondRight
                     && delayed[0](false) === false,
                     'delayed mapper capture/freshness');

                   phase = 'throws-and-terminals';
                   const getterSentinel = { marker: 'getter-throw' };
                   let caught;
                   try {
                     apply(raw, Object.defineProperty({}, 'fmap', {
                       get() { throw getterSentinel; },
                     }), [selected]);
                   } catch (error) { caught = error; }
                   check(caught === getterSentinel,
                     'fmap getter throw identity');
                   const callSentinel = { marker: 'call-throw' };
                   caught = undefined;
                   try {
                     apply(raw, { fmap() { throw callSentinel; } }, [selected]);
                   } catch (error) { caught = error; }
                   check(caught === callSentinel,
                     'fmap call throw identity');
                   const symbol = Symbol('raw-and-terminal');
                   const object = {};
                   const callable = function terminalFunction() {};
                   const promise = Promise.resolve('terminal');
                   for (const customTerminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(raw, {
                       fmap() { return customTerminal; },
                     }, [selected]), customTerminal),
                     'custom fmap terminal changed');
                   }

                   phase = 'constructors';
                   let defaultError;
                   try { Reflect.construct(raw, [selected]); }
                   catch (error) { defaultError = error; }
                   check(defaultError instanceof TypeError,
                     'default constructor found fmap');
                   let constructorThis;
                   let constructorTerminal;
                   raw.prototype.fmap = function (mapper) {
                     constructorThis = this;
                     check(mapper(false) === false,
                       'constructor lost false lhs');
                     check(mapper(leftTruthy) === selected,
                       'constructor lost selected rhs');
                     return constructorTerminal;
                   };
                   constructorTerminal = { marker: 'object-terminal' };
                   const objectResult = Reflect.construct(raw, [selected]);
                   check(objectResult === constructorTerminal
                     && constructorThis instanceof raw,
                     'object constructor terminal');
                   constructorTerminal = 7;
                   const primitiveResult = Reflect.construct(raw, [selected]);
                   check(primitiveResult === constructorThis
                     && primitiveResult instanceof raw,
                     'primitive constructor fallback');
                   function Alternate() {}
                   Alternate.prototype.fmap = function (mapper) {
                     check(this instanceof Alternate,
                       'custom newTarget receiver');
                     check(mapper(leftTruthy) === selected,
                       'custom newTarget mapper');
                     return 3;
                   };
                   const alternate = Reflect.construct(
                     raw, [selected], Alternate
                   );
                   check(alternate instanceof Alternate,
                     'custom newTarget fallback');

                   phase = 'poisoning';
                   const originalFmap = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'fmap'
                   );
                   let replacementHits = 0;
                   Object.defineProperty(Pattern.prototype, 'fmap', {
                     configurable: true,
                     writable: true,
                     value(mapper) {
                       replacementHits++;
                       return [mapper(false), mapper(leftTruthy)];
                     },
                   });
                   const replacement = apply(raw, pure(0), [selected]);
                   check(replacementHits === 1
                     && replacement[0] === false
                     && replacement[1] === selected,
                     'saved raw did not follow replaced fmap');
                   Object.defineProperty(
                     Pattern.prototype, 'fmap', originalFmap
                   );
                   let poisonHits = 0;
                   const surfacePoison = () => {
                     poisonHits++;
                     throw new Error('poisoned and surface reached');
                   };
                   Object.defineProperty(Pattern.prototype, '_and', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   Object.defineProperty(Pattern.prototype, 'and', {
                     configurable: true, writable: true,
                     enumerable: true, value: surfacePoison,
                   });
                   globalThis.and = surfacePoison;
                   rustelScope.and = surfacePoison;
                   check(apply(raw, {
                     fmap(mapper) { return mapper(leftTruthy); },
                   }, [selected]) === selected && poisonHits === 0,
                     'saved raw routed through poisoned and surface');

                   globalThis.rawAndSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawAndSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawAndSurfaceError"), None);
    assert_eq!(runtime.get_number("rawAndSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw and surface and operand routing");
}

#[test]
fn raw_and_is_lazy_truthy_metadata_safe_and_pins_bridge_residuals() {
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

                   phase = 'lazy-repeated';
                   const originalFmap = Pattern.prototype.fmap;
                   let mapperCalls = 0;
                   Pattern.prototype.fmap = function (mapper) {
                     return Reflect.apply(originalFmap, this, [
                       new Proxy(mapper, {
                         apply(target, thisArg, args) {
                           mapperCalls++;
                           return Reflect.apply(target, thisArg, args);
                         },
                       }),
                     ]);
                   };
                   const selected = { marker: 'selected' };
                   const truthy = { marker: 'truthy' };
                   const source = fastcat(
                     pure(false), pure(truthy), pure(NaN)
                   ).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const result = source._and(selected);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0, 'logical and was eager');
                   check(result !== source && result.query !== sourceQuery,
                     'result/query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     'steps were not freshly preserved');
                   const sourceHaps = source.query(state(0, 1));
                   const first = result.query(state(0, 1));
                   const second = result.query(state(0, 1));
                   const routed = haps => haps.length === 3
                     && Object.is(haps[0].value, false)
                     && Object.is(haps[1].value, selected)
                     && Object.is(haps[2].value, NaN);
                   check(routed(first) && routed(second)
                     && first.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && second.map(timing).join('|')
                       === sourceHaps.map(timing).join('|')
                     && mapperCalls === 6,
                     'lazy repeated operand routing');

                   phase = 'truthiness-matrix';
                   let matrixCase = 0;
                   const exact = (left, right, expected) => {
                     matrixCase += 1;
                     const haps = pure(left)._and(right).query(state(0, 1));
                     check(haps.length === 1
                       && Object.is(haps[0].value, expected),
                       `truthiness matrix ${matrixCase}`);
                   };
                   exact(undefined, selected, undefined);
                   exact(null, selected, null);
                   exact(false, selected, false);
                   exact(0, selected, 0);
                   exact(-0, selected, -0);
                   exact(NaN, selected, NaN);
                   exact('', selected, '');
                   exact(true, selected, selected);
                   exact(1, selected, selected);
                   exact(-1, selected, selected);
                   exact('left', selected, selected);
                   const leftObject = { marker: 'left-object' };
                   const rightObject = { marker: 'right-object' };
                   exact(leftObject, rightObject, rightObject);
                   const rightArray = [];
                   exact([], rightArray, rightArray);
                   const rightFunction = function rightFunction() {};
                   exact(function leftFunction() {}, rightFunction,
                     rightFunction);
                   exact(false, 0, false);
                   exact(true, false, false);

                   phase = 'no-coercion-or-traps';
                   let hookReads = 0;
                   let proxyTraps = 0;
                   const hostile = {};
                   for (const key of [
                     Symbol.toPrimitive, 'valueOf', 'toString'
                   ]) {
                     Object.defineProperty(hostile, key, {
                       configurable: true,
                       get() {
                         hookReads++;
                         throw new Error('logical and coercion hook read');
                       },
                     });
                   }
                   exact(hostile, selected, selected);
                   exact(false, hostile, false);
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   exact(revoked, selected, selected);
                   exact(false, revoked, false);
                   check(hookReads === 0 && proxyTraps === 0,
                     'logical and coerced or trapped');

                   phase = 'pattern-right-hand-side';
                   let controlQueries = 0;
                   const patternControl = new Pattern(() => {
                     controlQueries++;
                     throw new Error('raw and queried false-branch rhs');
                   });
                   exact(false, patternControl, false);
                   check(controlQueries === 0,
                     'false branch queried Pattern rhs');

                   phase = 'step-matrix';
                   for (const stepSource of [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ]) {
                     const stepResult = stepSource._and('selected');
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
                   tagged.__pure_loc = { start: 11, end: 12 };
                   check(Object.prototype.hasOwnProperty.call(
                     tagged, '__pure'
                   ) && Object.prototype.hasOwnProperty.call(
                     tagged, '__pure_loc'
                   ), 'source tag precondition');
                   const tagResult = tagged._and('selected');
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
                   const falseLiteral = pure(false)._and('selected');
                   const truthyLiteral = pure('source')._and('selected');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const falseHaps = falseLiteral.query(state(0, 1));
                   const truthyHaps = truthyLiteral.query(state(0, 1));
                   check(parserCalls === 0 && falseHaps.length === 1
                     && falseHaps[0].value === false
                     && truthyHaps.length === 1
                     && truthyHaps[0].value === 'selected',
                     'parser reached at query');
                   setStringParser(undefined);

                   phase = 'unsupported-transport';
                   const zeroBigInt = pure(0n)._and('selected-zero-bigint')
                     .query(state(0, 1));
                   check(zeroBigInt.length === 1
                     && zeroBigInt[0].value === 0,
                     'BigInt source number residual changed');
                   const selectedBigInt = 9007199254740993n;
                   const selectedSymbol = Symbol('raw-and-selected');
                   for (const selectedValue of [
                     selectedBigInt, selectedSymbol, rev
                   ]) {
                     const haps = pure(true)._and(selectedValue)
                       .query(state(0, 1));
                     check(haps.length === 1
                       && !Object.is(haps[0].value, selectedValue),
                       'selected unsupported identity residual changed');
                   }
                   check(!Object.hasOwn(globalThis, 'and')
                     && !Object.hasOwn(rustelScope, 'and')
                     && Object.hasOwn(Pattern.prototype, '_or'),
                     'public destination or next raw residual changed');

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure(false);
                   nativeSource.query = pure(true).query;
                   const reassigned = nativeSource._and(selected)
                     .query(state(0, 1));
                   check(reassigned.length === 1
                     && reassigned[0].value === false,
                     'native own-query residual changed');

                   globalThis.rawAndSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawAndSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawAndSemanticError"), None);
    assert_eq!(runtime.get_number("rawAndSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw and lazy truthiness semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const haps = pure(patternValue)._and('selected').query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('Pattern-source residual changed');
                 }
                 globalThis.rawAndPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawAndPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawAndSourceCalls = 0;
                 globalThis.rawAndSourceMapped = 0;
                 const sentinel = { marker: 'source-query-throw' };
                 const source = new Pattern(() => {
                   globalThis.rawAndSourceCalls++;
                   throw sentinel;
                 });
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         globalThis.rawAndSourceMapped++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     }),
                   ]);
                 };
                 const result = source._and(sentinel);
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('source throw produced haps');
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawAndSourceCalls"), Some(1.0));
    assert_eq!(runtime.get_number("rawAndSourceMapped"), Some(0.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "source query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw and source-query throw cutoff");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawAndThrowingMapperCalls = 0;
                 const sentinel = { marker: 'mapper-query-throw' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawAndThrowingMapperCalls++;
                         throw sentinel;
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(false), pure(true))
                   ._and('selected');
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 globalThis.rawAndThrowingMapperHaps = haps.length;
                 globalThis.rawAndThrowingMapperUndefined =
                   haps.every(hap => hap.value === undefined) ? 1 : -1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawAndThrowingMapperCalls"), Some(2.0));
    assert_eq!(runtime.get_number("rawAndThrowingMapperHaps"), Some(2.0));
    assert_eq!(
        runtime.get_number("rawAndThrowingMapperUndefined"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "mapper query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw and direct mapper-query throw residual");

    let mapper_product = semantic_runtime(
        r#"(() => {
             globalThis.rawAndProductMapperCalls = 0;
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               return Reflect.apply(originalFmap, this, [
                 new Proxy(mapper, {
                   apply() {
                     globalThis.rawAndProductMapperCalls++;
                     throw new Error('product mapper query throw');
                   },
                 }),
               ]);
             };
             const result = fastcat(pure(false), pure(true))
               ._and('selected');
             Pattern.prototype.fmap = originalFmap;
             return result;
           })()"#,
    );
    assert!(
        query_active(&mapper_product, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        mapper_product.get_number("rawAndProductMapperCalls"),
        Some(2.0)
    );
    assert_clean(&mapper_product, "raw and outer mapper-query throw");
    mapper_product.clear_active();

    // strudel.cc preserves a falsey 0n source and exact selected BigInt,
    // Symbol, tagged-native callable, and Pattern-left routing. Native keeps
    // the 0n source as the Number 0, materialises the selected unsupported
    // identities, and joins a Pattern-valued source before the outer mapper.
    // A generic throwing fmap mapper runs for both native haps and direct
    // query returns two undefined haps; the outer product empties only after
    // both calls, whereas pinned Node cuts off empty after the first call.
    // Public destinations, own/custom/later query replacement, cross-realm
    // and browser exotica, exact errors/stacks, raw function toString, and
    // complete absolute order remain explicit residuals.
}

#[test]
fn raw_and_retains_operands_prunes_transients_and_keeps_nested_accounting() {
    let selected_branch = semantic_runtime(
        r#"(() => {
             let sourceValue = { marker: 'raw-and-source' };
             let selected = { marker: 'raw-and-selected' };
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-and-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawAndSelectedMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawAndSelectedRef = new WeakRef(selected);
             globalThis.rawAndSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawAndSelectedDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, source, [selected, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             sourceValue = selected = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        selected_branch.active_needs_host(),
        "raw and selected mapper lost its JavaScript query owner"
    );
    assert_eq!(
        selected_branch
            .active_pattern()
            .expect("raw and selected graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        selected_branch.run_gc();
    }
    selected_branch
        .eval(
            r#"globalThis.rawAndSelectedAlive = Number(
                 rawAndSelectedRef.deref()?.marker === 'raw-and-selected'
               );
               globalThis.rawAndSourceValueAlive = Number(
                 rawAndSourceValueRef.deref()?.marker === 'raw-and-source'
               );
               globalThis.rawAndSelectedMapperAlive = Number(
                 typeof rawAndSelectedMapperRef.deref() === 'function'
               );
               globalThis.rawAndSelectedTransientsPruned = Number(
                 rawAndSelectedDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(selected_branch.get_number("rawAndSelectedAlive"), Some(1.0));
    assert_eq!(
        selected_branch.get_number("rawAndSourceValueAlive"),
        Some(1.0)
    );
    assert_eq!(
        selected_branch.get_number("rawAndSelectedMapperAlive"),
        Some(1.0)
    );
    assert_eq!(
        selected_branch.get_number("rawAndSelectedTransientsPruned"),
        Some(1.0)
    );
    let selected_values = query_active(&selected_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(selected_values.len(), 4);
    assert!(
        selected_values
            .iter()
            .all(|hap| hap.value.show() == "marker:raw-and-selected")
    );
    assert_clean(&selected_branch, "raw and selected-branch ownership");
    selected_branch.clear_active();
    drop(selected_branch);

    let false_branch = semantic_runtime(
        r#"(() => {
             let rightQueries = 0;
             let right = new Pattern(() => {
               rightQueries++;
               throw new Error('raw and false branch queried rhs');
             });
             let source = pure(false).fast(4).setSteps(5);
             let extra = { marker: 'raw-and-false-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawAndFalseMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawAndRightQueries = () => rightQueries;
             globalThis.rawAndRightRef = new WeakRef(right);
             globalThis.rawAndFalseDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, source, [right, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             right = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        false_branch.active_needs_host(),
        "raw and false mapper lost its JavaScript query owner"
    );
    assert_eq!(
        false_branch
            .active_pattern()
            .expect("raw and false graph")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        false_branch.run_gc();
    }
    false_branch
        .eval(
            r#"globalThis.rawAndRightAlive = Number(
                 rawAndRightRef.deref() instanceof Pattern
               );
               globalThis.rawAndFalseMapperAlive = Number(
                 typeof rawAndFalseMapperRef.deref() === 'function'
               );
               globalThis.rawAndFalseTransientsPruned = Number(
                 rawAndFalseDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(false_branch.get_number("rawAndRightAlive"), Some(1.0));
    assert_eq!(false_branch.get_number("rawAndFalseMapperAlive"), Some(1.0));
    assert_eq!(
        false_branch.get_number("rawAndFalseTransientsPruned"),
        Some(1.0)
    );
    let false_values = query_active(&false_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(false_values.len(), 4);
    assert!(
        false_values
            .iter()
            .all(|hap| hap.value == Value::Bool(false))
    );
    false_branch
        .eval("globalThis.rawAndRightQueryCount = rawAndRightQueries();")
        .unwrap();
    assert_eq!(false_branch.get_number("rawAndRightQueryCount"), Some(0.0));
    assert_clean(&false_branch, "raw and false-branch ownership");
    false_branch.clear_active();
    drop(false_branch);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-and-js-terminal' };
             let selected = { marker: 'raw-and-js-selected' };
             let target = {};
             let extra = { marker: 'raw-and-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawAndJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawAndJsKeptRef = new WeakRef(kept);
             globalThis.rawAndJsDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, target, [selected, extra]
             );
             kept = selected = target = extra = helper = null;
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
            r#"globalThis.rawAndJsKeptAlive = Number(
                 rawAndJsKeptRef.deref()?.marker === 'raw-and-js-terminal'
               );
               globalThis.rawAndJsMapperPruned = Number(
                 rawAndJsMapperRef.deref() === undefined
               );
               globalThis.rawAndJsTransientsPruned = Number(
                 rawAndJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawAndJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawAndJsMapperPruned"), Some(1.0));
    assert_eq!(
        js_terminal.get_number("rawAndJsTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-and-js-terminal"
    );
    assert_clean(&js_terminal, "raw and JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let selected = { marker: 'raw-and-unused-selected' };
             let target = {};
             let extra = { marker: 'raw-and-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawAndTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-and-terminal');
             };
             target.fmap = helper;
             globalThis.rawAndTerminalDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._and, target, [selected, extra]
             );
             selected = target = extra = helper = null;
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
            r#"globalThis.rawAndTerminalTransientsPruned = Number(
                 rawAndTerminalMapperRef.deref() === undefined
                 && rawAndTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawAndTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-and-terminal"
    );
    assert_clean(&terminal, "raw and custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(false).fast(4).setSteps(5)._and('selected')");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw and charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw and charged query-time stepwise work"
    );
    assert_clean(&direct, "raw and direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._and('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw and suppressed or added nested source work"
    );
    assert_clean(&exact, "raw and nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({}).shrink(0))._and('selected')",
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
    assert_clean(&over, "raw and nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(0).polyBind(() => gap({limit}).shrink(0))._and('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw and nested recovery");
    recovery.clear_active();

    // The direct logical mapper adds no operation or shared-stepwise charge.
    // Exotic fmap/terminal behavior, aggregate ownership lineage, async
    // settlement, arbitrary callback graphs, density and allocation, general
    // resources, exact stacks/toString, and absolute order remain explicit
    // residuals.
}
