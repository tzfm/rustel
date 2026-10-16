use super::*;

#[test]
fn raw_never_always_keep_surface_order_strict_forwarding_and_terminals() {
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
                   const rawNever = Pattern.prototype._never;
                   const rawAlways = Pattern.prototype._always;

                   phase = 'surface';
                   check(typeof rawNever === 'function'
                     && typeof rawAlways === 'function'
                     && rawNever !== Pattern.prototype.never
                     && rawAlways !== Pattern.prototype.always
                     && rawNever !== rawAlways
                     && rawAlways !== Pattern.prototype._apply,
                     'raw identities');
                   for (const [name, raw] of [
                     ['_never', rawNever], ['_always', rawAlways]
                   ]) {
                     const descriptor = Object.getOwnPropertyDescriptor(
                       Pattern.prototype, name
                     );
                     check(descriptor.value === raw
                       && descriptor.writable
                       && descriptor.enumerable
                       && descriptor.configurable,
                       `${name} descriptor`);
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
                   const quartet = [
                     'never', '_never', 'always', '_always'
                   ];
                   check(Reflect.ownKeys(Pattern.prototype)
                     .filter(name => quartet.includes(name)).join('|')
                     === quartet.join('|'), 'own-key quartet order');
                   check(Object.keys(Pattern.prototype)
                     .filter(name => quartet.includes(name)).join('|')
                     === quartet.join('|'), 'enumerable quartet order');

                   phase = 'never-forwarding';
                   const ignored = Object.create(null);
                   Object.defineProperties(ignored, {
                     valueOf: { get() {
                       throw new Error('never read ignored valueOf');
                     } },
                     [Symbol.toPrimitive]: { get() {
                       throw new Error('never read ignored primitive hook');
                     } },
                   });
                   const receivers = [
                     null, undefined, 7, 'x', 1n, Symbol('receiver')
                   ];
                   for (const receiver of receivers) {
                     check(apply(rawNever, receiver, []) === undefined,
                       'never zero-argument shift');
                     check(Object.is(
                       apply(rawNever, receiver, [ignored]), receiver
                     ), 'never strict receiver forwarding');
                   }
                   const neverTarget = { marker: 'never-target' };
                   const neverOuter = { marker: 'never-outer' };
                   const neverExtra = { marker: 'never-extra' };
                   check(apply(rawNever, neverOuter, [
                     ignored, neverTarget
                   ]) === neverTarget, 'never explicit target');
                   check(apply(rawNever, neverOuter, [
                     ignored, neverTarget, neverExtra, 'ignored'
                   ]) === neverTarget, 'never ignored extras');
                   let ignoredCallbackHits = 0;
                   const ignoredCallback = new Proxy(function () {
                     ignoredCallbackHits++;
                   }, {
                     apply() {
                       ignoredCallbackHits++;
                       throw new Error('never callback reached');
                     },
                   });
                   check(apply(rawNever, neverOuter, [
                     ignoredCallback, neverTarget
                   ]) === neverTarget && ignoredCallbackHits === 0,
                   'never invoked ignored callback');

                   phase = 'always-forwarding';
                   let proxyHits = 0;
                   for (const receiver of receivers) {
                     const callback = new Proxy(function () {
                       throw new Error('always proxy body reached');
                     }, {
                       apply(_target, thisArg, args) {
                         proxyHits++;
                         check(thisArg === undefined,
                           'always callback thisArg');
                         check(args.length === 1
                           && Object.is(args[0], receiver),
                           'always strict receiver forwarding');
                         return args[0];
                       },
                     });
                     check(Object.is(
                       apply(rawAlways, receiver, [callback]), receiver
                     ), 'always one-argument terminal');
                   }
                   check(proxyHits === receivers.length,
                     'always Proxy apply count');
                   const zeroTerminal = {};
                   let zeroHits = 0;
                   function zeroCallback(received) {
                     'use strict';
                     zeroHits++;
                     check(this === undefined && new.target === undefined
                       && received === undefined,
                       'always zero-argument forwarding');
                     return zeroTerminal;
                   }
                   check(apply(rawAlways, zeroCallback, []) === zeroTerminal
                     && zeroHits === 1, 'always zero-argument terminal');
                   const explicitTarget = { marker: 'always-target' };
                   const outer = { marker: 'always-outer' };
                   const explicitTerminal = {};
                   let explicitHits = 0;
                   function explicitCallback(received) {
                     'use strict';
                     explicitHits++;
                     check(this === undefined && new.target === undefined
                       && received === explicitTarget,
                       'always explicit target forwarding');
                     return explicitTerminal;
                   }
                   check(apply(rawAlways, outer, [
                     explicitCallback, explicitTarget
                   ]) === explicitTerminal, 'always two arguments');
                   check(apply(rawAlways, outer, [
                     explicitCallback, explicitTarget, {}, 'ignored'
                   ]) === explicitTerminal, 'always ignored extras');
                   check(explicitHits === 2,
                     'always callback was skipped or repeated');

                   phase = 'non-touch-and-terminals';
                   let touchHits = 0;
                   const opaqueTarget = {};
                   for (const name of [
                     'query', '_steps', '__pure', '__pure_loc'
                   ]) {
                     Object.defineProperty(opaqueTarget, name, {
                       configurable: true,
                       get() {
                         touchHits++;
                         throw new Error(`raw read target ${name}`);
                       },
                     });
                   }
                   const parserCalls = [];
                   setStringParser(value => {
                     parserCalls.push(value);
                     return pure(value);
                   });
                   check(apply(rawNever, null, [ignored, opaqueTarget])
                     === opaqueTarget, 'never opaque target identity');
                   check(apply(rawAlways, null, [
                     value => value, opaqueTarget
                   ]) === opaqueTarget, 'always opaque target identity');
                   check(apply(rawNever, null, [ignored, 'never text'])
                     === 'never text', 'never raw string target');
                   check(apply(rawAlways, null, [
                     value => value, 'always text'
                   ]) === 'always text', 'always raw string target');
                   setStringParser(undefined);
                   check(touchHits === 0 && parserCalls.length === 0,
                     'raw pair touched query/steps/parser surfaces');

                   const symbol = Symbol('always-terminal');
                   const object = { marker: 'object-terminal' };
                   const callable = function terminal() {};
                   const promise = Promise.resolve('promise-terminal');
                   for (const terminal of [
                     undefined, null, false, 0, -0, 1n, 'text', symbol,
                     object, callable, promise
                   ]) {
                     check(Object.is(apply(rawAlways, outer, [
                       () => terminal, explicitTarget
                     ]), terminal), 'always transformed terminal');
                   }
                   const sentinel = { marker: 'always-throw' };
                   let caught;
                   try {
                     apply(rawAlways, outer, [
                       () => { throw sentinel; }, explicitTarget
                     ]);
                   } catch (error) {
                     caught = error;
                   }
                   check(caught === sentinel, 'always translated throw');
                   let nonCallable;
                   try { apply(rawAlways, null, [7, explicitTarget]); }
                   catch (error) { nonCallable = error.name; }
                   check(nonCallable === 'TypeError',
                     'always non-callable did not throw TypeError');

                   phase = 'constructors';
                   const neverZero = Reflect.construct(rawNever, []);
                   const neverOne = Reflect.construct(rawNever, [ignored]);
                   check(neverZero instanceof rawNever
                     && neverOne instanceof rawNever,
                     'never zero/one constructor fallback');
                   const neverObject = { marker: 'never-object' };
                   check(Reflect.construct(rawNever, [
                     ignored, neverObject
                   ]) === neverObject, 'never object constructor return');
                   const neverPrimitive = Reflect.construct(rawNever, [
                     ignored, 7
                   ]);
                   check(neverPrimitive instanceof rawNever,
                     'never primitive constructor fallback');
                   let alwaysZero = 'no-error';
                   try { Reflect.construct(rawAlways, []); }
                   catch (error) { alwaysZero = error.name; }
                   check(alwaysZero === 'TypeError',
                     'always zero constructor did not call instance');
                   const alwaysObject = { marker: 'always-object' };
                   let constructedPat;
                   const alwaysObjectResult = Reflect.construct(rawAlways, [
                     function (pat) {
                       'use strict';
                       check(this === undefined && new.target === undefined,
                         'always constructor callback invocation');
                       constructedPat = pat;
                       return alwaysObject;
                     },
                   ]);
                   check(alwaysObjectResult === alwaysObject
                     && constructedPat instanceof rawAlways,
                     'always object constructor return');
                   let primitivePat;
                   const alwaysPrimitive = Reflect.construct(rawAlways, [
                     function (pat) {
                       primitivePat = pat;
                       return 7;
                     },
                   ]);
                   check(alwaysPrimitive === primitivePat
                     && alwaysPrimitive instanceof rawAlways,
                     'always primitive constructor fallback');
                   const redirectedObject = {
                     marker: 'always-redirected-object'
                   };
                   let redirectedPat;
                   check(Reflect.construct(rawAlways, [
                     function (pat) {
                       redirectedPat = pat;
                       return redirectedObject;
                     },
                     explicitTarget,
                   ]) === redirectedObject
                     && redirectedPat === explicitTarget,
                     'always redirected object constructor return');
                   let primitiveRedirectedPat;
                   const primitiveRedirected = Reflect.construct(rawAlways, [
                     function (pat) {
                       primitiveRedirectedPat = pat;
                       return 7;
                     },
                     9,
                   ]);
                   check(primitiveRedirected instanceof rawAlways
                     && primitiveRedirectedPat === 9,
                     'always redirected primitive constructor fallback');

                   phase = 'poisoning';
                   let poisonHits = 0;
                   const poison = () => {
                     poisonHits++;
                     throw new Error('poisoned public surface reached');
                   };
                   const poisonedTarget = {};
                   for (const name of [
                     'never', '_never', 'always', '_always'
                   ]) {
                     Object.defineProperty(poisonedTarget, name, {
                       configurable: true,
                       get() {
                         poisonHits++;
                         throw new Error(`target ${name} getter reached`);
                       },
                     });
                   }
                   Pattern.prototype.never = poison;
                   Pattern.prototype._never = poison;
                   Pattern.prototype.always = poison;
                   Pattern.prototype._always = poison;
                   globalThis.never = poison;
                   globalThis.always = poison;
                   rustelScope.never = poison;
                   rustelScope.always = poison;
                   check(apply(rawNever, null, [
                     poison, poisonedTarget
                   ]) === poisonedTarget, 'saved never poisoning');
                   check(apply(rawAlways, null, [
                     value => value, poisonedTarget
                   ]) === poisonedTarget && poisonHits === 0,
                   'saved always poisoning');

                   globalThis.rawNeverAlwaysSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.rawNeverAlwaysSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("rawNeverAlwaysSurfaceError"), None);
    assert_eq!(runtime.get_number("rawNeverAlwaysSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "raw never/always surface and forwarding");
}

#[test]
fn raw_never_always_are_eager_host_free_and_preserve_only_returned_ownership() {
    let never_native = semantic_runtime(
        r#"(() => {
             let ignored = { marker: 'never-ignored' };
             let outer = { marker: 'never-outer' };
             let extra = { marker: 'never-extra' };
             globalThis.rawNeverNativeDroppedRefs = [
               new WeakRef(ignored), new WeakRef(outer), new WeakRef(extra)
             ];
             const source = pure('raw-never-native').setSteps(3);
             const location = { start: 4, end: 9 };
             source.__pure_loc = location;
             const query = source.query;
             const steps = source._steps;
             const pureValue = source.__pure;
             const result = Reflect.apply(Pattern.prototype._never, outer, [
               ignored, source, extra
             ]);
             globalThis.rawNeverNativeIdentity = Number(
               result === source
               && result.query === query
               && result._steps === steps
               && Object.hasOwn(result, '__pure')
               && result.__pure === pureValue
               && result.__pure_loc === location
             );
             ignored = outer = extra = null;
             return result;
           })()"#,
    );
    assert_eq!(never_native.get_number("rawNeverNativeIdentity"), Some(1.0));
    assert!(
        !never_native.active_needs_host(),
        "an exact native raw-never target became host-backed"
    );
    assert_eq!(
        never_native
            .active_pattern()
            .expect("raw never native pattern")
            .steps,
        Some(Fraction::int(3))
    );
    for _ in 0..3 {
        never_native.run_gc();
    }
    never_native
        .eval(
            r#"globalThis.rawNeverNativeTransientsPruned = Number(
                 rawNeverNativeDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               )"#,
        )
        .unwrap();
    assert_eq!(
        never_native.get_number("rawNeverNativeTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&never_native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-never-native"
    );
    assert_clean(&never_native, "raw never eager native target");
    never_native.clear_active();

    let always_native = semantic_runtime(
        r#"(() => {
             globalThis.rawAlwaysNativeHits = 0;
             globalThis.rawAlwaysNativeMismatch = 0;
             let transient = { marker: 'always-transient' };
             let callback = function (pat) {
               'use strict';
               rawAlwaysNativeHits++;
               if (this !== undefined || new.target !== undefined
                   || pat !== source
                   || transient.marker !== 'always-transient') {
                 rawAlwaysNativeMismatch++;
               }
               return pat;
             };
             globalThis.rawAlwaysNativeDroppedRefs = [
               new WeakRef(callback), new WeakRef(transient)
             ];
             const source = pure('raw-always-native').setSteps(5);
             const location = { start: 11, end: 17 };
             source.__pure_loc = location;
             const query = source.query;
             const steps = source._steps;
             const pureValue = source.__pure;
             const result = source._always(callback);
             globalThis.rawAlwaysNativeIdentity = Number(
               result === source
               && result.query === query
               && result._steps === steps
               && Object.hasOwn(result, '__pure')
               && result.__pure === pureValue
               && result.__pure_loc === location
             );
             callback = transient = null;
             return result;
           })()"#,
    );
    assert_eq!(always_native.get_number("rawAlwaysNativeHits"), Some(1.0));
    assert_eq!(
        always_native.get_number("rawAlwaysNativeMismatch"),
        Some(0.0)
    );
    assert_eq!(
        always_native.get_number("rawAlwaysNativeIdentity"),
        Some(1.0)
    );
    assert!(
        !always_native.active_needs_host(),
        "an eager identity raw-always return retained its callback"
    );
    assert_eq!(
        always_native
            .active_pattern()
            .expect("raw always native pattern")
            .steps,
        Some(Fraction::int(5))
    );
    for _ in 0..3 {
        always_native.run_gc();
    }
    always_native
        .eval(
            r#"globalThis.rawAlwaysNativeTransientsPruned = Number(
                 rawAlwaysNativeDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               )"#,
        )
        .unwrap();
    assert_eq!(
        always_native.get_number("rawAlwaysNativeTransientsPruned"),
        Some(1.0)
    );
    assert_eq!(
        query_active(&always_native, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "raw-always-native"
    );
    assert_eq!(
        always_native.get_number("rawAlwaysNativeHits"),
        Some(1.0),
        "raw always callback was reread during query"
    );
    assert_clean(&always_native, "raw always eager native return");
    always_native.clear_active();

    let never_owned = semantic_runtime(
        r#"(() => {
             globalThis.rawNeverOwnedQueryHits = 0;
             const makePattern = kept => new Pattern(state => {
               rawNeverOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-never-owned' };
             let ignored = { marker: 'never-owned-ignored' };
             let outer = { marker: 'never-owned-outer' };
             let extra = { marker: 'never-owned-extra' };
             let target = makePattern(kept);
             globalThis.rawNeverOwnedKeptRef = new WeakRef(kept);
             globalThis.rawNeverOwnedDroppedRefs = [
               new WeakRef(ignored), new WeakRef(outer), new WeakRef(extra)
             ];
             const result = Reflect.apply(Pattern.prototype._never, outer, [
               ignored, target, extra
             ]);
             globalThis.rawNeverOwnedIdentity = Number(result === target);
             kept = ignored = outer = extra = target = null;
             return result;
           })()"#,
    );
    assert_eq!(never_owned.get_number("rawNeverOwnedIdentity"), Some(1.0));
    assert_eq!(never_owned.get_number("rawNeverOwnedQueryHits"), Some(0.0));
    assert!(
        never_owned.active_needs_host(),
        "raw never lost its exact JavaScript target ownership"
    );
    for _ in 0..3 {
        never_owned.run_gc();
    }
    never_owned
        .eval(
            r#"globalThis.rawNeverOwnedKeptAlive = Number(
                 rawNeverOwnedKeptRef.deref()?.marker === 'raw-never-owned'
               );
               globalThis.rawNeverOwnedTransientsPruned = Number(
                 rawNeverOwnedDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(never_owned.get_number("rawNeverOwnedKeptAlive"), Some(1.0));
    assert_eq!(
        never_owned.get_number("rawNeverOwnedTransientsPruned"),
        Some(1.0)
    );
    let never_values = query_active(&never_owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        never_values
            .iter()
            .any(|value| value.contains("raw-never-owned")),
        "raw never JavaScript target was not queryable: {never_values:?}"
    );
    assert_eq!(never_owned.get_number("rawNeverOwnedQueryHits"), Some(1.0));
    assert_clean(&never_owned, "raw never returned ownership");
    never_owned.clear_active();

    let always_owned = semantic_runtime(
        r#"(() => {
             globalThis.rawAlwaysOwnedHits = 0;
             globalThis.rawAlwaysOwnedQueryHits = 0;
             globalThis.rawAlwaysOwnedMismatch = 0;
             const makePattern = kept => new Pattern(state => {
               rawAlwaysOwnedQueryHits++;
               return pure(kept).query(state);
             });
             let kept = { marker: 'raw-always-owned' };
             let outer = { marker: 'always-owned-outer' };
             let target = { marker: 'always-owned-target' };
             let transient = { marker: 'always-owned-transient' };
             let extra = { marker: 'always-owned-extra' };
             let callback = function (received) {
               'use strict';
               rawAlwaysOwnedHits++;
               if (this !== undefined || new.target !== undefined
                   || received !== target
                   || transient.marker !== 'always-owned-transient') {
                 rawAlwaysOwnedMismatch++;
               }
               return makePattern(kept);
             };
             globalThis.rawAlwaysOwnedKeptRef = new WeakRef(kept);
             globalThis.rawAlwaysOwnedDroppedRefs = [
               new WeakRef(outer), new WeakRef(target),
               new WeakRef(transient), new WeakRef(extra),
               new WeakRef(callback)
             ];
             const result = Reflect.apply(Pattern.prototype._always, outer, [
               callback, target, extra
             ]);
             kept = outer = target = transient = extra = callback = null;
             return result;
           })()"#,
    );
    assert_eq!(always_owned.get_number("rawAlwaysOwnedHits"), Some(1.0));
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedQueryHits"),
        Some(0.0)
    );
    assert_eq!(always_owned.get_number("rawAlwaysOwnedMismatch"), Some(0.0));
    assert!(
        always_owned.active_needs_host(),
        "raw always lost its returned JavaScript Pattern ownership"
    );
    for _ in 0..3 {
        always_owned.run_gc();
    }
    always_owned
        .eval(
            r#"globalThis.rawAlwaysOwnedKeptAlive = Number(
                 rawAlwaysOwnedKeptRef.deref()?.marker === 'raw-always-owned'
               );
               globalThis.rawAlwaysOwnedTransientsPruned = Number(
                 rawAlwaysOwnedDroppedRefs.every(
                   ref => ref.deref() === undefined
                 )
               );"#,
        )
        .unwrap();
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedKeptAlive"),
        Some(1.0)
    );
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedTransientsPruned"),
        Some(1.0)
    );
    let always_values = query_active(&always_owned, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        always_values
            .iter()
            .any(|value| value.contains("raw-always-owned")),
        "raw always JavaScript terminal was not queryable: {always_values:?}"
    );
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedHits"),
        Some(1.0),
        "raw always callback ran again during query"
    );
    assert_eq!(
        always_owned.get_number("rawAlwaysOwnedQueryHits"),
        Some(1.0)
    );
    assert_clean(&always_owned, "raw always returned ownership");
    always_owned.clear_active();
}

