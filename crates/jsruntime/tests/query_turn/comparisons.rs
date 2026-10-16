use super::*;

fn comparison_runtime(method: &str) -> JsRuntime {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .eval(&format!(
            "globalThis.comparisonMethod = '{method}';
             globalThis.rawComparisonMethod = '_{method}';
             globalThis.sameResult = {};
             globalThis.differentResult = {};",
            method == "eqt",
            method == "net",
        ))
        .unwrap();
    runtime
}

fn comparison_score(method: &str, source: &str) -> JsRuntime {
    let runtime = comparison_runtime(method);
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{method}: could not install `{source}`: {error}"));
    runtime
}

#[test]
fn raw_comparisons_keep_direct_routing_and_constructors() {
    for method in ["eqt", "net"] {
        let runtime = comparison_runtime(method);
        runtime
            .evaluate_prelude(
                r#"(() => {
                     let phase = 'setup';
                     try {
                       const check = (condition, message) => {
                         if (!condition) throw new Error(message);
                       };
                       const apply = Reflect.apply;
                       const raw = Pattern.prototype[rawComparisonMethod];

                       phase = 'surface';
                       const descriptor = Object.getOwnPropertyDescriptor(
                         Pattern.prototype, rawComparisonMethod
                       );
                       const publicDescriptor = Object.getOwnPropertyDescriptor(
                         Pattern.prototype, comparisonMethod
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
                         'public comparison getter descriptor');
                       const local = comparisonMethod === 'eqt'
                         ? ['eq', '_eqt', 'eqt', 'ne']
                         : ['ne', '_net', 'net', '_and', 'and'];
                       check(Reflect.ownKeys(Pattern.prototype)
                         .filter(name => local.includes(name)).join('|')
                           === local.join('|'),
                         'filtered strict-equality order');
                       check(Object.keys(Pattern.prototype)
                         .filter(name => local.includes(name)).join('|')
                           === (comparisonMethod === 'eqt' ? '_eqt' : '_net|_and'),
                         'enumerable strict-equality order');
                       check(!(rawComparisonMethod in globalThis)
                         && !Object.prototype.hasOwnProperty.call(
                           rustelScope, rawComparisonMethod
                         ), 'raw escaped prototype');

                       phase = 'strict-receivers';
                       for (const receiver of [null, undefined]) {
                         let caught;
                         try { apply(raw, receiver, [receiver]); }
                         catch (error) { caught = error; }
                         check(caught instanceof TypeError,
                           'nullish receiver did not fail strictly');
                       }
                       for (const receiver of [
                         7, 'x', false, 1n, Symbol('raw-comparison-receiver')
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
                           const result = apply(raw, receiver, [source, {}]);
                           check(result.length === 2
                             && result.every(value => value === sameResult),
                             'primitive receiver mapper values');
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
                       let target;
                       const mappers = [];
                       const source = new Proxy({}, {
                         get() { throw new Error('mapper inspected source'); },
                         ownKeys() {
                           throw new Error('mapper enumerated source');
                         },
                       });
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
                       check(apply(raw, target, [source, {}, {}]) === sameResult,
                         'same identity result');
                       check(apply(raw, target, [{}, source]) === differentResult,
                         'distinct identity result');
                       check(gets === 2 && calls === 2
                         && new Set(mappers).size === 2,
                         'dynamic fmap or mapper freshness');
                       check(apply(raw, {
                         fmap(mapper) { return mapper(source); },
                       }, []) === differentResult,
                       'zero-argument comparison result');

                       phase = 'delayed-closures';
                       const delayed = [];
                       const terminal = {};
                       const delayedTarget = {
                         fmap(mapper) { delayed.push(mapper); return terminal; },
                       };
                       check(apply(raw, delayedTarget, [source]) === terminal
                         && apply(raw, delayedTarget, [{}]) === terminal,
                         'delayed terminals');
                       check(delayed.length === 2 && delayed[0] !== delayed[1]
                         && delayed[0](source) === sameResult
                         && delayed[1](source) === differentResult,
                         'delayed mapper capture/freshness');

                       phase = 'throws-and-terminals';
                       const getterSentinel = { marker: 'getter-throw' };
                       let caught;
                       try {
                         apply(raw, Object.defineProperty({}, 'fmap', {
                           get() { throw getterSentinel; },
                         }), [source]);
                       } catch (error) { caught = error; }
                       check(caught === getterSentinel,
                         'fmap getter throw identity');
                       const callSentinel = { marker: 'call-throw' };
                       caught = undefined;
                       try {
                         apply(raw, { fmap() { throw callSentinel; } }, [source]);
                       } catch (error) { caught = error; }
                       check(caught === callSentinel,
                         'fmap call throw identity');
                       const symbol = Symbol('raw-comparison-terminal');
                       const object = {};
                       const callable = function terminal() {};
                       const promise = Promise.resolve('terminal');
                       for (const customTerminal of [
                         undefined, null, false, 0, -0, 1n, 'text', symbol,
                         object, callable, promise
                       ]) {
                         check(Object.is(apply(raw, {
                           fmap() { return customTerminal; },
                         }, [source]), customTerminal),
                         'custom fmap terminal changed');
                       }

                       phase = 'constructors';
                       let defaultError;
                       try { Reflect.construct(raw, [source]); }
                       catch (error) { defaultError = error; }
                       check(defaultError instanceof TypeError,
                         'default constructor found fmap');
                       let constructorThis;
                       let constructorMapped;
                       let constructorTerminal;
                       raw.prototype.fmap = function (mapper) {
                         constructorThis = this;
                         constructorMapped = mapper(source);
                         return constructorTerminal;
                       };
                       constructorTerminal = { marker: 'object-terminal' };
                       const objectResult = Reflect.construct(raw, [source]);
                       check(objectResult === constructorTerminal
                         && constructorThis instanceof raw
                         && constructorMapped === sameResult,
                         'object constructor terminal');
                       constructorTerminal = 7;
                       const primitiveResult = Reflect.construct(raw, [{}]);
                       check(primitiveResult === constructorThis
                         && primitiveResult instanceof raw
                         && constructorMapped === differentResult,
                         'primitive constructor fallback');
                       function Alternate() {}
                       Alternate.prototype.fmap = function (mapper) {
                         check(this instanceof Alternate,
                           'custom newTarget receiver');
                         check(mapper(source) === sameResult,
                           'custom newTarget mapper');
                         return 3;
                       };
                       const alternate = Reflect.construct(
                         raw, [source], Alternate
                       );
                       check(alternate instanceof Alternate,
                         'custom newTarget fallback');

                       phase = 'poisoning';
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
                       check(apply(raw, pure(0), [replacementSource]) === sameResult
                         && replacementHits === 1,
                         'saved raw did not follow replaced fmap');
                       Object.defineProperty(
                         Pattern.prototype, 'fmap', originalFmap
                       );
                       let poisonHits = 0;
                       const surfacePoison = () => {
                         poisonHits++;
                         throw new Error('poisoned comparison surface reached');
                       };
                       Object.defineProperty(Pattern.prototype, rawComparisonMethod, {
                         configurable: true, writable: true,
                         enumerable: true, value: surfacePoison,
                       });
                       Object.defineProperty(Pattern.prototype, comparisonMethod, {
                         configurable: true, writable: true,
                         enumerable: true, value: surfacePoison,
                       });
                       globalThis[comparisonMethod] = surfacePoison;
                       rustelScope[comparisonMethod] = surfacePoison;
                       check(apply(raw, {
                         fmap(mapper) { return mapper(replacementSource); },
                       }, [replacementSource]) === sameResult && poisonHits === 0,
                         'saved raw routed through poisoned comparison surface');

                       globalThis.rawComparisonSurfaceOkay = 1;
                     } catch (error) {
                       globalThis.rawComparisonSurfaceError =
                         `${phase}:${error.name}:${error.message}:${error.stack}`;
                     }
                   })()"#,
                &TranspileOptions::default(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(runtime.get_string("rawComparisonSurfaceError"), None);
        assert_eq!(runtime.get_number("rawComparisonSurfaceOkay"), Some(1.0));
        assert_clean(&runtime, "raw comparison surface and direct routing");
    }
}

#[test]
fn raw_comparisons_are_lazy_and_preserve_metadata() {
    for method in ["eqt", "net"] {
        let runtime = comparison_runtime(method);
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
                       const shared = {};
                       const distinct = {};
                       const source = fastcat(
                         pure(shared), pure(distinct), pure(shared)
                       ).setSteps(7);
                       const sourceQuery = source.query;
                       const sourceSteps = source._steps;
                       const result = source[rawComparisonMethod](shared);
                       Pattern.prototype.fmap = originalFmap;
                       check(mapperCalls === 0, 'comparison was eager');
                       check(result !== source && result.query !== sourceQuery,
                         'result/query identity');
                       check(result._steps !== sourceSteps
                         && result._steps.show() === sourceSteps.show(),
                         'steps were not freshly preserved');
                       const sourceHaps = source.query(state(0, 1));
                       const first = result.query(state(0, 1));
                       const second = result.query(state(0, 1));
                       check(first.length === 3 && second.length === 3
                         && first.map(hap => hap.value).join('|')
                           === [sameResult, differentResult, sameResult].join('|')
                         && second.map(hap => hap.value).join('|')
                           === [sameResult, differentResult, sameResult].join('|')
                         && first.map(timing).join('|')
                           === sourceHaps.map(timing).join('|')
                         && mapperCalls === 6,
                         'lazy repeated strict equality');

                       phase = 'strict-matrix';
                       let matrixCase = 0;
                       const exact = (left, right, expected) => {
                         matrixCase += 1;
                         const haps = pure(left)[rawComparisonMethod](right).query(state(0, 1));
                         check(haps.length === 1 && haps[0].value === expected,
                           `strict matrix ${matrixCase}`);
                       };
                       exact(undefined, undefined, sameResult);
                       exact(null, null, sameResult);
                       exact(null, undefined, differentResult);
                       exact(false, false, sameResult);
                       exact(true, 1, differentResult);
                       exact(7, 7, sameResult);
                       exact(7, '7', differentResult);
                       exact(-0, 0, sameResult);
                       exact(NaN, NaN, differentResult);
                       exact('same', 'same', sameResult);
                       exact('same', 'different', differentResult);
                       const object = {};
                       exact(object, object, sameResult);
                       exact(object, {}, differentResult);
                       const array = [];
                       exact(array, array, sameResult);
                       exact(array, [], differentResult);
                       const callable = function exactFunction() {};
                       exact(callable, callable, sameResult);
                       exact(callable, function distinctFunction() {}, differentResult);
                       exact(rev, rev, sameResult);

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
                             throw new Error('strict equality coercion hook read');
                           },
                         });
                       }
                       exact(hostile, hostile, sameResult);
                       exact(hostile, {}, differentResult);
                       const revocable = Proxy.revocable({}, {
                         get() { proxyTraps++; throw new Error('proxy get'); },
                         ownKeys() {
                           proxyTraps++;
                           throw new Error('proxy ownKeys');
                         },
                       });
                       const revoked = revocable.proxy;
                       revocable.revoke();
                       exact(revoked, revoked, sameResult);
                       check(hookReads === 0 && proxyTraps === 0,
                         'strict equality coerced or trapped');

                       phase = 'pattern-right-hand-side';
                       let controlQueries = 0;
                       const patternControl = new Pattern(() => {
                         controlQueries++;
                         throw new Error('raw comparison queried Pattern rhs');
                       });
                       exact(shared, patternControl, differentResult);
                       check(controlQueries === 0,
                         'Pattern rhs was queried');

                       phase = 'step-matrix';
                       for (const stepSource of [
                         pure('default'),
                         pure('none').setSteps(undefined),
                         pure('zero').setSteps(0),
                         pure('seven').setSteps(7),
                       ]) {
                         const stepResult = stepSource[rawComparisonMethod]('never-equal');
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
                       tagged.__pure_loc = { start: 9, end: 10 };
                       check(Object.prototype.hasOwnProperty.call(
                         tagged, '__pure'
                       ) && Object.prototype.hasOwnProperty.call(
                         tagged, '__pure_loc'
                       ), 'source tag precondition');
                       const tagResult = tagged[rawComparisonMethod]('tagged-source');
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
                       const literal = pure('source')[rawComparisonMethod]('source');
                       check(parserCalls === 0,
                         'parser reached during construction');
                       const literalHaps = literal.query(state(0, 1));
                       check(parserCalls === 0 && literalHaps.length === 1
                         && literalHaps[0].value === sameResult,
                         'parser reached at query');
                       setStringParser(undefined);

                       phase = 'unsupported-identities';
                       const bigint = 1n;
                       const symbol = Symbol('raw-comparison-source');
                       for (const [left, right] of [
                         [bigint, bigint], [symbol, symbol]
                       ]) {
                         const haps = pure(left)[rawComparisonMethod](right).query(state(0, 1));
                         check(haps.length === 1 && haps[0].value === differentResult,
                           'unsupported identity residual changed');
                       }
                       check(!Object.hasOwn(globalThis, comparisonMethod)
                         && !Object.hasOwn(rustelScope, comparisonMethod)
                         && !Object.hasOwn(Pattern.prototype, '_eq')
                         && !Object.hasOwn(Pattern.prototype, '_ne')
                         && Object.hasOwn(Pattern.prototype,
                           comparisonMethod === 'eqt' ? '_net' : '_and'),
                         'public destination or strict raw residual changed');

                       phase = 'query-reassignment-residual';
                       const nativeSource = pure('native-source');
                       nativeSource.query = pure('replacement-source').query;
                       const reassigned = nativeSource[rawComparisonMethod]('native-source')
                         .query(state(0, 1));
                       check(reassigned.length === 1
                         && reassigned[0].value === sameResult,
                         'native own-query residual changed');

                       globalThis.rawComparisonSemanticOkay = 1;
                     } catch (error) {
                       globalThis.rawComparisonSemanticError =
                         `${phase}:${error.name}:${error.message}:${error.stack}`;
                     } finally {
                       setStringParser(undefined);
                     }
                   })()"#,
                &TranspileOptions::default(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(runtime.get_string("rawComparisonSemanticError"), None);
        assert_eq!(runtime.get_number("rawComparisonSemanticOkay"), Some(1.0));
        assert_clean(&runtime, "raw comparison lazy strict semantics");

        runtime
            .evaluate_prelude(
                r#"(() => {
                     const patternValue = pure('nested-pattern');
                     const haps = pure(patternValue)[rawComparisonMethod](patternValue).query({
                       span: { begin: 0, end: 1 }, controls: {},
                     });
                     if (haps.length !== 0) {
                       throw new Error('Pattern-source residual changed');
                     }
                     globalThis.rawComparisonPatternResidualOkay = 1;
                   })()"#,
                &TranspileOptions::default(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            runtime.get_number("rawComparisonPatternResidualOkay"),
            Some(1.0)
        );
        assert!(
            rustel_core::take_query_error().is_some_and(
                |error| error.contains("a top-level pattern-of-patterns node is join-only")
            ),
            "Pattern-source residual lost its join-only query error"
        );

        if method == "net" {
            runtime
                .evaluate_prelude(
                    r#"(() => {
                     const leftPattern = pure('left-pattern');
                     const rightPattern = pure('right-pattern');
                     const haps = pure(leftPattern)[rawComparisonMethod](rightPattern).query({
                       span: { begin: 0, end: 1 }, controls: {},
                     });
                     if (haps.length !== 0) {
                       throw new Error('distinct Pattern-source residual changed');
                     }
                     globalThis.rawComparisonDistinctPatternResidualOkay = 1;
                   })()"#,
                    &TranspileOptions::default(),
                    Duration::from_secs(5),
                )
                .unwrap();
            assert_eq!(
                runtime.get_number("rawComparisonDistinctPatternResidualOkay"),
                Some(1.0)
            );
            assert!(
                rustel_core::take_query_error().is_some_and(
                    |error| error.contains("a top-level pattern-of-patterns node is join-only")
                ),
                "distinct Pattern-source residual lost its join-only query error"
            );
        }

        runtime
            .evaluate_prelude(
                r#"(() => {
                     globalThis.rawComparisonSourceCalls = 0;
                     globalThis.rawComparisonSourceMapped = 0;
                     const sentinel = { marker: 'source-query-throw' };
                     const source = new Pattern(() => {
                       globalThis.rawComparisonSourceCalls++;
                       throw sentinel;
                     });
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [
                         new Proxy(mapper, {
                           apply(target, thisArg, args) {
                             globalThis.rawComparisonSourceMapped++;
                             return Reflect.apply(target, thisArg, args);
                           },
                         }),
                       ]);
                     };
                     const result = source[rawComparisonMethod](sentinel);
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
        assert_eq!(runtime.get_number("rawComparisonSourceCalls"), Some(1.0));
        assert_eq!(runtime.get_number("rawComparisonSourceMapped"), Some(0.0));
        assert!(
            rustel_core::take_query_error()
                .is_some_and(|error| error.contains("a thrown object value")),
            "source query throw did not retain its query error"
        );
        assert_clean(&runtime, "raw comparison source-query throw cutoff");

        runtime
            .evaluate_prelude(
                r#"(() => {
                     globalThis.rawComparisonThrowingMapperCalls = 0;
                     const sentinel = { marker: 'mapper-query-throw' };
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [
                         new Proxy(mapper, {
                           apply() {
                             globalThis.rawComparisonThrowingMapperCalls++;
                             throw sentinel;
                           },
                         }),
                       ]);
                     };
                     const result = fastcat(pure(1), pure(1))[rawComparisonMethod](1);
                     Pattern.prototype.fmap = originalFmap;
                     const haps = result.query({
                       span: { begin: 0, end: 1 }, controls: {},
                     });
                     globalThis.rawComparisonThrowingMapperHaps = haps.length;
                     globalThis.rawComparisonThrowingMapperUndefined =
                       haps.every(hap => hap.value === undefined) ? 1 : -1;
                   })()"#,
                &TranspileOptions::default(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            runtime.get_number("rawComparisonThrowingMapperCalls"),
            Some(2.0)
        );
        assert_eq!(
            runtime.get_number("rawComparisonThrowingMapperHaps"),
            Some(2.0)
        );
        assert_eq!(
            runtime.get_number("rawComparisonThrowingMapperUndefined"),
            Some(1.0)
        );
        assert!(
            rustel_core::take_query_error()
                .is_some_and(|error| error.contains("a thrown object value")),
            "mapper query throw did not retain its query error"
        );
        assert_clean(
            &runtime,
            "raw comparison direct mapper-query throw residual",
        );

        let mapper_product = comparison_score(
            method,
            r#"(() => {
                 globalThis.rawComparisonProductMapperCalls = 0;
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawComparisonProductMapperCalls++;
                         throw new Error('product mapper query throw');
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(1), pure(1))[rawComparisonMethod](1);
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
            mapper_product.get_number("rawComparisonProductMapperCalls"),
            Some(2.0)
        );
        assert_clean(&mapper_product, "raw comparison outer mapper-query throw");
        mapper_product.clear_active();

        // BigInt and Symbol source identity is lost before the mapper. Pattern-valued
        // sources join before the comparison. Throwing mappers produce undefined haps.
    }
}

