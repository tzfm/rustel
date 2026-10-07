use rustel_jsruntime::JsRuntime;

fn runtime_with_fraction() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

#[test]
fn rust_fraction_keeps_the_javascript_mutation_and_reflection_contract() {
    let runtime = runtime_with_fraction();
    runtime
        .eval(
            r#"
            (() => {
              const check = (condition, message) => {
                if (!condition) throw new Error(message);
              };
              const F = Fraction._original;
              const value = F('2/6');
              check(value.show() === '1/3', 'reduction');
              check(Object.keys(value).join('|') === 's|n|d', 'own field order');
              for (const name of ['s', 'n', 'd']) {
                const descriptor = Object.getOwnPropertyDescriptor(value, name);
                check(descriptor.writable && descriptor.enumerable && descriptor.configurable,
                  `${name} descriptor`);
              }

              value.s = -1n;
              value.n = 7n;
              value.d = 9n;
              check(value.add('2/9').show() === '-5/9', 'mutable fields');

              const argument = {
                n: { valueOf() { value.n = 5n; return 1; } },
                d: 9,
              };
              check(value.add(argument).show() === '-4/9', 'coercion order');

              class Derived extends F {}
              const derived = new Derived('5/7');
              check(derived instanceof Derived && derived instanceof F,
                'subclass identity');

              const originalPrototype = F.prototype;
              const replacement = { marker: 42 };
              F.prototype = replacement;
              try {
                const called = F('5/7');
                const built = new F('5/7');
                check(Object.getPrototypeOf(called) === replacement,
                  'call prototype replacement');
                check(Object.getPrototypeOf(built) === replacement,
                  'new prototype replacement');
              } finally {
                F.prototype = originalPrototype;
              }

              check(F.name === 'Fraction' && F.length === 2, 'raw function shape');
              check(Object.keys(F).length === 0, 'raw enumerable properties');
              check(Object.getOwnPropertyNames(F).join('|') === 'length|name|prototype',
                'ES module import shape');
              globalThis.fractionSurfaceOkay = 1;
            })();
            "#,
        )
        .expect("Fraction surface contract");
    assert_eq!(runtime.get_number("fractionSurfaceOkay"), Some(1.0));
}

#[test]
fn rust_fraction_refuses_bounded_work_and_recovers_in_the_same_runtime() {
    let runtime = runtime_with_fraction();
    runtime
        .eval(
            r#"
            (() => {
              const F = Fraction._original;
              const failures = [];
              const expectBound = (name, operation) => {
                try {
                  operation();
                  failures.push(`${name}:accepted`);
                } catch (error) {
                  if (error.name !== 'RangeError' || !error.message.includes('bounded')) {
                    failures.push(`${name}:${error.name}:${error.message}`);
                  }
                }
                if (F('1/3').add('1/6').toFraction() !== '1/2') {
                  failures.push(`${name}:runtime did not recover`);
                }
              };

              expectBound('result bits', () => F(2).pow(2_000_000));
              expectBound('factorization', () => F(100003n * 100019n).pow(F(1, 2)));
              if (F(Number.MIN_VALUE).toFraction() !== '0') {
                failures.push('accelerated Farey extreme diverged');
              }
              let previous = 1n;
              let current = 1n;
              for (let i = 0; i < 45; i++) {
                [previous, current] = [current, previous + current];
              }
              const common = 1n << 900000n;
              expectBound('gcd work', () => F(previous * common, current * common));
              const decimal = F(1);
              decimal.d = common + 1n;
              expectBound('decimal work', () => decimal.toString(100));
              const continued = F(1);
              continued.n = previous * common;
              continued.d = current * common;
              expectBound('continued work', () => continued.toContinued());
              const simplify = F(1);
              simplify.abs = () => ({
                toContinued: () => Array.from({length: 500}, () => 1n),
              });
              expectBound('simplify work', () => simplify.simplify());

              const hugeExponent = 1n << 200n;
              if (F(1).pow(hugeExponent).toFraction() !== '1'
                  || F(-1).pow(hugeExponent + 1n).toFraction() !== '-1') {
                failures.push('bounded pow rejected a constant-sized result');
              }

              const poisoned = F('1/3');
              poisoned.d = 0n;
              const operations = [
                () => poisoned.abs(), () => poisoned.neg(),
                () => poisoned.add(1), () => poisoned.sub(1),
                () => poisoned.mul(2), () => poisoned.div(2),
                () => poisoned.clone(), () => poisoned.mod(),
                () => poisoned.mod(2), () => poisoned.gcd(2),
                () => poisoned.lcm(2), () => poisoned.inverse(),
                () => poisoned.pow(2), () => poisoned.pow(F(1, 2)),
                () => poisoned.log(2), () => poisoned.equals(1),
                () => poisoned.compare(1), () => poisoned.ceil(),
                () => poisoned.floor(), () => poisoned.round(),
                () => poisoned.roundTo(F(1, 8)),
                () => poisoned.divisible(2), () => poisoned.valueOf(),
                () => poisoned.toString(), () => poisoned.toFraction(),
                () => poisoned.toLatex(), () => poisoned.toContinued(),
                () => poisoned.simplify(),
              ];
              for (const operation of operations) {
                try { operation(); } catch (_) {}
                if (F('1/3').add('1/6').toFraction() !== '1/2') {
                  failures.push('mutated denominator poisoned runtime');
                  break;
                }
              }
              globalThis.fractionSafetyFailures = failures.join('|');
            })();
            "#,
        )
        .expect("bounded Fraction operations");
    assert_eq!(
        runtime.get_string("fractionSafetyFailures").as_deref(),
        Some("")
    );
}

#[test]
fn rust_fraction_roots_survive_gc_and_stay_runtime_local() {
    let first = runtime_with_fraction();
    let second = runtime_with_fraction();
    first
        .eval(
            r#"
            Fraction._original.prototype.runtimeMarker = 'first';
            globalThis.savedFractions = Array.from({ length: 2048 }, (_, i) =>
              Fraction._original(BigInt(i + 1), BigInt(i + 2)));
            "#,
        )
        .expect("first runtime values");
    second
        .eval(
            "globalThis.secondHasMarker = 'runtimeMarker' in Fraction._original.prototype ? 1 : 0",
        )
        .expect("second runtime isolation");

    for _ in 0..4 {
        first.run_gc();
        second.run_gc();
    }
    first
        .eval(
            r#"
            globalThis.fractionGcOkay =
              savedFractions[1023].add(savedFractions[1024]).d > 0n ? 1 : 0;
            "#,
        )
        .expect("Fraction values after GC");

    assert_eq!(second.get_number("secondHasMarker"), Some(0.0));
    assert_eq!(first.get_number("fractionGcOkay"), Some(1.0));
}
