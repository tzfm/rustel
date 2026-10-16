use super::*;

#[test]
fn shrinklist_growlist_helpers_preserve_surface_ranges_and_dynamic_dispatch() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .eval(
            r#"(() => {
              const check = (condition, label) => {
                if (!condition) throw new Error(label);
              };
              const isConstructor = value => {
                try { Reflect.construct(function () {}, [], value); return true; }
                catch (_) { return false; }
              };
              const flags = (object, name) => {
                const descriptor = Object.getOwnPropertyDescriptor(object, name);
                return descriptor && descriptor.writable
                  && descriptor.enumerable && descriptor.configurable;
              };

              check(shrinklist.name === 'shrinklist' && shrinklist.length === 2,
                'shrinklist arrow reflection');
              check(growlist.name === 'growlist' && growlist.length === 2,
                'growlist arrow reflection');
              check(!Object.hasOwn(shrinklist, 'prototype') && !isConstructor(shrinklist),
                'shrinklist must be a nonconstructible arrow');
              check(!Object.hasOwn(growlist, 'prototype') && !isConstructor(growlist),
                'growlist must be a nonconstructible arrow');
              check(s_taperlist === shrinklist, 'free taperlist identity');
              check(flags(globalThis, 'shrinklist') && flags(globalThis, 'growlist')
                && flags(globalThis, 's_taperlist'), 'ordinary global descriptors');

              const protoShrink = Pattern.prototype.shrinklist;
              const protoGrow = Pattern.prototype.growlist;
              check(protoShrink.name === '' && protoShrink.length === 1
                && isConstructor(protoShrink), 'prototype shrinklist reflection');
              check(protoGrow.name === '' && protoGrow.length === 1
                && isConstructor(protoGrow), 'prototype growlist reflection');
              check(flags(Pattern.prototype, 'shrinklist')
                && flags(Pattern.prototype, 'growlist')
                && flags(Pattern.prototype, 's_taperlist'),
                'ordinary prototype descriptors');
              check(Pattern.prototype.s_taperlist === protoShrink,
                'prototype taperlist identity');
              check(Array.isArray(Reflect.construct(protoShrink, [1])),
                'constructible shrinklist result');
              let growConstruction = 'none';
              try { Reflect.construct(protoGrow, [1]); }
              catch (error) { growConstruction = error.name; }
              check(growConstruction === 'TypeError', 'growlist constructor phase');
              check([
                '_shrinklist', '_growlist', '_s_taperlist', 'taperlist',
                's_shrinklist', 's_growlist', 'waxlist', 'wanelist'
              ].every(name => !Object.hasOwn(globalThis, name)
                && !Object.hasOwn(Pattern.prototype, name)), 'invented list aliases');

              const base = sequence(pure('a'), pure('b'), pure('c'), pure('d'));
              const steps = list => list.map(pattern => pattern._steps.show()).join(',');
              const one = base.shrinklist(1);
              check(Array.isArray(one) && Object.getPrototypeOf(one) === Array.prototype,
                'ordinary result array');
              check(one.length === 4 && steps(one) === '4/1,3/1,2/1,1/1',
                `positive ranges: ${one.length}|${steps(one)}`);
              check(one.every((pattern, index) => pattern instanceof Pattern
                && one.indexOf(pattern) === index), 'distinct Pattern wrappers');
              const state = { span: { begin: 0, end: 1 }, controls: {} };
              const windows = list => list.map(pattern => pattern.query(state).map(hap =>
                `${hap.part.begin.show()}>${hap.part.end.show()}:${hap.value}`
              ).join(';')).join('|');
              check(windows(one) ===
                '0/1>1/4:a;1/4>1/2:b;1/2>3/4:c;3/4>1/1:d|'
                + '0/1>1/3:b;1/3>2/3:c;2/3>1/1:d|'
                + '0/1>1/2:c;1/2>1/1:d|0/1>1/1:d',
                'positive query windows');
              check(steps(base.shrinklist(-1)) === '4/1,3/1,2/1,1/1',
                'negative ranges');
              check(windows(base.shrinklist(-1)) ===
                '0/1>1/4:a;1/4>1/2:b;1/2>3/4:c;3/4>1/1:d|'
                + '0/1>1/3:a;1/3>2/3:b;2/3>1/1:c|'
                + '0/1>1/2:a;1/2>1/1:b|0/1>1/1:a',
                'negative query windows');
              const zero = base.shrinklist(0);
              check(zero.length === 4 && steps(zero) === '4/1,4/1,4/1,4/1'
                && zero.every(pattern => pattern !== base), 'zero amount behavior');
              const terminal = base.shrinklist(2);
              check(steps(terminal) === '4/1,2/1,0/1'
                && terminal[2] !== nothing && terminal[2] instanceof Pattern
                && terminal[2].query(state).length === 0,
                'distinct inclusive terminal range');
              check(base.shrinklist(5).length === 1, 'large amount cutoff');
              const thirds = sequence(pure('a'), pure('b'), pure('c'))
                .shrinklist([1, 2]);
              check(steps(thirds) === '3/1,2/1'
                && windows(thirds) ===
                  '0/1>1/3:a;1/3>2/3:b;2/3>1/1:c|'
                  + '0/1>1/2:b;1/2>1/1:c',
                'exact non-binary rational projection');
              check(steps(base.shrinklist([1, 2.5])) === '4/1,3/1,2/1',
                'fractional times loop');
              check(base.shrinklist([1, -1]).length === 0,
                'negative times loop');
              check(base.shrinklist([1, 0])[0] === base,
                'strict numeric zero shortcut identity');
              const noSteps = pure('x').setSteps(undefined);
              check(noSteps.shrinklist(Symbol('ignored'))[0] === noSteps,
                'no-step shortcut precedes coercion');
              check(steps(base.growlist(1)) === '1/1,2/1,3/1,4/1',
                'growlist reverses shrinklist');
              check(windows(base.growlist(1)) ===
                '0/1>1/1:d|0/1>1/2:c;1/2>1/1:d|'
                + '0/1>1/3:b;1/3>2/3:c;2/3>1/1:d|'
                + '0/1>1/4:a;1/4>1/2:b;1/2>3/4:c;3/4>1/1:d',
                'growlist query windows');

              let zeroStepError = 'none';
              try { gap(0).shrinklist(1); }
              catch (error) { zeroStepError = error.message; }
              check(zeroStepError.includes('Division by Zero'),
                'zero-step eager division phase');
              const publicFraction = globalThis.Fraction;
              globalThis.Fraction = () => { throw new Error('mutable global Fraction'); };
              check(base.shrinklist(1).length === 4, 'lexical Fraction capture');
              globalThis.Fraction = publicFraction;

              const dispatch = [];
              const receiver = {
                shrinklist(value) { dispatch.push(`free-shrink:${value}`); return ['S']; },
                growlist(value) { dispatch.push(`free-grow:${value}`); return ['G']; },
              };
              check(shrinklist(2, receiver)[0] === 'S'
                && s_taperlist(3, receiver)[0] === 'S'
                && growlist(4, receiver)[0] === 'G', 'free dynamic dispatch');
              const reversed = ['left', 'right'];
              let reverseReceiver;
              const growReceiver = {
                shrinklist(value) {
                  dispatch.push(`proto-grow:${value}`);
                  reversed.reverse = function () {
                    reverseReceiver = this;
                    return Array.prototype.reverse.call(this);
                  };
                  return reversed;
                },
              };
              const grown = protoGrow.call(growReceiver, 5);
              check(grown === reversed && reverseReceiver === reversed
                && grown.join(',') === 'right,left',
                'growlist dynamic shrinklist and in-place reverse');
              check(dispatch.join('|') ===
                'free-shrink:2|free-shrink:3|free-grow:4|proto-grow:5',
                'dynamic dispatch arguments');
              globalThis.__shrinklistSurfaceOkay = 1;
            })()"#,
        )
        .unwrap();
    assert_eq!(runtime.get_number("__shrinklistSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "shrinklist helper surface");
}

#[test]
fn shrinklist_preflight_is_atomic_typed_and_recovers() {
    rustel_core::reset_stepwise_entries_materialised();
    let exact = semantic_runtime("stack(...gap(16384).shrinklist(0))");
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "the exact helper boundary did not materialise its complete wrapper list"
    );
    assert!(query_active(&exact, Fraction::ZERO, Fraction::ZERO).is_ok());
    assert_clean(&exact, "exact shrinklist boundary");
    exact.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let over = semantic_runtime(
        r#"(() => {
             const source = gap(16385);
             globalThis.shrinklistZoomHits = 0;
             source.zoom = function (...args) {
               globalThis.shrinklistZoomHits++;
               return Pattern.prototype.zoom.call(this, ...args);
             };
             return stack(...source.shrinklist(0));
           })()"#,
    );
    assert_eq!(
        over.get_number("shrinklistZoomHits"),
        Some(0.0),
        "oversized shrinklist entered mutable zoom before preflight"
    );
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        0,
        "oversized shrinklist completed wrappers before refusal"
    );
    assert!(matches!(
        query_active(&over, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&over, "oversized shrinklist refusal");

    over.evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("same runtime should recover after shrinklist refusal");
    assert_eq!(
        query_active(&over, Fraction::ZERO, Fraction::ONE).unwrap()[0]
            .value
            .show(),
        "recovered"
    );
    assert_clean(&over, "shrinklist recovery");
    over.clear_active();

    rustel_core::reset_stepwise_entries_materialised();
    let started = Instant::now();
    let infinite = semantic_runtime(
        r#"(() => {
             const source = gap(1);
             globalThis.infiniteShrinklistZoomHits = 0;
             source.zoom = function () {
               globalThis.infiniteShrinklistZoomHits++;
               return this;
             };
             return stack(...source.shrinklist([0, Infinity]));
           })()"#,
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "infinite shrinklist times escaped the bounded planner"
    );
    assert_eq!(infinite.get_number("infiniteShrinklistZoomHits"), Some(0.0));
    assert_eq!(rustel_core::stepwise_entries_materialised(), 0);
    assert!(matches!(
        query_active(&infinite, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_clean(&infinite, "infinite-times shrinklist refusal");
    infinite.clear_active();
}

#[test]
fn shrinklist_and_growlist_share_the_cumulative_stepwise_allowance() {
    let exact = semantic_runtime(
        r#"fastcat(pure(0), pure(1)).polyBind(() =>
             stack(...gap(8192).growlist(0))
           )"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&exact, Fraction::ZERO, Fraction::ONE).unwrap();
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        16_384,
        "growlist charged or materialised its shrinklist result twice"
    );
    assert_clean(&exact, "cumulative growlist exact side");
    exact.clear_active();

    let refused = semantic_runtime(
        r#"(() => {
             globalThis.cumulativeShrinklistZoomHits = 0;
             return fastcat(pure(0), pure(1)).polyBind(value => {
               const source = gap(value === 0 ? 8192 : 8193);
               source.zoom = function (...args) {
                 globalThis.cumulativeShrinklistZoomHits++;
                 return Pattern.prototype.zoom.call(this, ...args);
               };
               return stack(...source.shrinklist(0));
             });
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&refused, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_192,
        "the cumulative overshoot materialised its refused branch"
    );
    assert_eq!(
        refused.get_number("cumulativeShrinklistZoomHits"),
        Some(8_192.0),
        "the refused branch reached dynamic zoom"
    );
    assert_clean(&refused, "cumulative shrinklist refusal");

    refused
        .eval("globalThis.cumulativeShrinklistZoomHits = 0")
        .unwrap();
    rustel_core::reset_stepwise_entries_materialised();
    query_active(&refused, Fraction::ZERO, Fraction::new(1, 2)).unwrap();
    assert_eq!(rustel_core::stepwise_entries_materialised(), 8_192);
    assert_eq!(
        refused.get_number("cumulativeShrinklistZoomHits"),
        Some(8_192.0)
    );
    assert_clean(&refused, "cumulative shrinklist recovery");
    refused.clear_active();

    let mixed = semantic_runtime(
        r#"(() => {
             globalThis.mixedShrinklistZoomHits = 0;
             return fastcat(pure(0), pure(1)).polyBind(value => {
               if (value === 0) return gap(8000).shrink(0);
               const source = gap(8385);
               source.zoom = function (...args) {
                 globalThis.mixedShrinklistZoomHits++;
                 return Pattern.prototype.zoom.call(this, ...args);
               };
               return stack(...source.shrinklist(0));
             });
           })()"#,
    );
    rustel_core::reset_stepwise_entries_materialised();
    assert!(matches!(
        query_active(&mixed, Fraction::ZERO, Fraction::ONE),
        Err(QueryError::Limit(QueryLimit::StepwiseExpansion {
            operation: "shrinklist",
            minimum_entries: 16_385,
            limit: 16_384,
        }))
    ));
    assert_eq!(
        rustel_core::stepwise_entries_materialised(),
        8_000,
        "shrinklist used a separate allowance or materialised after mixed refusal"
    );
    assert_eq!(mixed.get_number("mixedShrinklistZoomHits"), Some(0.0));
    assert_clean(&mixed, "mixed shrink/shrinklist refusal");
    mixed.clear_active();
}

