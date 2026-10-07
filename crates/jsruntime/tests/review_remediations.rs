use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rustel_core::combinators::MAX_ITER_PARTS;
use rustel_core::{QueryLimit, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime(source: &str) -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("could not install `{source}`: {error}"));
    runtime
}

fn query(runtime: &JsRuntime) -> Result<Vec<rustel_core::Hap>, QueryError> {
    runtime.query_cancellable(
        Slot::Active,
        0,
        Fraction::ZERO,
        Fraction::ONE,
        Duration::from_secs(5),
        &AtomicBool::new(false),
    )
}

#[test]
fn construction_limit_reaches_javascript_without_running_the_transformer() {
    let requested = MAX_ITER_PARTS + 1;
    let runtime = runtime(&format!(
        "globalThis.reviewCalls = 0; \
         pure('a').applyN({requested}, pattern => {{ reviewCalls++; return pattern.rev(); }})"
    ));

    assert_eq!(runtime.get_number("reviewCalls"), Some(0.0));
    assert!(matches!(
        query(&runtime),
        Err(QueryError::Limit(QueryLimit::IterParts {
            operation: "applyN",
            parts: actual,
            limit: MAX_ITER_PARTS,
        })) if actual == requested
    ));
    assert_eq!(runtime.get_number("reviewCalls"), Some(0.0));

    runtime
        .evaluate_score("pure('recovered')", &TranspileOptions::default())
        .expect("replace refused graph");
    assert_eq!(query(&runtime).expect("query recovered graph").len(), 1);
}

#[test]
fn huge_iter_reports_a_saturated_host_count_without_constructing_it() {
    let runtime = runtime("pure('a').iter(100000000000000000000)");
    assert!(matches!(
        query(&runtime),
        Err(QueryError::Limit(QueryLimit::IterParts {
            operation: "iter",
            parts: u64::MAX,
            limit: MAX_ITER_PARTS,
        }))
    ));
}

#[test]
fn js_duration_and_native_composition_keep_strudel_semantics() {
    let duration = runtime("pure({ duration: 1/3, clip: 1/3 })");
    let haps = query(&duration).expect("duration query");
    assert_eq!(haps[0].duration(), Fraction::new(1, 9));

    let composed = runtime(
        "pure({ gain: 1, left: { x: 2 } }) \
         .add(pure({ gain: 2, right: { x: 3 } }))",
    );
    let haps = query(&composed).expect("composition query");
    assert_eq!(haps[0].value.get("gain"), Some(&Value::F64(3.0)));
    assert_eq!(
        haps[0].value.get("left").and_then(|value| value.get("x")),
        Some(&Value::F64(2.0))
    );
    assert_eq!(
        haps[0].value.get("right").and_then(|value| value.get("x")),
        Some(&Value::F64(3.0))
    );
}

#[test]
fn oversized_fraction_bigint_is_refused_before_host_parse() {
    // Decimal length is checked before Rust BigInt::parse so a planted
    // oversized value cannot allocate unbounded host RSS under the QuickJS
    // heap ceiling.
    let digits = "9".repeat((1_048_576usize * 301 / 1000) + 8);
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    let source = format!("Fraction({digits})");
    let error = runtime
        .evaluate_score(&source, &TranspileOptions::default())
        .expect_err("oversized Fraction must refuse");
    let message = error.to_string();
    assert!(
        message.contains("bounded arithmetic") || message.contains("RangeError"),
        "unexpected refusal: {message}"
    );
}
