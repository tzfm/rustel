use super::*;

#[test]
fn raw_expand_contract_and_with_steps_keep_surface_and_phase_order() {
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
                   const withSteps = Pattern.prototype.withSteps;
                   const rawExpand = Pattern.prototype._expand;
                   const rawContract = Pattern.prototype._contract;

                   phase = 'withSteps-reflection';
                   const withStepsDescriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, 'withSteps'
                   );
                   check(withStepsDescriptor.value === withSteps
                     && withStepsDescriptor.writable
                     && !withStepsDescriptor.enumerable
                     && withStepsDescriptor.configurable,
                     'withSteps descriptor');
                   check(withSteps.name === 'withSteps'
                     && withSteps.length === 1
                     && !Object.prototype.hasOwnProperty.call(
                       withSteps, 'prototype'
                     ), 'withSteps reflection');
                   let withStepsConstruction;
                   try { new withSteps(() => 1); }
                   catch (error) { withStepsConstruction = error; }
                   check(withStepsConstruction instanceof TypeError,
                     'withSteps became constructible');
                   const elementalOrder = Reflect.ownKeys(Pattern.prototype)
                     .filter(name => [
                       'constructor', '_steps', 'setSteps', 'withSteps',
                       'hasSteps'
                     ].includes(name)).join('|');
                   check(elementalOrder
                     === 'constructor|_steps|setSteps|withSteps|hasSteps',
                     `elemental order: ${elementalOrder}`);

                   phase = 'raw-reflection';
                   check(Pattern.prototype.s_expand === Pattern.prototype.expand
                     && Pattern.prototype.s_contract
                       === Pattern.prototype.contract
                     && globalThis.s_expand === globalThis.expand
                     && globalThis.s_contract === globalThis.contract,
                     'raw install replaced copied aliases');
                   check(rawExpand !== Pattern.prototype.expand
                     && rawContract !== Pattern.prototype.contract
                     && rawExpand !== rawContract, 'raw identities');
                   for (const [name, raw] of [
                     ['_expand', rawExpand], ['_contract', rawContract]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} prototype descriptor`);
                     check(Reflect.ownKeys(raw).join('|')
                       === 'length|name|prototype', `${name} own keys`);
                     check(raw.name === '' && raw.length === 0
                       && Object.prototype.hasOwnProperty.call(
                         raw, 'prototype'
                       ), `${name} reflection`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'expand', '_expand', 'contract', '_contract',
                       's_expand', 's_contract'
                     ].includes(name)).join('|');
                   check(rawOrder
                     === 'expand|_expand|contract|_contract|s_expand|s_contract',
                     `raw expand/contract order: ${rawOrder}`);
                   check(['_s_expand', '_s_contract'].every(name =>
                     !(name in Pattern.prototype)
                     && !(name in globalThis)
                     && !Object.prototype.hasOwnProperty.call(
                       rustelScope, name
                     )), 'raw aliases leaked');

                   phase = 'withSteps-order';
                   const source = pure('source').setSteps(2);
                   const sourceQuery = source.query;
                   const firstSteps = Fraction(2);
                   const secondSteps = Fraction(3);
                   const order = [];
                   let stepReads = 0;
                   Object.defineProperty(source, 'query', {
                     configurable: true,
                     get() {
                       order.push('query');
                       return sourceQuery;
                     },
                   });
                   Object.defineProperty(source, '_steps', {
                     configurable: true,
                     get() {
                       order.push(`steps:${stepReads}`);
                       return stepReads++ === 0 ? firstSteps : secondSteps;
                     },
                   });
                   const result = apply(withSteps, source, [steps => {
                     order.push(`callback:${steps === secondSteps}`);
                     return steps;
                   }]);
                   check(order.join('|')
                     === 'query|steps:0|steps:1|callback:true',
                     `withSteps order: ${order}`);
                   check(result !== source && result.query === sourceQuery,
                     'withSteps identity/query identity');
                   check(result._steps.show() === '3/1',
                     `withSteps final steps: ${result._steps?.show?.()}`);
                   check(!Object.prototype.hasOwnProperty.call(result, '__pure')
                     && !Object.prototype.hasOwnProperty.call(
                       result, '__pure_loc'
                     ), 'withSteps retained register fast-path tags');

                   phase = 'undefined-steps';
                   const noSteps = new Pattern(() => [], undefined);
                   const noStepsQuery = noSteps.query;
                   let callbackHits = 0;
                   const noStepsResult = noSteps.withSteps(() => {
                     callbackHits++;
                     return Symbol('must not convert');
                   });
                   const noStepsContract = noSteps._contract(0);
                   check(noStepsResult !== noSteps
                     && noStepsResult.query === noStepsQuery
                     && noStepsResult._steps === undefined
                     && callbackHits === 0,
                     'undefined steps called or converted its callback');
                   check(noStepsContract !== noSteps
                     && noStepsContract.query === noStepsQuery
                     && noStepsContract._steps === undefined,
                     'no-step contract zero reached division/conversion');
                   globalThis.rawExpandContractSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawExpandContractSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawExpandContractSurfaceError"), None);
    assert_eq!(
        runtime.get_number("rawExpandContractSurfaceOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "raw expand/contract surface");
}

#[test]
fn raw_expand_contract_delay_fraction_work_and_keep_throw_cutoffs() {
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
                   const rawExpand = Pattern.prototype._expand;
                   const rawContract = Pattern.prototype._contract;

                   phase = 'delayed-expand';
                   const log = [];
                   const factor = new Proxy({ n: 2, d: 1 }, {
                     has(target, key) {
                       log.push(`factor.has:${String(key)}`);
                       return Reflect.has(target, key);
                     },
                     get(target, key, receiver) {
                       log.push(`factor.get:${String(key)}`);
                       return Reflect.get(target, key, receiver);
                     },
                   });
                   let delayed;
                   const terminal = { marker: 'expand-terminal' };
                   const target = {};
                   Object.defineProperty(target, 'withSteps', {
                     configurable: true,
                     get() {
                       log.push('withSteps.get');
                       return function (callback) {
                         log.push(`withSteps.call:${this === target}`);
                         delayed = callback;
                         return terminal;
                       };
                     },
                   });
                   const decoy = {};
                   Object.defineProperty(decoy, 'withSteps', {
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   check(apply(rawExpand, decoy, [
                     factor, target, 'ignored'
                   ]) === terminal, '_expand replaced override result');
                   check(log.join('|') === 'withSteps.get|withSteps.call:true',
                     `factor converted before callback: ${log}`);

                   const multiplied = { marker: 'multiplied' };
                   const steps = {};
                   Object.defineProperty(steps, 'mul', {
                     configurable: true,
                     get() {
                       log.push('mul.get');
                       return function (received) {
                         log.push(`mul.call:${this === steps}`
                           + `:${received.show()}`);
                         return multiplied;
                       };
                     },
                   });
                   check(delayed(steps) === multiplied,
                     'delayed expand callback result');
                   const mulGet = log.indexOf('mul.get');
                   const factorRead = log.findIndex(entry =>
                     entry.startsWith('factor.')
                   );
                   const mulCall = log.findIndex(entry =>
                     entry.startsWith('mul.call:')
                   );
                   check(mulGet >= 0 && factorRead > mulGet
                     && mulCall > factorRead
                     && log[mulCall] === 'mul.call:true:2/1',
                     `expand callback order: ${log}`);

                   phase = 'retarget-and-constructor';
                   const receiverTarget = {
                     withSteps(callback) {
                       check(this === receiverTarget,
                         'one-argument receiver changed');
                       return callback(Fraction(3));
                     },
                   };
                   check(apply(rawExpand, receiverTarget, [2]).show()
                     === '6/1', 'one explicit argument did not use receiver');
                   let omitted;
                   try { apply(rawExpand, receiverTarget, []); }
                   catch (error) { omitted = error; }
                   check(omitted instanceof TypeError,
                     'omission did not leave pat undefined');

                   const constructorObject = { marker: 'constructor-object' };
                   const constructorTarget = {
                     withSteps(callback) {
                       check(this === constructorTarget
                         && typeof callback === 'function',
                         'constructor target/callback');
                       return constructorObject;
                     },
                   };
                   check(Reflect.construct(
                     rawExpand, [factor, constructorTarget]
                   ) === constructorObject,
                   'constructor did not return object body result');
                   const primitiveTarget = { withSteps() { return 7; } };
                   const primitiveConstruction = Reflect.construct(
                     rawContract, [2, primitiveTarget]
                   );
                   check(primitiveConstruction instanceof rawContract,
                     'constructor returned primitive body result');

                   phase = 'withSteps-throw-cutoff';
                   let getterFactorHits = 0;
                   const getterFactor = new Proxy({ n: 2, d: 1 }, {
                     has(target, key) {
                       getterFactorHits++;
                       return Reflect.has(target, key);
                     },
                     get(target, key, receiver) {
                       getterFactorHits++;
                       return Reflect.get(target, key, receiver);
                     },
                   });
                   const getterStop = {};
                   Object.defineProperty(getterStop, 'withSteps', {
                     get() { throw new Error('withSteps-stop'); },
                   });
                   let getterError;
                   try { apply(rawExpand, null, [getterFactor, getterStop]); }
                   catch (error) { getterError = error; }
                   check(getterError?.message === 'withSteps-stop'
                     && getterFactorHits === 0,
                     'withSteps getter throw did not cut off conversion');

                   phase = 'div-throw-cutoff';
                   let divFactorHits = 0;
                   const divFactor = new Proxy({ n: 2, d: 1 }, {
                     has(target, key) {
                       divFactorHits++;
                       return Reflect.has(target, key);
                     },
                     get(target, key, receiver) {
                       divFactorHits++;
                       return Reflect.get(target, key, receiver);
                     },
                   });
                   const divSteps = {};
                   Object.defineProperty(divSteps, 'div', {
                     get() { throw new Error('div-stop'); },
                   });
                   let divError;
                   try {
                     apply(rawContract, null, [divFactor, {
                       withSteps(callback) { return callback(divSteps); },
                     }]);
                   } catch (error) { divError = error; }
                   check(divError?.message === 'div-stop'
                     && divFactorHits === 0,
                     'div getter throw did not cut off conversion');

                   phase = 'conversion-throw-cutoff';
                   let mulGets = 0;
                   let mulCalls = 0;
                   const conversionSteps = {};
                   Object.defineProperty(conversionSteps, 'mul', {
                     get() {
                       mulGets++;
                       return function () { mulCalls++; };
                     },
                   });
                   let conversionError;
                   try {
                     apply(rawExpand, null, [Symbol('bad-factor'), {
                       withSteps(callback) {
                         return callback(conversionSteps);
                       },
                     }]);
                   } catch (error) { conversionError = error; }
                   check(conversionError instanceof Error
                     && mulGets === 1 && mulCalls === 0,
                     'factor throw did not follow mul getter');
                   globalThis.rawExpandContractRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawExpandContractRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawExpandContractRoutingError"), None);
    assert_eq!(
        runtime.get_number("rawExpandContractRoutingOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "raw expand/contract routing");
}

#[test]
fn with_steps_tag_loss_forces_public_step_register_routing() {
    for method in ["expand", "contract"] {
        let runtime = semantic_runtime(&format!(
            r#"(() => {{
                 globalThis.withStepsPatternedHits = 0;
                 globalThis.withStepsDirectHits = 0;

                 const patternedSource = pure(2);
                 const patternedQuery = patternedSource.query;
                 patternedSource.query = state => {{
                   globalThis.withStepsPatternedHits++;
                   return Reflect.apply(
                     patternedQuery, patternedSource, [state]
                   );
                 }};
                 const patternedFactor = patternedSource.withSteps(
                   steps => steps
                 );

                 const directFactor = pure(2);
                 const directQuery = directFactor.query;
                 directFactor.query = state => {{
                   globalThis.withStepsDirectHits++;
                   return Reflect.apply(directQuery, directFactor, [state]);
                 }};

                 const patterned = pure('patterned').{method}(
                   patternedFactor
                 );
                 const direct = pure('direct').{method}(directFactor);
                 globalThis.withStepsPatternedConstructionHits =
                   globalThis.withStepsPatternedHits;
                 globalThis.withStepsDirectConstructionHits =
                   globalThis.withStepsDirectHits;
                 return stack(patterned, direct);
               }})()"#
        ));
        assert_eq!(
            runtime.get_number("withStepsPatternedConstructionHits"),
            Some(1.0),
            "public {method} did not query the stripped factor during stepJoin"
        );
        assert_eq!(
            runtime.get_number("withStepsDirectConstructionHits"),
            Some(0.0),
            "public {method} queried a direct pure factor during construction"
        );
        query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            runtime.get_number("withStepsPatternedHits"),
            Some(2.0),
            "public {method} did not keep the patterned factor route at query"
        );
        assert_eq!(
            runtime.get_number("withStepsDirectHits"),
            Some(0.0),
            "public {method} queried a direct pure factor"
        );
        assert_clean(&runtime, &format!("withSteps public {method} routing"));
        runtime.clear_active();
    }
}

#[test]
fn with_steps_tracks_native_query_identity_across_reassignment_and_repetition() {
    let runtime = semantic_runtime(
        r#"(() => {
             const a = pure('a');
             const b = pure('b');
             const copiedQuery = b.query;
             a.query = copiedQuery;

             const first = a.withSteps(steps => steps);
             const second = first.withSteps(steps => steps.mul(2));
             globalThis.withStepsCopiedFirst = Number(
               first !== a
               && first.query === copiedQuery
               && first._steps.show() === '1/1'
               && !Object.prototype.hasOwnProperty.call(first, '__pure')
             );
             globalThis.withStepsCopiedSecond = Number(
               second !== first
               && second.query === copiedQuery
               && second._steps.show() === '2/1'
               && !Object.prototype.hasOwnProperty.call(second, '__pure')
             );
             return second;
           })()"#,
    );
    assert_eq!(runtime.get_number("withStepsCopiedFirst"), Some(1.0));
    assert_eq!(runtime.get_number("withStepsCopiedSecond"), Some(1.0));
    assert!(
        !runtime.active_needs_host(),
        "native query reassignment or repeated withSteps fell into JS dispatch"
    );
    let pattern = runtime.active_pattern().expect("copied-query result");
    assert_eq!(pattern.steps, Some(Fraction::int(2)));
    assert!(pattern.is_pure(), "copied native query lost native purity");
    assert!(
        pattern.as_pure().is_none(),
        "repeated withSteps restored structural pure fast-path metadata"
    );
    assert_eq!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "b",
        "withSteps cloned the reassignment target instead of copied query"
    );
    assert_clean(&runtime, "withSteps native query reassignment");
    runtime.clear_active();
}

#[test]
fn raw_expand_contract_keep_timing_purity_ownership_and_pool_neutrality() {
    let with_steps = semantic_runtime("pure('with-steps').withSteps(t => t.mul(2))");
    let with_steps_pattern = with_steps
        .active_pattern()
        .expect("active withSteps result");
    assert_eq!(with_steps_pattern.steps, Some(Fraction::int(2)));
    assert!(
        with_steps_pattern.is_pure(),
        "metadata-only withSteps lost native purity"
    );
    assert!(
        with_steps_pattern.as_pure().is_none(),
        "withSteps retained the structural register fast path"
    );
    assert!(
        !with_steps.active_needs_host(),
        "native withSteps retained an eager callback"
    );
    assert_eq!(
        query_active(&with_steps, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "with-steps"
    );
    assert_clean(&with_steps, "host-free withSteps");
    with_steps.clear_active();

    for (expression, expected_steps) in [
        ("sequence('a', 'b')._expand(2)", Some(Fraction::int(4))),
        ("sequence('a', 'b')._expand(.5)", Some(Fraction::ONE)),
        ("sequence('a', 'b')._expand(-1)", Some(Fraction::int(-2))),
        ("sequence('a', 'b')._expand(0)", Some(Fraction::ZERO)),
        ("sequence('a', 'b')._contract(2)", Some(Fraction::ONE)),
        ("sequence('a', 'b')._contract(.5)", Some(Fraction::int(4))),
        ("sequence('a', 'b')._contract(-1)", Some(Fraction::int(-2))),
    ] {
        let runtime = semantic_runtime(expression);
        assert_eq!(
            runtime
                .active_pattern()
                .expect("active scalar result")
                .steps,
            expected_steps,
            "{expression}: steps"
        );
        assert!(
            !runtime.active_needs_host(),
            "{expression}: scalar metadata path retained JavaScript"
        );
        let haps = query_active(&runtime, Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(
            haps.iter()
                .map(|hap| (
                    hap.value.show(),
                    hap.part.begin,
                    hap.part.end,
                    hap.whole.expect("sequence hap whole").begin,
                    hap.whole.expect("sequence hap whole").end,
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "a".to_string(),
                    Fraction::ZERO,
                    Fraction::new(1, 2),
                    Fraction::ZERO,
                    Fraction::new(1, 2),
                ),
                (
                    "b".to_string(),
                    Fraction::new(1, 2),
                    Fraction::ONE,
                    Fraction::new(1, 2),
                    Fraction::ONE,
                ),
            ],
            "{expression}: changing steps changed query timing"
        );
        assert_clean(&runtime, expression);
        runtime.clear_active();
    }

    let zero_contract = JsRuntime::new().unwrap();
    zero_contract.install_semantic_bindings().unwrap();
    let zero_error = zero_contract
        .evaluate_score(
            "sequence('a', 'b')._contract(0)",
            &TranspileOptions::default(),
        )
        .expect_err("raw contract zero must throw during construction");
    assert!(
        zero_error.contains("Division by Zero"),
        "unexpected raw contract zero error: {zero_error}"
    );
    assert_clean(&zero_contract, "raw contract zero");

    let lexical = semantic_runtime(
        r#"(() => {
             const left = sequence('a', 'b');
             const right = sequence('c', 'd');
             globalThis.rawExpandContractParserHits = 0;
             setStringParser(value => {
               globalThis.rawExpandContractParserHits++;
               return pure(value);
             });
             const savedGlobal = globalThis.Fraction;
             const savedScope = rustelScope.Fraction;
             const poison = () => {
               throw new Error('mutable Fraction surface reached');
             };
             globalThis.Fraction = poison;
             rustelScope.Fraction = poison;
             try {
               const expanded = left._expand('2');
               const contracted = right._contract('2');
               globalThis.rawExpandStringSteps = Number(expanded._steps);
               globalThis.rawContractStringSteps = Number(contracted._steps);
               return stack(expanded, contracted);
             } finally {
               globalThis.Fraction = savedGlobal;
               rustelScope.Fraction = savedScope;
             }
           })()"#,
    );
    assert_eq!(lexical.get_number("rawExpandContractParserHits"), Some(0.0));
    assert_eq!(lexical.get_number("rawExpandStringSteps"), Some(4.0));
    assert_eq!(lexical.get_number("rawContractStringSteps"), Some(1.0));
    assert!(
        !lexical.active_needs_host(),
        "lexical string factors retained parser JavaScript"
    );
    assert_clean(&lexical, "raw expand/contract lexical strings");
    lexical.clear_active();

    let no_steps =
        semantic_runtime("new Pattern(() => [], undefined)._expand(Symbol('unused-factor'))");
    assert_eq!(
        no_steps.active_pattern().expect("no-step raw expand").steps,
        None
    );
    assert!(
        query_active(&no_steps, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty(),
        "no-step custom query changed"
    );
    assert_clean(&no_steps, "raw expand undefined-step cutoff");
    no_steps.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let pureMarker = { marker: 'raw-expand-owned' };
             let customMarker = { marker: 'raw-contract-owned' };
             globalThis.rawExpandOwnedRef = new WeakRef(pureMarker);
             globalThis.rawContractOwnedRef = new WeakRef(customMarker);
             let pureSource = pure(pureMarker).setSteps(2);
             const customCallback = (ownedMarker => state => [{
               begin: state.span.begin,
               end: state.span.end,
               value: ownedMarker,
             }])(customMarker);
             let customSource = new Pattern(customCallback, 2);
             const pureQuery = pureSource.query;
             const customQuery = customSource.query;
             const expanded = pureSource._expand(2);
             const contracted = customSource._contract(2);
             globalThis.rawExpandOwnedQueryIdentity = Number(
               expanded.query === pureQuery
             );
             globalThis.rawContractOwnedQueryIdentity = Number(
               contracted.query === customQuery
             );
             pureMarker = null;
             customMarker = null;
             pureSource = null;
             customSource = null;
             return stack(expanded, contracted);
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "owned results lost JS dependencies"
    );
    for _ in 0..3 {
        owned.run_gc();
    }
    owned
        .eval(
            r#"globalThis.rawExpandOwnedAlive = Number(
                 rawExpandOwnedRef.deref()?.marker === 'raw-expand-owned'
               );
               globalThis.rawContractOwnedAlive = Number(
                 rawContractOwnedRef.deref()?.marker === 'raw-contract-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawExpandOwnedQueryIdentity"), Some(1.0));
    assert_eq!(owned.get_number("rawContractOwnedQueryIdentity"), Some(1.0));
    assert_eq!(owned.get_number("rawExpandOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawContractOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-expand-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-contract-owned"))
    );
    assert_clean(&owned, "raw expand/contract ownership");
    owned.clear_active();

    let pruned = semantic_runtime(
        r#"(() => {
             let dropped = { marker: 'raw-expand-dropped' };
             globalThis.rawExpandDroppedRef = new WeakRef(dropped);
             pure(dropped);
             dropped = null;
             return sequence('a', 'b')._expand(2);
           })()"#,
    );
    for _ in 0..3 {
        pruned.run_gc();
    }
    pruned
        .eval(
            "globalThis.rawExpandDroppedCollected = Number(\
               rawExpandDroppedRef.deref() === undefined);",
        )
        .unwrap();
    assert_eq!(pruned.get_number("rawExpandDroppedCollected"), Some(1.0));
    assert_clean(&pruned, "raw expand unrelated-owner pruning");
    pruned.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b')._expand(2),
             sequence('c', 'd')._contract(2)
           )"#,
    );
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw expand/contract charged stepwise work"
    );
    assert_clean(&direct, "raw expand/contract direct pool neutrality");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) return sequence('a', 'b')._expand(2);
             if (value === 1) return sequence('c', 'd')._contract(2);
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw expand/contract consumed shared stepwise allowance"
    );
    assert_clean(&shared, "raw expand/contract shared-pool neutrality");
    shared.clear_active();
}