#[test]
fn retained_shrinklist_entry_keeps_its_js_owned_value_alive() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 let marker = { marker: 'retained-shrinklist-entry' };
                 globalThis.retainedShrinklistRef = new WeakRef(marker);
                 let source = sequence(pure(marker), pure('tail'));
                 let list = source.shrinklist(1);
                 const retained = list[0];
                 marker = null;
                 source = null;
                 list = null;
                 return retained;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("retain one wrapper from a shrinklist array");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            r#"globalThis.retainedShrinklistAlive = Number(
                 retainedShrinklistRef.deref()?.marker === 'retained-shrinklist-entry'
               )"#,
        )
        .unwrap();
    assert_eq!(runtime.get_number("retainedShrinklistAlive"), Some(1.0));
    let values = query_active(&runtime, Fraction::ZERO, Fraction::ONE)
        .unwrap()
        .into_iter()
        .map(|hap| hap.value.show())
        .collect::<Vec<_>>();
    assert!(
        values
            .iter()
            .any(|value| value.contains("retained-shrinklist-entry")),
        "retained wrapper lost the JS-owned value from its source sidecar: {values:?}"
    );
    assert_clean(&runtime, "retained shrinklist entry ownership");
    runtime.clear_active();
}

#[test]
fn retained_zero_width_shrinklist_entry_keeps_its_source_sidecar_alive() {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
        .evaluate_score(
            r#"(() => {
                 let marker = { marker: 'retained-zero-width-source' };
                 globalThis.zeroWidthShrinklistRef = new WeakRef(marker);
                 let source = sequence(
                   pure(marker), pure('b'), pure('c'), pure('d')
                 );
                 let list = source.shrinklist([1, 5]);
                 const terminal = list[4];
                 globalThis.retainedZeroWidthTerminal = terminal;
                 marker = null;
                 source = null;
                 list = null;
                 return terminal;
               })()"#,
            &TranspileOptions::default(),
        )
        .expect("retain only the zero-width shrinklist terminal");
    for _ in 0..3 {
        runtime.run_gc();
    }
    runtime
        .eval(
            r#"globalThis.zeroWidthShrinklistSourceAlive = Number(
                 zeroWidthShrinklistRef.deref()?.marker
                   === 'retained-zero-width-source'
               )"#,
        )
        .unwrap();
    assert_eq!(
        runtime.get_number("zeroWidthShrinklistSourceAlive"),
        Some(1.0),
        "the distinct terminal wrapper pruned its pinned source ownership"
    );
    assert_eq!(
        runtime
            .active_pattern()
            .expect("zero-width shrinklist terminal")
            .steps,
        Some(Fraction::ZERO)
    );
    assert!(
        query_active(&runtime, Fraction::ZERO, Fraction::ONE)
            .unwrap()
            .is_empty()
    );
    assert_clean(&runtime, "retained zero-width shrinklist ownership");
    runtime.clear_active();
}

