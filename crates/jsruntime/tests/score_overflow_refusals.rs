//! Mini-notation slow factors preserve events from nonzero factors.
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Vec<rustel_core::Hap> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .expect("score evaluates");
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::from(4))
        .expect("score queries")
}

#[test]
fn mini_slow_zero_keeps_events_from_nonzero_factors() {
    let runtime = runtime();
    let shape = |haps: Vec<rustel_core::Hap>| {
        haps.into_iter()
            .map(|hap| (hap.whole, hap.part, hap.value))
            .collect::<Vec<_>>()
    };
    for (score, expected) in [
        (r#"s("bd/[0 2]")"#, r#"s("bd/[~ 2]")"#),
        (r#"s("bd/[2 0]")"#, r#"s("bd/[2 ~]")"#),
        (r#"s("bd/<0 2>")"#, r#"s("bd/<~ 2>")"#),
    ] {
        let expected = evaluate(&runtime, expected);
        assert!(!expected.is_empty());
        assert_eq!(shape(evaluate(&runtime, score)), shape(expected), "{score}");
    }
    for score in [r#"s("bd/0")"#, r#"s("bd/[0]")"#] {
        assert!(evaluate(&runtime, score).is_empty());
    }
}
