use super::*;

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
            .steps(),
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