#[test]
fn canonical_shrink_grow_pair_arrays_keep_identity_asymmetry_and_aliases() {
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
                 const base = sequence('a', 'b', 'c', 'd');
                 const pair = [1, 2];
                 phase = 'shrink';
                 const shrunk = base.shrink(pair);
                 phase = 'grow';
                 const grown = base.grow(pair);
                 phase = 'taper';
                 const tapered = base.s_taper(pair);
                 phase = 'shrink-steps';
                 check(shrunk._steps.show() === '7/1', 'shrink pair steps');
                 phase = 'grow-steps';
                 check(grown._steps.show() === '13/1', 'grow pair steps');
                 phase = 'taper-steps';
                 check(tapered._steps.show() === '7/1', 'taper pair steps');
                 phase = 'free-alias';
                 check(s_taper === shrink, 'free taper identity');
                 phase = 'proto-alias';
                 check(Pattern.prototype.s_taper === Pattern.prototype.shrink,
                   'prototype taper identity');
                 phase = 'raw';
                 const rawShrink = Pattern.prototype._shrink;
                 const rawGrow = Pattern.prototype._grow;
                 check(typeof rawShrink === 'function'
                   && typeof rawGrow === 'function', 'raw pair absent');
                 check(rawShrink !== Pattern.prototype.shrink
                   && rawGrow !== Pattern.prototype.grow
                   && rawShrink !== rawGrow, 'raw pair identity');
                 for (const [name, raw] of [
                   ['_shrink', rawShrink], ['_grow', rawGrow]
                 ]) {
                   const descriptor = Object.getOwnPropertyDescriptor(
                     Pattern.prototype, name
                   );
                   check(descriptor.value === raw
                     && descriptor.writable
                     && descriptor.enumerable
                     && descriptor.configurable,
                     `${name} descriptor`);
                   check(raw.name === '' && raw.length === 0
                     && Object.prototype.hasOwnProperty.call(raw, 'prototype'),
                     `${name} reflection`);
                   check(Reflect.construct(function () {}, [], raw)
                     instanceof raw, `${name} constructibility`);
                   check(!(name in globalThis)
                     && !Object.prototype.hasOwnProperty.call(rustelScope, name),
                     `${name} publication`);
                 }
                 const rawOrder = Object.keys(Pattern.prototype).filter(name => [
                   'shrinklist', 'growlist', 'shrink', '_shrink',
                   'grow', '_grow', 's_taper', 's_taperlist'
                 ].includes(name)).join('|');
                 check(rawOrder
                   === 'shrinklist|growlist|shrink|_shrink|grow|_grow|s_taper|s_taperlist',
                   `raw pair installation order: ${rawOrder}`);
                 check(!('_s_taper' in Pattern.prototype)
                   && !('_s_taper' in globalThis)
                   && !Object.prototype.hasOwnProperty.call(
                     rustelScope, '_s_taper'
                   ), 'raw taper leaked');

                 const receiver = sequence('a', 'b', 'c', 'd');
                 let shrinkArgument;
                 receiver.shrinklist = function (argument) {
                   shrinkArgument = argument;
                   return [this];
                 };
                 phase = 'instance-shrink';
                 receiver.shrink(pair);
                 check(shrinkArgument === pair, 'shrink cloned its pair');

                 phase = 'entries';
                 const left = gap(2);
                 const right = gap(3);
                 const returned = [left, right];
                 const order = [];
                 returned.reverse = function () {
                   order.push('reverse');
                   return Array.prototype.reverse.call(this);
                 };
                 returned.reduce = function (...args) {
                   order.push(`reduce:${this === returned}:${this[0] === right}`);
                   return Array.prototype.reduce.apply(this, args);
                 };
                 let growArgument;
                 let growlistHits = 0;
                 receiver.shrinklist = function (argument) {
                   growArgument = argument;
                   return returned;
                 };
                 receiver.growlist = function () { growlistHits++; return []; };
                 phase = 'custom-grow';
                 const customGrow = receiver.grow(pair);
                 phase = 'custom-check';
                 check(growArgument.show() === '-1/2', 'grow pair coercion');
                 check(growArgument instanceof Fraction._original,
                   'grow did not pass a canonical Fraction');
                 check(growArgument !== pair, 'grow forwarded its pair Array');
                 check(growlistHits === 0, 'grow dispatched through growlist');
                 check(returned[0] === right && returned[1] === left,
                   'grow did not reverse the returned array in place');
                 check(order.join('|') === 'reverse|reduce:true:true',
                   `grow phase order: ${order}`);
                 check(customGrow._steps.show() === '5/1', 'custom grow steps');
                 globalThis.canonicalPairSurfaceOkay = 1;
                 } catch (error) {
                   globalThis.canonicalPairSurfaceError =
                     `${phase}:${error.name}:${error.message}:${error.stack}`;
                 }
               })()"#,
            &TranspileOptions::default(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(runtime.get_string("canonicalPairSurfaceError"), None);
    assert_eq!(runtime.get_number("canonicalPairSurfaceOkay"), Some(1.0));
    assert_clean(&runtime, "canonical pair surface");

    runtime
        .evaluate_score(
            "sequence('a', 'b', 'c', 'd').shrink([1, 2])",
            &TranspileOptions::default(),
        )
        .unwrap();
    assert!(
        !runtime.active_needs_host(),
        "a pure pair must finish as a host-free native graph"
    );
    assert_clean(&runtime, "canonical pair purity");
    runtime.clear_active();
}