#[test]
fn raw_comparisons_retain_operands_and_bound_nested_work() {
    for method in ["eqt", "net"] {
        let owned = comparison_score(
            method,
            r#"(() => {
                 let comparisonQueries = 0;
                 let comparison = new Pattern(() => {
                   comparisonQueries++;
                   throw new Error('raw comparison comparison was queried');
                 });
                 let sourceValue = { marker: 'raw-comparison-source' };
                 let source = pure(sourceValue).fast(4).setSteps(5);
                 let extra = { marker: 'raw-comparison-extra' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   globalThis.rawComparisonMapperRef = new WeakRef(mapper);
                   return Reflect.apply(originalFmap, this, [mapper]);
                 };
                 globalThis.rawComparisonComparisonQueries = () => comparisonQueries;
                 globalThis.rawComparisonComparisonRef = new WeakRef(comparison);
                 globalThis.rawComparisonSourceValueRef = new WeakRef(sourceValue);
                 globalThis.rawComparisonDroppedRefs = [
                   new WeakRef(source), new WeakRef(extra),
                 ];
                 const result = Reflect.apply(
                   Pattern.prototype[rawComparisonMethod], source, [comparison, extra]
                 );
                 Pattern.prototype.fmap = originalFmap;
                 comparison = sourceValue = source = extra = null;
                 return result;
               })()"#,
        );
        assert!(
            owned.active_needs_host(),
            "raw comparison mapper lost its JavaScript query owner"
        );
        assert_eq!(
            owned
                .active_pattern()
                .expect("raw comparison owned graph")
                .steps,
            Some(Fraction::int(5))
        );
        for _ in 0..3 {
            owned.run_gc();
        }
        owned
            .eval(
                r#"globalThis.rawComparisonComparisonAlive = Number(
                     rawComparisonComparisonRef.deref() instanceof Pattern
                   );
                   globalThis.rawComparisonSourceValueAlive = Number(
                     rawComparisonSourceValueRef.deref()?.marker === 'raw-comparison-source'
                   );
                   globalThis.rawComparisonMapperAlive = Number(
                     typeof rawComparisonMapperRef.deref() === 'function'
                   );
                   globalThis.rawComparisonTransientsPruned = Number(
                     rawComparisonDroppedRefs.every(
                       reference => reference.deref() === undefined
                     )
                   );"#,
            )
            .unwrap();
        assert_eq!(owned.get_number("rawComparisonComparisonAlive"), Some(1.0));
        assert_eq!(owned.get_number("rawComparisonSourceValueAlive"), Some(1.0));
        assert_eq!(owned.get_number("rawComparisonMapperAlive"), Some(1.0));
        assert_eq!(owned.get_number("rawComparisonTransientsPruned"), Some(1.0));
        let values = query_active(&owned, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(values.len(), 4);
        assert!(
            values
                .iter()
                .all(|hap| hap.value == Value::Bool(method == "net"))
        );
        owned
            .eval(
                "globalThis.rawComparisonComparisonQueryCount = rawComparisonComparisonQueries();",
            )
            .unwrap();
        assert_eq!(
            owned.get_number("rawComparisonComparisonQueryCount"),
            Some(0.0)
        );
        assert_clean(&owned, "raw comparison operand ownership");
        owned.clear_active();
        drop(owned);

        let js_terminal = comparison_score(
            method,
            r#"(() => {
                 let kept = { marker: 'raw-comparison-js-terminal' };
                 let comparison = { marker: 'raw-comparison-js-comparison' };
                 let target = {};
                 let extra = { marker: 'raw-comparison-js-extra' };
                 const makePattern = value => new Pattern(
                   state => pure(value).query(state), 3
                 );
                 let helper = function (mapper) {
                   globalThis.rawComparisonJsMapperRef = new WeakRef(mapper);
                   return makePattern(kept);
                 };
                 target.fmap = helper;
                 globalThis.rawComparisonJsKeptRef = new WeakRef(kept);
                 globalThis.rawComparisonJsDroppedRefs = [
                   new WeakRef(comparison), new WeakRef(target),
                   new WeakRef(extra), new WeakRef(helper),
                 ];
                 const result = Reflect.apply(
                   Pattern.prototype[rawComparisonMethod], target, [comparison, extra]
                 );
                 kept = comparison = target = extra = helper = null;
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
                r#"globalThis.rawComparisonJsKeptAlive = Number(
                     rawComparisonJsKeptRef.deref()?.marker === 'raw-comparison-js-terminal'
                   );
                   globalThis.rawComparisonJsMapperPruned = Number(
                     rawComparisonJsMapperRef.deref() === undefined
                   );
                   globalThis.rawComparisonJsTransientsPruned = Number(
                     rawComparisonJsDroppedRefs.every(
                       reference => reference.deref() === undefined
                     )
                   );"#,
            )
            .unwrap();
        assert_eq!(
            js_terminal.get_number("rawComparisonJsKeptAlive"),
            Some(1.0)
        );
        assert_eq!(
            js_terminal.get_number("rawComparisonJsMapperPruned"),
            Some(1.0)
        );
        assert_eq!(
            js_terminal.get_number("rawComparisonJsTransientsPruned"),
            Some(1.0)
        );
        assert_eq!(
            query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "marker:raw-comparison-js-terminal"
        );
        assert_clean(&js_terminal, "raw comparison JavaScript terminal ownership");
        js_terminal.clear_active();

        let terminal = comparison_score(
            method,
            r#"(() => {
                 let comparison = { marker: 'raw-comparison-unused-comparison' };
                 let target = {};
                 let extra = { marker: 'raw-comparison-terminal-extra' };
                 let helper = function (mapper) {
                   globalThis.rawComparisonTerminalMapperRef = new WeakRef(mapper);
                   return pure('raw-comparison-terminal');
                 };
                 target.fmap = helper;
                 globalThis.rawComparisonTerminalDroppedRefs = [
                   new WeakRef(comparison), new WeakRef(target),
                   new WeakRef(extra), new WeakRef(helper),
                 ];
                 const result = Reflect.apply(
                   Pattern.prototype[rawComparisonMethod], target, [comparison, extra]
                 );
                 comparison = target = extra = helper = null;
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
                r#"globalThis.rawComparisonTerminalTransientsPruned = Number(
                     rawComparisonTerminalMapperRef.deref() === undefined
                     && rawComparisonTerminalDroppedRefs.every(
                       reference => reference.deref() === undefined
                     )
                   );"#,
            )
            .unwrap();
        assert_eq!(
            terminal.get_number("rawComparisonTerminalTransientsPruned"),
            Some(1.0)
        );
        assert_eq!(
            query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "raw-comparison-terminal"
        );
        assert_clean(&terminal, "raw comparison custom terminal pruning");
        terminal.clear_active();

        rustel_core::reset_stepwise_entries_materialised();
        let direct = comparison_score(
            method,
            "pure(1).fast(4).setSteps(5)[rawComparisonMethod](1)",
        );
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "raw comparison charged construction-time stepwise work"
        );
        query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            0,
            "raw comparison charged query-time stepwise work"
        );
        assert_clean(&direct, "raw comparison direct pool neutrality");
        direct.clear_active();

        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        let exact = comparison_score(
            method,
            &format!("pure(0).polyBind(() => gap({limit}).shrink(0))[rawComparisonMethod](0)"),
        );
        rustel_core::reset_stepwise_entries_materialised();
        query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            limit,
            "raw comparison suppressed or added nested source work"
        );
        assert_clean(&exact, "raw comparison nested exact accounting");
        exact.clear_active();

        let over = comparison_score(
            method,
            &format!(
                "pure(0).polyBind(() => gap({}).shrink(0))[rawComparisonMethod](0)",
                limit + 1
            ),
        );
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
        assert_clean(&over, "raw comparison nested refusal attribution");
        over.clear_active();

        let recovery = comparison_score(
            method,
            &format!("pure(0).polyBind(() => gap({limit}).shrink(0))[rawComparisonMethod](0)"),
        );
        rustel_core::reset_stepwise_entries_materialised();
        query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            rustel_core::stepwise_entries_materialised(),
            limit,
            "nested accounting did not recover after refusal"
        );
        assert_clean(&recovery, "raw comparison nested recovery");
        recovery.clear_active();
    }
}
