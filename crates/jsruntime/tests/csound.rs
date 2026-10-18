//! Csound score routes retain control values and enforce loader scope.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rustel_core::{Hap, Value};
use rustel_fraction::Fraction;
use rustel_jsruntime::{CsoundSource, JsRuntime, QueryError, ScoreEffects, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) -> Vec<Hap> {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query")
}

#[test]
fn instrument_routes_preserve_custom_controls_and_final_mode() {
    let runtime = runtime();
    for (source, route, instrument) in [
        (
            "note('a4').csound()",
            "csound",
            Value::Str("triangle".into()),
        ),
        (
            "note('a4').csound('Bell')",
            "csound",
            Value::Str("Bell".into()),
        ),
        ("note('a4').csoundm(2.5)", "csoundm", Value::F64(2.5)),
        (
            "note('a4').csoundm('Bell')",
            "csoundm",
            Value::Str("Bell".into()),
        ),
        (
            "note('a4').csoundm(1).csound('Bell')",
            "csound",
            Value::Str("Bell".into()),
        ),
        (
            "note('a4').csound('Bell').csoundm(2)",
            "csoundm",
            Value::F64(2.0),
        ),
    ] {
        let haps = evaluate(
            &runtime,
            &format!("{source}.gain(0.4).set({{ brightness: 0.7, label: 'bell' }})"),
        );
        assert_eq!(haps.len(), 1, "{source}");
        let value = &haps[0].value;
        assert_eq!(value.get(route), Some(&instrument), "{source}");
        assert!(
            value
                .get(if route == "csound" {
                    "csoundm"
                } else {
                    "csound"
                })
                .is_none()
        );
        assert_eq!(value.get("note"), Some(&Value::Str("a4".into())));
        assert_eq!(value.get("gain"), Some(&Value::F64(0.4)));
        assert_eq!(value.get("brightness"), Some(&Value::F64(0.7)));
        assert_eq!(value.get("label"), Some(&Value::Str("bell".into())));
    }

    let haps = evaluate(&runtime, "pure(69).csound()");
    assert_eq!(haps[0].value.get("value"), Some(&Value::F64(69.0)));
    let haps = evaluate(&runtime, "note(\"c4 e4\").csound(\"triangle sine\")");
    assert_eq!(haps.len(), 2);
    assert_eq!(
        haps[0].value.get("csound").and_then(Value::as_str),
        Some("triangle")
    );
    assert_eq!(
        haps[1].value.get("csound").and_then(Value::as_str),
        Some("sine")
    );
}

#[test]
fn orchestra_loaders_cannot_escape_score_or_setup_transactions() {
    let runtime = runtime();
    for source in [
        "loadCSound('instr Bell\\nendin')",
        "loadOrc('https://example.org/bell.orc')",
        "loadCSound(42)",
        "loadOrc()",
    ] {
        let raw = runtime
            .eval(&format!("try {{ {source}; }} catch (_) {{}}"))
            .expect_err("raw load refused");
        assert!(raw.contains("loadCSound"), "{raw}");
        let (_, effects) = runtime.evaluate_score_with_effects_cancellable(
            &format!("pure('x').fmap(value => {{ try {{ {source}; }} catch (_) {{}} return value; }})"),
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &AtomicBool::new(false),
        ).expect("callback installed");
        assert_eq!(effects, ScoreEffects::default());
        let error = runtime
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect_err("query-time load refused");
        assert!(matches!(error, QueryError::Policy(message) if message.contains("loadCSound")));
    }
}

#[test]
fn definition_only_scores_commit_orchestras_and_still_refuse_invalid_results() {
    let runtime = runtime();
    for source in [
        "loadCSound`instr Bell\nendin`",
        "await loadCSound(\"instr Bell\\nendin\")",
        "loadCSound('instr Bell\\nendin'); undefined",
    ] {
        let (_, effects) = runtime
            .evaluate_score_with_effects_cancellable(
                source,
                &TranspileOptions::default(),
                Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .expect("definition-only score");
        assert_eq!(
            effects.csound,
            vec![CsoundSource::Code("instr Bell\nendin".into())]
        );
        assert!(
            runtime
                .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
                .expect("silent definition")
                .is_empty()
        );
    }
    for source in [
        "loadCSound(); 42",
        "loadCSound(); throw new Error('reject orchestra')",
        "loadCSound(); Promise.resolve().then(() => 0); undefined",
    ] {
        runtime
            .evaluate_score_with_effects_cancellable(
                source,
                &TranspileOptions::default(),
                Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .expect_err("invalid score refuses orchestra effects");
    }
}
