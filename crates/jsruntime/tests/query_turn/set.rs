use super::*;

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
        owned.active_pattern().expect("raw set owned graph").steps(),
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
