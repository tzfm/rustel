//! Native fraction guards on score combinators: `morph` refuses a groove
//! outside the native fraction range, and `fastcat` never needs the step LCM
//! of its inputs.

use rustel_core::{Hap, QueryLimit};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const PRIMES_TO_101: [u32; 26] = [
    2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    101,
];

fn query_first_cycle(source: &str) -> Result<Vec<Hap>, QueryError> {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("could not install `{source}`: {error}"));
    runtime.query_cancellable(
        Slot::Active,
        0,
        Fraction::ZERO,
        Fraction::ONE,
        Duration::from_secs(5),
        &AtomicBool::new(false),
    )
}

fn timing(haps: Vec<Hap>) -> Vec<String> {
    haps.into_iter()
        .map(|hap| format!("{:?} {:?} {:?}", hap.whole, hap.part, hap.value))
        .collect()
}

#[test]
fn morph_refuses_a_groove_outside_the_native_fraction_range() {
    for by in ["1e38", "-1e38"] {
        let source = format!(r#"s("bd").struct(morph([1,0,0,0,0],[0,0,1,0,0],{by}))"#);
        let result = query_first_cycle(&source);
        assert!(
            matches!(
                &result,
                Err(QueryError::Limit(QueryLimit::NativeFraction {
                    operation: "morph"
                }))
            ),
            "{source}: {result:?}"
        );
    }
}

#[test]
fn morph_keeps_an_ordinary_groove() {
    let haps = query_first_cycle(r#"s("bd").struct(morph([1,0,0,0,0],[0,0,1,0,0],0.5))"#)
        .expect("morph plays");
    let begins: Vec<_> = haps
        .iter()
        .map(|hap| hap.whole.map(|whole| whole.begin))
        .collect();
    assert_eq!(begins, [Some(Fraction::new(1, 5))]);
}

#[test]
fn fastcat_plays_inputs_whose_step_lcm_is_unrepresentable() {
    let mini = PRIMES_TO_101.map(|count| format!("[bd!{count}]")).join(" ");
    let expected = timing(query_first_cycle(&format!(r#"s("{mini}")"#)).expect("mini plays"));
    assert_eq!(expected.len(), PRIMES_TO_101.iter().sum::<u32>() as usize);
    let args = PRIMES_TO_101
        .map(|count| format!(r#""bd!{count}""#))
        .join(", ");
    for name in ["fastcat", "seq", "sequence"] {
        let source = format!("s({name}({args}))");
        let haps = query_first_cycle(&source).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(timing(haps), expected, "{name}");
    }
}
