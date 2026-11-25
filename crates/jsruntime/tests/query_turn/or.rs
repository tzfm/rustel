use super::*;

#[test]
fn raw_or_is_lazy_truthy_metadata_safe_and_pins_bridge_residuals() {
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
                     pure(truthy), pure(false), pure(NaN)
                   ).setSteps(7);
                   const sourceQuery = source.query;
                   const sourceSteps = source._steps;
                   const result = source._or(selected);
                   Pattern.prototype.fmap = originalFmap;
                   check(mapperCalls === 0, 'logical or was eager');
                   check(result !== source && result.query !== sourceQuery,
                     'result/query identity');
                   check(result._steps !== sourceSteps
                     && result._steps.show() === sourceSteps.show(),
                     'steps were not freshly preserved');
                   const sourceHaps = source.query(state(0, 1));
                   const first = result.query(state(0, 1));
                   const second = result.query(state(0, 1));
                   const routed = haps => haps.length === 3
                     && Object.is(haps[0].value, truthy)
                     && Object.is(haps[1].value, selected)
                     && Object.is(haps[2].value, selected);
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
                     const haps = pure(left)._or(right).query(state(0, 1));
                     check(haps.length === 1
                       && Object.is(haps[0].value, expected),
                       `truthiness matrix ${matrixCase}`);
                   };
                   exact(undefined, selected, selected);
                   exact(null, selected, selected);
                   exact(false, selected, selected);
                   exact(0, selected, selected);
                   exact(-0, selected, selected);
                   exact(NaN, selected, selected);
                   exact('', selected, selected);
                   exact(true, selected, true);
                   exact(1, selected, 1);
                   exact(-1, selected, -1);
                   exact('left', selected, 'left');
                   const leftObject = { marker: 'left-object' };
                   const rightObject = { marker: 'right-object' };
                   exact(leftObject, rightObject, leftObject);
                   const leftArray = [];
                   exact(leftArray, [], leftArray);
                   const leftFunction = function leftFunction() {};
                   exact(leftFunction, function rightFunction() {},
                     leftFunction);
                   exact(false, 0, 0);
                   exact(false, false, false);
                   exact(true, false, true);
                   exact(2, 4, 2);

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
                         throw new Error('logical or coercion hook read');
                       },
                     });
                   }
                   const direct = (left, right) => Reflect.apply(
                     Pattern.prototype._or,
                     { fmap(mapper) { return mapper(left); } },
                     [right]
                   );
                   check(direct(hostile, selected) === hostile
                     && direct(true, hostile) === true,
                     'logical or touched a coercion hook');
                   const revocable = Proxy.revocable({}, {
                     get() { proxyTraps++; throw new Error('proxy get'); },
                     ownKeys() {
                       proxyTraps++;
                       throw new Error('proxy ownKeys');
                     },
                   });
                   const revoked = revocable.proxy;
                   revocable.revoke();
                   check(direct(revoked, selected) === revoked
                     && direct(false, revoked) === revoked,
                     'logical or changed a revoked Proxy operand');
                   check(hookReads === 0 && proxyTraps === 0,
                     'logical or coerced or trapped');

                   phase = 'selected-pattern';
                   let selectedQueries = 0;
                   const selectedPattern = new Pattern(() => {
                     selectedQueries++;
                     throw new Error('raw or eagerly queried selected rhs');
                   });
                   check(direct(false, selectedPattern) === selectedPattern
                     && selectedQueries === 0,
                     'selected Pattern identity or laziness changed');

                   phase = 'step-matrix';
                   for (const stepSource of [
                     pure('default'),
                     pure('none').setSteps(undefined),
                     pure('zero').setSteps(0),
                     pure('seven').setSteps(7),
                   ]) {
                     const stepResult = stepSource._or('selected');
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
                   check(Object.hasOwn(tagged, '__pure')
                     && Object.hasOwn(tagged, '__pure_loc'),
                     'source tag precondition');
                   const tagResult = tagged._or('selected');
                   check(!Object.hasOwn(tagResult, '__pure')
                     && !Object.hasOwn(tagResult, '__pure_loc'),
                     'structural tags survived');

                   phase = 'configured-parser';
                   let parserCalls = 0;
                   setStringParser(value => {
                     parserCalls++;
                     throw new Error(`parser reached:${value}`);
                   });
                   const falseLiteral = pure(false)._or('selected');
                   const truthyLiteral = pure('source')._or('selected');
                   check(parserCalls === 0,
                     'parser reached during construction');
                   const falseHaps = falseLiteral.query(state(0, 1));
                   const truthyHaps = truthyLiteral.query(state(0, 1));
                   check(parserCalls === 0 && falseHaps.length === 1
                     && falseHaps[0].value === 'selected'
                     && truthyHaps.length === 1
                     && truthyHaps[0].value === 'source',
                     'parser reached at query');
                   setStringParser(undefined);

                   phase = 'unsupported-transport';
                   const zeroBigInt = pure(0n)._or('selected-zero-bigint')
                     .query(state(0, 1));
                   check(zeroBigInt.length === 1
                     && zeroBigInt[0].value === 'selected-zero-bigint',
                     'falsey BigInt source was not skipped');
                   const selectedBigInt = 9007199254740993n;
                   const selectedSymbol = Symbol('raw-or-selected');
                   for (const selectedValue of [
                     selectedBigInt, selectedSymbol, rev
                   ]) {
                     const haps = pure(false)._or(selectedValue)
                       .query(state(0, 1));
                     check(haps.length === 1
                       && !Object.is(haps[0].value, selectedValue),
                       'selected unsupported identity residual changed');
                   }
                   check(!Object.hasOwn(globalThis, 'or')
                     && !Object.hasOwn(rustelScope, 'or')
                     && !Object.hasOwn(Pattern.prototype, '_func')
                     && !Object.hasOwn(Pattern.prototype, 'func'),
                     'public destination or next raw residual changed');

                   phase = 'query-reassignment-residual';
                   const nativeSource = pure(true);
                   nativeSource.query = pure(false).query;
                   const reassigned = nativeSource._or(selected)
                     .query(state(0, 1));
                   check(reassigned.length === 1
                     && reassigned[0].value === true,
                     'native own-query residual changed');

                   globalThis.rawOrSemanticOkay = 1;
                 } catch (error) {
                   globalThis.rawOrSemanticError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 } finally {
                   setStringParser(undefined);
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawOrSemanticError"), None);
    assert_eq!(runtime.get_number("rawOrSemanticOkay"), Some(1.0));
    assert_clean(&runtime, "raw or lazy truthiness semantics");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 const patternValue = pure('nested-pattern');
                 const haps = pure(patternValue)._or('selected').query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 if (haps.length !== 0) {
                   throw new Error('Pattern-source residual changed');
                 }
                 globalThis.rawOrPatternResidualOkay = 1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawOrPatternResidualOkay"), Some(1.0));
    assert!(
        rustel_core::take_query_error().is_some_and(
            |error| error.contains("a top-level pattern-of-patterns node is join-only")
        ),
        "Pattern-source residual lost its join-only query error"
    );

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawOrSourceCalls = 0;
                 globalThis.rawOrSourceMapped = 0;
                 const sentinel = { marker: 'source-query-throw' };
                 const source = new Pattern(() => {
                   globalThis.rawOrSourceCalls++;
                   throw sentinel;
                 });
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply(target, thisArg, args) {
                         globalThis.rawOrSourceMapped++;
                         return Reflect.apply(target, thisArg, args);
                       },
                     }),
                   ]);
                 };
                 const result = source._or(sentinel);
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
    assert_eq!(runtime.get_number("rawOrSourceCalls"), Some(1.0));
    assert_eq!(runtime.get_number("rawOrSourceMapped"), Some(0.0));
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "source query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw or source-query throw cutoff");

    runtime
        .evaluate_prelude(
            r#"(() => {
                 globalThis.rawOrThrowingMapperCalls = 0;
                 const sentinel = { marker: 'mapper-query-throw' };
                 const originalFmap = Pattern.prototype.fmap;
                 Pattern.prototype.fmap = function (mapper) {
                   return Reflect.apply(originalFmap, this, [
                     new Proxy(mapper, {
                       apply() {
                         globalThis.rawOrThrowingMapperCalls++;
                         throw sentinel;
                       },
                     }),
                   ]);
                 };
                 const result = fastcat(pure(true), pure(false))
                   ._or('selected');
                 Pattern.prototype.fmap = originalFmap;
                 const haps = result.query({
                   span: { begin: 0, end: 1 }, controls: {},
                 });
                 globalThis.rawOrThrowingMapperHaps = haps.length;
                 globalThis.rawOrThrowingMapperUndefined =
                   haps.every(hap => hap.value === undefined) ? 1 : -1;
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_number("rawOrThrowingMapperCalls"), Some(2.0));
    assert_eq!(runtime.get_number("rawOrThrowingMapperHaps"), Some(2.0));
    assert_eq!(
        runtime.get_number("rawOrThrowingMapperUndefined"),
        Some(1.0)
    );
    assert!(
        rustel_core::take_query_error()
            .is_some_and(|error| error.contains("a thrown object value")),
        "mapper query throw did not retain its query error"
    );
    assert_clean(&runtime, "raw or direct mapper-query throw residual");

    let mapper_product = semantic_runtime(
        r#"(() => {
             globalThis.rawOrProductMapperCalls = 0;
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               return Reflect.apply(originalFmap, this, [
                 new Proxy(mapper, {
                   apply() {
                     globalThis.rawOrProductMapperCalls++;
                     throw new Error('product mapper query throw');
                   },
                 }),
               ]);
             };
             const result = fastcat(pure(true), pure(false))
               ._or('selected');
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
        mapper_product.get_number("rawOrProductMapperCalls"),
        Some(2.0)
    );
    assert_clean(&mapper_product, "raw or outer mapper-query throw");
    mapper_product.clear_active();

    // strudel.cc preserves exact truthy Pattern sources and exact selected
    // Pattern/BigInt/Symbol/tagged-native values. Native joins Pattern-valued
    // sources before the mapper and materialises unsupported selected values.
    // Generic throwing fmap callbacks run twice natively and direct query
    // returns two undefined haps; the product empties only after both calls,
    // whereas pinned Node cuts off empty after the first call. Public
    // destinations, query replacement, cross-realm/browser exotica, exact
    // errors/stacks, raw toString and complete order remain residuals.
}

