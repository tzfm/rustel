use super::*;

#[test]
fn raw_extend_replicate_keep_surface_and_dynamic_chain_routing() {
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
                   const rawExtend = Pattern.prototype._extend;
                   const rawReplicate = Pattern.prototype._replicate;

                   phase = 'surface';
                   check(Pattern.prototype.s_extend === Pattern.prototype.extend
                     && globalThis.s_extend === globalThis.extend
                     && rustelScope.s_extend === rustelScope.extend,
                     'raw install replaced the copied extend alias');
                   check(rawExtend !== Pattern.prototype.extend
                     && rawReplicate !== Pattern.prototype.replicate
                     && rawExtend !== rawReplicate, 'raw identities');
                   for (const [name, raw] of [
                     ['_extend', rawExtend], ['_replicate', rawReplicate]
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
                     const length = Object.getOwnPropertyDescriptor(raw, 'length');
                     const functionName = Object.getOwnPropertyDescriptor(raw, 'name');
                     const functionPrototype = Object.getOwnPropertyDescriptor(
                       raw, 'prototype'
                     );
                     const constructor = Object.getOwnPropertyDescriptor(
                       raw.prototype, 'constructor'
                     );
                     check(length.value === 0 && !length.writable
                       && !length.enumerable && length.configurable,
                       `${name} length descriptor`);
                     check(functionName.value === '' && !functionName.writable
                       && !functionName.enumerable && functionName.configurable,
                       `${name} name descriptor`);
                     check(functionPrototype.writable
                       && !functionPrototype.enumerable
                       && !functionPrototype.configurable
                       && constructor.value === raw
                       && constructor.writable
                       && !constructor.enumerable
                       && constructor.configurable,
                       `${name} function prototype`);
                     check(Object.getPrototypeOf(raw) === Function.prototype
                       && Object.getPrototypeOf(raw.prototype) === Object.prototype,
                       `${name} prototype chains`);
                     check(Reflect.construct(function () {}, [], raw)
                       instanceof raw, `${name} constructibility`);
                     check(!(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       ), `${name} publication`);
                   }
                   const rawOrder = Object.keys(Pattern.prototype)
                     .filter(name => [
                       'extend', '_extend', 'replicate', '_replicate',
                       's_extend'
                     ].includes(name)).join('|');
                   check(rawOrder
                     === 'extend|_extend|replicate|_replicate|s_extend',
                     `raw extend/replicate order: ${rawOrder}`);
                   check(['_pace', '_s_extend',
                     '_s_replicate', 's_replicate'].every(name =>
                       !(name in globalThis)
                       && !Object.prototype.hasOwnProperty.call(
                         rustelScope, name
                       )
                       && !(name in Pattern.prototype)
                     ), 'adjacent raw or alias surface expanded');

                   phase = 'omission';
                   for (const [name, raw] of [
                     ['_extend', rawExtend], ['_replicate', rawReplicate]
                   ]) {
                     let omitted;
                     try { apply(raw, {}, []); }
                     catch (error) { omitted = error; }
                     check(omitted instanceof TypeError,
                       `${name} omission did not leave pat undefined`);
                     let constructed;
                     try { new raw({ marker: name }); }
                     catch (error) { constructed = error; }
                     check(constructed instanceof TypeError,
                       `${name} constructor unexpectedly used its wrapper target`);
                   }

                   phase = 'extend-chain';
                   const factor = { marker: 'same-factor' };
                   const extendLog = [];
                   const extendFinal = { marker: 'extend-final' };
                   const extendMiddle = {};
                   Object.defineProperty(extendMiddle, 'expand', {
                     configurable: true,
                     get() {
                       extendLog.push('expand.get');
                       return function (received) {
                         extendLog.push(`expand.call:${this === extendMiddle}`
                           + `:${received === factor}`);
                         return extendFinal;
                       };
                     },
                   });
                   const extendTarget = {};
                   Object.defineProperty(extendTarget, 'fast', {
                     configurable: true,
                     get() {
                       extendLog.push('fast.get');
                       return function (received) {
                         extendLog.push(`fast.call:${this === extendTarget}`
                           + `:${received === factor}`);
                         return extendMiddle;
                       };
                     },
                   });
                   const decoy = {};
                   Object.defineProperty(decoy, 'fast', {
                     get() { throw new Error('wrapper receiver reached'); },
                   });
                   check(apply(rawExtend, decoy, [
                     factor, extendTarget, 'ignored'
                   ]) === extendFinal, '_extend replaced the terminal result');
                   check(extendLog.join('|') === [
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_extend chain order: ${extendLog}`);
                   extendLog.length = 0;
                   check(Reflect.construct(
                     rawExtend, [factor, extendTarget]
                   ) === extendFinal, '_extend constructor did not retarget');
                   check(extendLog.join('|') === [
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_extend constructor order: ${extendLog}`);

                   phase = 'replicate-chain';
                   const replicateLog = [];
                   const replicateFinal = { marker: 'replicate-final' };
                   const afterRepeat = {};
                   const afterFast = {};
                   Object.defineProperty(afterFast, 'expand', {
                     configurable: true,
                     get() {
                       replicateLog.push('expand.get');
                       return function (received) {
                         replicateLog.push(`expand.call:${this === afterFast}`
                           + `:${received === factor}`);
                         return replicateFinal;
                       };
                     },
                   });
                   Object.defineProperty(afterRepeat, 'fast', {
                     configurable: true,
                     get() {
                       replicateLog.push('fast.get');
                       return function (received) {
                         replicateLog.push(`fast.call:${this === afterRepeat}`
                           + `:${received === factor}`);
                         return afterFast;
                       };
                     },
                   });
                   const replicateTarget = {};
                   Object.defineProperty(replicateTarget, 'repeatCycles', {
                     configurable: true,
                     get() {
                       replicateLog.push('repeatCycles.get');
                       return function (received) {
                         replicateLog.push(
                           `repeatCycles.call:${this === replicateTarget}`
                           + `:${received === factor}`
                         );
                         return afterRepeat;
                       };
                     },
                   });
                   Object.defineProperty(decoy, 'repeatCycles', {
                     get() { throw new Error('replicate wrapper receiver reached'); },
                   });
                   check(apply(rawReplicate, decoy, [
                     factor, replicateTarget, 'ignored', 'ignored again'
                   ]) === replicateFinal,
                   '_replicate replaced the terminal result');
                   check(replicateLog.join('|') === [
                     'repeatCycles.get', 'repeatCycles.call:true:true',
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_replicate chain order: ${replicateLog}`);
                   replicateLog.length = 0;
                   check(Reflect.construct(
                     rawReplicate, [factor, replicateTarget]
                   ) === replicateFinal,
                   '_replicate constructor did not retarget');
                   check(replicateLog.join('|') === [
                     'repeatCycles.get', 'repeatCycles.call:true:true',
                     'fast.get', 'fast.call:true:true',
                     'expand.get', 'expand.call:true:true'
                   ].join('|'), `_replicate constructor order: ${replicateLog}`);

                   phase = 'primitive-constructor-results';
                   const primitiveExtendTarget = {
                     fast(received) {
                       check(this === primitiveExtendTarget
                         && received === factor,
                         'primitive extend fast receiver or factor');
                       return {
                         expand(next) {
                           check(next === factor,
                             'primitive extend repeated factor');
                           return 17;
                         },
                       };
                     },
                   };
                   check(apply(rawExtend, null, [
                     factor, primitiveExtendTarget
                   ]) === 17, '_extend call replaced a primitive result');
                   const constructedExtend = Reflect.construct(
                     rawExtend, [factor, primitiveExtendTarget]
                   );
                   check(constructedExtend instanceof rawExtend,
                     '_extend constructor returned its primitive body result');

                   const primitiveReplicateTarget = {
                     repeatCycles(received) {
                       check(this === primitiveReplicateTarget
                         && received === factor,
                         'primitive replicate receiver or factor');
                       return {
                         fast(next) {
                           check(next === factor,
                             'primitive replicate fast factor');
                           return {
                             expand(last) {
                               check(last === factor,
                                 'primitive replicate expand factor');
                               return 'replicate-terminal';
                             },
                           };
                         },
                       };
                     },
                   };
                   check(apply(rawReplicate, null, [
                     factor, primitiveReplicateTarget
                   ]) === 'replicate-terminal',
                   '_replicate call replaced a primitive result');
                   const constructedReplicate = Reflect.construct(
                     rawReplicate, [factor, primitiveReplicateTarget]
                   );
                   check(constructedReplicate instanceof rawReplicate,
                     '_replicate constructor returned its primitive body result');

                   phase = 'dynamic-prototype';
                   const base = sequence('a', 'b');
                   const saved = {
                     fast: Pattern.prototype.fast,
                     expand: Pattern.prototype.expand,
                     repeatCycles: Pattern.prototype.repeatCycles,
                   };
                   const dynamicLog = [];
                   const extended = pure('dynamic-extend').setSteps(2);
                   const replicated = pure('dynamic-replicate').setSteps(3);
                   let mode = 'extend';
                   Pattern.prototype.repeatCycles = function (received) {
                     dynamicLog.push(`repeat:${this === base}:${received === factor}`);
                     return this;
                   };
                   Pattern.prototype.fast = function (received) {
                     dynamicLog.push(`fast:${this === base}:${received === factor}`);
                     return this;
                   };
                   Pattern.prototype.expand = function (received) {
                     dynamicLog.push(`expand:${this === base}:${received === factor}`);
                     return mode === 'extend' ? extended : replicated;
                   };
                   let dynamicExtend;
                   let dynamicReplicate;
                   try {
                     dynamicExtend = apply(rawExtend, base, [factor]);
                     mode = 'replicate';
                     dynamicReplicate = apply(rawReplicate, base, [factor]);
                   } finally {
                     Object.assign(Pattern.prototype, saved);
                   }
                   check(dynamicExtend === extended
                     && dynamicReplicate === replicated,
                     'saved raws did not use the current public chain');
                   check(dynamicLog.join('|') === [
                     'fast:true:true', 'expand:true:true',
                     'repeat:true:true', 'fast:true:true', 'expand:true:true'
                   ].join('|'), `dynamic prototype chain: ${dynamicLog}`);

                   phase = 'global-poison';
                   const savedGlobals = {
                     fast: globalThis.fast,
                     expand: globalThis.expand,
                     repeatCycles: globalThis.repeatCycles,
                   };
                   const savedScope = {
                     fast: rustelScope.fast,
                     expand: rustelScope.expand,
                     repeatCycles: rustelScope.repeatCycles,
                   };
                   const poison = () => {
                     throw new Error('global chain decoy reached');
                   };
                   globalThis.fast = poison;
                   globalThis.expand = poison;
                   globalThis.repeatCycles = poison;
                   rustelScope.fast = poison;
                   rustelScope.expand = poison;
                   rustelScope.repeatCycles = poison;
                   let lexicalExtend;
                   let lexicalReplicate;
                   try {
                     lexicalExtend = apply(rawExtend, null, [
                       factor, extendTarget
                     ]);
                     lexicalReplicate = apply(rawReplicate, null, [
                       factor, replicateTarget
                     ]);
                   } finally {
                     Object.assign(globalThis, savedGlobals);
                     Object.assign(rustelScope, savedScope);
                   }
                   check(lexicalExtend === extendFinal
                     && lexicalReplicate === replicateFinal,
                     'raw bodies resolved a global or scope method');
                   globalThis.rawExtendReplicateRoutingOkay = 1;
                 } catch (error) {
                   globalThis.rawExtendReplicateRoutingError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawExtendReplicateRoutingError"), None);
    assert_eq!(
        runtime.get_number("rawExtendReplicateRoutingOkay"),
        Some(1.0)
    );
    assert_clean(&runtime, "raw extend/replicate routing");
}

#[test]
fn raw_extend_replicate_are_eager_and_preserve_returned_ownership() {
    let extend = semantic_runtime("slowcat(pure('a'), pure('b')).setSteps(2)._extend(2)");
    assert!(
        !extend.active_needs_host(),
        "scalar raw extend retained JavaScript"
    );
    assert_eq!(
        extend.active_pattern().expect("active raw extend").steps,
        Some(Fraction::int(4))
    );
    let extend_values = query_active(&extend, Fraction::ZERO, Fraction::int(2))
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(extend_values, ["a", "b", "a", "b"]);
    assert_clean(&extend, "scalar raw extend");
    extend.clear_active();

    let replicate = semantic_runtime("slowcat(pure('a'), pure('b')).setSteps(2)._replicate(2)");
    assert!(
        !replicate.active_needs_host(),
        "scalar raw replicate retained JavaScript"
    );
    assert_eq!(
        replicate
            .active_pattern()
            .expect("active raw replicate")
            .steps,
        Some(Fraction::int(4))
    );
    let replicate_values = query_active(&replicate, Fraction::ZERO, Fraction::int(2))
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert_eq!(replicate_values, ["a", "a", "b", "b"]);
    assert_clean(&replicate, "scalar raw replicate");
    replicate.clear_active();

    for (method, factor, expected_steps, expected_values) in [
        ("_extend", "1/2", Fraction::ONE, vec!["a"]),
        ("_replicate", "1/2", Fraction::ONE, vec!["a"]),
        ("_extend", "0", Fraction::ZERO, Vec::new()),
        ("_replicate", "0", Fraction::ZERO, Vec::new()),
    ] {
        let runtime = semantic_runtime(&format!("sequence('a', 'b').{method}({factor})"));
        assert_eq!(
            runtime
                .active_pattern()
                .expect("active raw numeric case")
                .steps,
            Some(expected_steps),
            "{method}({factor}) step metadata"
        );
        assert!(
            !runtime.active_needs_host(),
            "{method}({factor}) retained JavaScript"
        );
        let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .into_iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>();
        assert_eq!(values, expected_values, "{method}({factor}) values");
        assert_clean(&runtime, &format!("{method}({factor})"));
        runtime.clear_active();
    }

    let eager = semantic_runtime(
        r#"(() => {
             globalThis.rawExtendEagerHits = 0;
             globalThis.rawReplicateEagerHits = 0;
             globalThis.rawExtendReplicateLateHits = 0;
             globalThis.rawExtendReplicateMismatch = 0;
             const factor = { marker: 'raw-factor' };

             const extendSource = sequence('a', 'b');
             const extendMiddle = {};
             extendSource.fast = function (received) {
               globalThis.rawExtendEagerHits++;
               if (this !== extendSource || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return extendMiddle;
             };
             extendMiddle.expand = function (received) {
               globalThis.rawExtendEagerHits++;
               if (this !== extendMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return pure('raw-extend-eager').setSteps(2);
             };
             const extended = extendSource._extend(factor);
             extendSource.fast = extendMiddle.expand = function () {
               globalThis.rawExtendReplicateLateHits++;
               return pure('raw-extend-late');
             };

             const replicateSource = sequence('c', 'd');
             const repeatMiddle = {};
             const fastMiddle = {};
             replicateSource.repeatCycles = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== replicateSource || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return repeatMiddle;
             };
             repeatMiddle.fast = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== repeatMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return fastMiddle;
             };
             fastMiddle.expand = function (received) {
               globalThis.rawReplicateEagerHits++;
               if (this !== fastMiddle || received !== factor) {
                 globalThis.rawExtendReplicateMismatch++;
               }
               return pure('raw-replicate-eager').setSteps(3);
             };
             const replicated = replicateSource._replicate(factor);
             replicateSource.repeatCycles = repeatMiddle.fast
               = fastMiddle.expand = function () {
                 globalThis.rawExtendReplicateLateHits++;
                 return pure('raw-replicate-late');
               };
             return stack(extended, replicated);
           })()"#,
    );
    assert!(
        !eager.active_needs_host(),
        "eager raw chains returning native graphs retained JavaScript"
    );
    assert_eq!(eager.get_number("rawExtendEagerHits"), Some(2.0));
    assert_eq!(eager.get_number("rawReplicateEagerHits"), Some(3.0));
    assert_eq!(eager.get_number("rawExtendReplicateMismatch"), Some(0.0));
    let eager_values = query_active(&eager, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(eager_values.iter().any(|value| value == "raw-extend-eager"));
    assert!(
        eager_values
            .iter()
            .any(|value| value == "raw-replicate-eager")
    );
    assert_eq!(eager.get_number("rawExtendReplicateLateHits"), Some(0.0));
    assert_clean(&eager, "raw extend/replicate eager overrides");
    eager.clear_active();

    let owned = semantic_runtime(
        r#"(() => {
             let extendMarker = { marker: 'raw-extend-owned' };
             let replicateMarker = { marker: 'raw-replicate-owned' };
             globalThis.rawExtendOwnedRef = new WeakRef(extendMarker);
             globalThis.rawReplicateOwnedRef = new WeakRef(replicateMarker);
             let extendSource = sequence('a', 'b');
             let replicateSource = sequence('c', 'd');
             extendSource.fast = () => ({
               expand: () => pure(extendMarker).setSteps(1),
             });
             replicateSource.repeatCycles = () => ({
               fast: () => ({
                 expand: () => pure(replicateMarker).setSteps(1),
               }),
             });
             const result = stack(
               extendSource._extend(2),
               replicateSource._replicate(2)
             );
             extendMarker = null;
             replicateMarker = null;
             extendSource = null;
             replicateSource = null;
             return result;
           })()"#,
    );
    assert!(
        owned.active_needs_host(),
        "raw chain results lost their JavaScript-owned values"
    );
    owned.run_gc();
    owned.run_gc();
    owned.run_gc();
    owned
        .eval(
            r#"globalThis.rawExtendOwnedAlive = Number(
                 rawExtendOwnedRef.deref()?.marker === 'raw-extend-owned'
               );
               globalThis.rawReplicateOwnedAlive = Number(
                 rawReplicateOwnedRef.deref()?.marker === 'raw-replicate-owned'
               );"#,
        )
        .unwrap();
    assert_eq!(owned.get_number("rawExtendOwnedAlive"), Some(1.0));
    assert_eq!(owned.get_number("rawReplicateOwnedAlive"), Some(1.0));
    let owned_values = query_active(&owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-extend-owned"))
    );
    assert!(
        owned_values
            .iter()
            .any(|value| value.contains("raw-replicate-owned"))
    );
    assert_clean(&owned, "raw extend/replicate returned ownership");
    owned.clear_active();
}

#[test]
fn raw_extend_replicate_are_neutral_to_the_shared_stepwise_pool() {
    rustel_core::reset_stepwise_entries_materialised();
    let direct = semantic_runtime(
        r#"stack(
             sequence('a', 'b')._extend(2),
             slowcat(pure('c'), pure('d')).setSteps(2)._replicate(2)
           )"#,
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw extend/replicate charged construction-time stepwise work"
    );
    query_active(&direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw extend/replicate charged query-time stepwise work"
    );
    assert_clean(&direct, "raw extend/replicate pool-neutral direct paths");
    direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    let shared = semantic_runtime(&format!(
        r#"fastcat(pure(0), pure(1), pure(2)).polyBind(value => {{
             if (value === 0) {{
               return sequence('a', 'b')._extend(2);
             }}
             if (value === 1) {{
               return slowcat(pure('c'), pure('d'))
                 .setSteps(2)._replicate(2);
             }}
             return gap({limit}).shrink(0);
           }})"#
    ));
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&shared, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw extend/replicate consumed any shared stepwise allowance"
    );
    assert_clean(&shared, "raw extend/replicate shared-pool neutrality");
    shared.clear_active();
}