#[test]
fn raw_never_always_are_pool_neutral_and_preserve_nested_accounting() {
    rustel_core::reset_stepwise_entries_materialised();
    let never_direct = semantic_runtime(
        r#"(() => {
             globalThis.rawNeverDirectHits = 0;
             return pure('raw-never-direct')._never(() => {
               rawNeverDirectHits++;
               return gap(20000).shrink(0);
             });
           })()"#,
    );
    assert_eq!(never_direct.get_number("rawNeverDirectHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw never charged or entered its ignored callback"
    );
    query_active(&never_direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(never_direct.get_number("rawNeverDirectHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw never charged shared work during query"
    );
    assert_clean(&never_direct, "raw never direct pool neutrality");
    never_direct.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let always_direct = semantic_runtime("pure('raw-always-direct')._always(pat => pat)");
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&always_direct, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw always identity charged shared work"
    );
    assert_clean(&always_direct, "raw always direct pool neutrality");
    always_direct.clear_active();

    let limit = rustel_core::MAX_STEPWISE_ENTRIES;
    rustel_core::reset_stepwise_entries_materialised();
    let suppressed = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawNeverSuppressedHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._never(() => {{
                 rawNeverSuppressedHits++;
                 return gap({}).shrink(0);
               }})
             );
           }})()"#,
        limit + 1
    ));
    assert_eq!(suppressed.get_number("rawNeverSuppressedHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&suppressed, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(suppressed.get_number("rawNeverSuppressedHits"), Some(0.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw never entered or charged nested +1 callback work"
    );
    assert_clean(&suppressed, "raw never nested suppression");
    suppressed.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawAlwaysExactHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._always(() => {{
                 rawAlwaysExactHits++;
                 return gap({limit}).shrink(0);
               }})
             );
           }})()"#
    ));
    assert_eq!(exact.get_number("rawAlwaysExactHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(exact.get_number("rawAlwaysExactHits"), Some(1.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw always hid or duplicated exact nested accounting"
    );
    assert_clean(&exact, "raw always nested exact accounting");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(&format!(
        r#"(() => {{
             globalThis.rawAlwaysOverHits = 0;
             return pure(0).polyBind(() =>
               pure('x')._always(() => {{
                 rawAlwaysOverHits++;
                 return gap({}).shrink(0);
               }})
             );
           }})()"#,
        limit + 1
    ));
    assert_eq!(over.get_number("rawAlwaysOverHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrink/grow",
            minimum_entries,
            limit: actual,
        })) if minimum_entries == limit + 1 && actual == limit
    ));
    assert_eq!(over.get_number("rawAlwaysOverHits"), Some(1.0));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "raw always nested +1 refusal partially materialised"
    );
    assert_clean(&over, "raw always nested +1 refusal");
    over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let recovery = semantic_runtime(&format!(
        r#"pure(0).polyBind(() =>
             pure('x')._always(() => gap({limit}).shrink(0))
           )"#
    ));
    query_active(&recovery, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        limit,
        "raw always nested accounting did not recover after refusal"
    );
    assert_clean(&recovery, "raw always nested recovery");
    recovery.clear_active();

    // Callback-created arbitrary graphs remain governed by their own
    // operation-specific limits; the two raw slots add no new policy.
}