#[test]
fn raw_or_retains_operands_prunes_transients_and_keeps_nested_accounting() {
    let truthy_branch = semantic_runtime(
        r#"(() => {
             let sourceValue = { marker: 'raw-or-source' };
             let rightQueries = 0;
             let right = new Pattern(() => {
               rightQueries++;
               throw new Error('raw or truthy branch queried rhs');
             });
             let source = pure(sourceValue).fast(4).setSteps(5);
             let extra = { marker: 'raw-or-truthy-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawOrTruthyMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawOrSourceValueRef = new WeakRef(sourceValue);
             globalThis.rawOrRightRef = new WeakRef(right);
             globalThis.rawOrRightQueries = () => rightQueries;
             globalThis.rawOrTruthyDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, source, [right, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             sourceValue = right = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        truthy_branch.active_needs_host(),
        "raw or truthy mapper lost its JavaScript query owner"
    );
    assert_eq!(
        truthy_branch
            .active_pattern()
            .expect("raw or truthy graph")
            .steps(),
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        truthy_branch.run_gc();
    }
    truthy_branch
        .eval(
            r#"globalThis.rawOrSourceValueAlive = Number(
                 rawOrSourceValueRef.deref()?.marker === 'raw-or-source'
               );
               globalThis.rawOrRightAlive = Number(
                 rawOrRightRef.deref() instanceof Pattern
               );
               globalThis.rawOrTruthyMapperAlive = Number(
                 typeof rawOrTruthyMapperRef.deref() === 'function'
               );
               globalThis.rawOrTruthyTransientsPruned = Number(
                 rawOrTruthyDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(truthy_branch.get_number("rawOrSourceValueAlive"), Some(1.0));
    assert_eq!(truthy_branch.get_number("rawOrRightAlive"), Some(1.0));
    assert_eq!(
        truthy_branch.get_number("rawOrTruthyMapperAlive"),
        Some(1.0)
    );
    assert_eq!(
        truthy_branch.get_number("rawOrTruthyTransientsPruned"),
        Some(1.0)
    );
    let truthy_values = query_active(&truthy_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(truthy_values.len(), 4);
    assert!(
        truthy_values
            .iter()
            .all(|hap| hap.value.show() == "marker:raw-or-source")
    );
    truthy_branch
        .eval("globalThis.rawOrRightQueryCount = rawOrRightQueries();")
        .unwrap();
    assert_eq!(truthy_branch.get_number("rawOrRightQueryCount"), Some(0.0));
    assert_clean(&truthy_branch, "raw or truthy-branch ownership");
    truthy_branch.clear_active();
    drop(truthy_branch);

    let selected_branch = semantic_runtime(
        r#"(() => {
             let selected = { marker: 'raw-or-selected' };
             let source = pure(false).fast(4).setSteps(5);
             let extra = { marker: 'raw-or-selected-extra' };
             const originalFmap = Pattern.prototype.fmap;
             Pattern.prototype.fmap = function (mapper) {
               globalThis.rawOrSelectedMapperRef = new WeakRef(mapper);
               return Reflect.apply(originalFmap, this, [mapper]);
             };
             globalThis.rawOrSelectedRef = new WeakRef(selected);
             globalThis.rawOrSelectedDroppedRefs = [
               new WeakRef(source), new WeakRef(extra),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, source, [selected, extra]
             );
             Pattern.prototype.fmap = originalFmap;
             selected = source = extra = null;
             return result;
           })()"#,
    );
    assert!(
        selected_branch.active_needs_host(),
        "raw or selected mapper lost its JavaScript query owner"
    );
    assert_eq!(
        selected_branch
            .active_pattern()
            .expect("raw or selected graph")
            .steps(),
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        selected_branch.run_gc();
    }
    selected_branch
        .eval(
            r#"globalThis.rawOrSelectedAlive = Number(
                 rawOrSelectedRef.deref()?.marker === 'raw-or-selected'
               );
               globalThis.rawOrSelectedMapperAlive = Number(
                 typeof rawOrSelectedMapperRef.deref() === 'function'
               );
               globalThis.rawOrSelectedTransientsPruned = Number(
                 rawOrSelectedDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(selected_branch.get_number("rawOrSelectedAlive"), Some(1.0));
    assert_eq!(
        selected_branch.get_number("rawOrSelectedMapperAlive"),
        Some(1.0)
    );
    assert_eq!(
        selected_branch.get_number("rawOrSelectedTransientsPruned"),
        Some(1.0)
    );
    let selected_values = query_active(&selected_branch, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(selected_values.len(), 4);
    assert!(
        selected_values
            .iter()
            .all(|hap| hap.value.show() == "marker:raw-or-selected")
    );
    assert_clean(&selected_branch, "raw or selected-branch ownership");
    selected_branch.clear_active();
    drop(selected_branch);

    let js_terminal = semantic_runtime(
        r#"(() => {
             let kept = { marker: 'raw-or-js-terminal' };
             let selected = { marker: 'raw-or-js-selected' };
             let target = {};
             let extra = { marker: 'raw-or-js-extra' };
             const makePattern = value => new Pattern(
               state => pure(value).query(state), 3
             );
             let helper = function (mapper) {
               globalThis.rawOrJsMapperRef = new WeakRef(mapper);
               return makePattern(kept);
             };
             target.fmap = helper;
             globalThis.rawOrJsKeptRef = new WeakRef(kept);
             globalThis.rawOrJsDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, target, [selected, extra]
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
            r#"globalThis.rawOrJsKeptAlive = Number(
                 rawOrJsKeptRef.deref()?.marker === 'raw-or-js-terminal'
               );
               globalThis.rawOrJsMapperPruned = Number(
                 rawOrJsMapperRef.deref() === undefined
               );
               globalThis.rawOrJsTransientsPruned = Number(
                 rawOrJsDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(js_terminal.get_number("rawOrJsKeptAlive"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawOrJsMapperPruned"), Some(1.0));
    assert_eq!(js_terminal.get_number("rawOrJsTransientsPruned"), Some(1.0));
    assert_eq!(
        query_active(&js_terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "marker:raw-or-js-terminal"
    );
    assert_clean(&js_terminal, "raw or JavaScript terminal ownership");
    js_terminal.clear_active();

    let terminal = semantic_runtime(
        r#"(() => {
             let selected = { marker: 'raw-or-unused-selected' };
             let target = {};
             let extra = { marker: 'raw-or-terminal-extra' };
             let helper = function (mapper) {
               globalThis.rawOrTerminalMapperRef = new WeakRef(mapper);
               return pure('raw-or-terminal');
             };
             target.fmap = helper;
             globalThis.rawOrTerminalDroppedRefs = [
               new WeakRef(selected), new WeakRef(target),
               new WeakRef(extra), new WeakRef(helper),
             ];
             const result = Reflect.apply(
               Pattern.prototype._or, target, [selected, extra]
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
            r#"globalThis.rawOrTerminalTransientsPruned = Number(
                 rawOrTerminalMapperRef.deref() === undefined
                 && rawOrTerminalDroppedRefs.every(
                   reference => reference.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        terminal.get_number("rawOrTerminalTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&terminal, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-or-terminal"
    );
    assert_clean(&terminal, "raw or custom terminal pruning");
    terminal.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime("pure(true).fast(4).setSteps(5)._or('selected')");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw or charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw or charged query-time stepwise work"
    );
    assert_clean(&direct, "raw or direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let exact = semantic_runtime(&format!(
        "pure(false).polyBind(() => gap({limit}).shrink(0))._or('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw or suppressed or added nested source work"
    );
    assert_clean(&exact, "raw or nested exact accounting");
    exact.clear_active();

    let over = semantic_runtime(&format!(
        "pure(false).polyBind(() => gap({}).shrink(0))._or('selected')",
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
    assert_clean(&over, "raw or nested refusal attribution");
    over.clear_active();

    let recovery = semantic_runtime(&format!(
        "pure(false).polyBind(() => gap({limit}).shrink(0))._or('selected')"
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw or nested recovery");
    recovery.clear_active();

    // The direct logical mapper adds no operation or shared-stepwise charge.
    // Exotic fmap/terminal behavior, aggregate ownership lineage, async
    // settlement, arbitrary callback graphs, density and allocation, general
    // resources, exact stacks/toString and absolute order remain residuals.
}
