//! Differential coverage for the JavaScript-compatible callback-IR tier.

use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, PatternTransformIrMode, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().expect("runtime");
    runtime
        .install_semantic_bindings()
        .expect("semantic bindings");
    runtime
}

fn evaluate(runtime: &JsRuntime, source: &str) {
    runtime
        .evaluate_score(source, &TranspileOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
}

fn shown(runtime: &JsRuntime) -> String {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("query")
        .iter()
        .map(|hap| hap.show())
        .collect::<Vec<_>>()
        .join(";")
}

#[test]
fn identity_arrow_auto_compatibility_and_dual_run_agree() {
    let runtime = runtime();
    evaluate(
        &runtime,
        r#"s("bd sd hh cp").sometimesBy(1, pattern => pattern)"#,
    );
    let graph = runtime.active_pattern().expect("active graph");
    assert!(
        !graph.is_pure(),
        "the retained fallback keeps this graph impure"
    );
    assert!(
        !graph.reachable_callbacks().is_empty() || graph.purity().opaque,
        "the original JavaScript callback must remain owned"
    );

    runtime.set_pattern_transform_ir_mode(PatternTransformIrMode::Auto);
    runtime.reset_pattern_transform_ir_stats();
    let native = shown(&runtime);
    let native_stats = runtime.pattern_transform_ir_stats();
    assert!(native_stats.calls > 0, "the native candidate did not run");
    assert_eq!(native_stats.native_executions, native_stats.calls);
    assert_eq!(native_stats.compatibility_executions, 0);

    runtime.run_gc();
    runtime.run_gc();
    runtime.set_pattern_transform_ir_mode(PatternTransformIrMode::Compatibility);
    runtime.reset_pattern_transform_ir_stats();
    let compatibility = shown(&runtime);
    let compatibility_stats = runtime.pattern_transform_ir_stats();
    assert_eq!(compatibility, native);
    assert_eq!(compatibility_stats.calls, native_stats.calls);
    assert_eq!(compatibility_stats.native_executions, 0);
    assert_eq!(
        compatibility_stats.compatibility_executions,
        compatibility_stats.calls
    );

    runtime.set_pattern_transform_ir_mode(PatternTransformIrMode::DualRun);
    runtime.reset_pattern_transform_ir_stats();
    let dual = shown(&runtime);
    let dual_stats = runtime.pattern_transform_ir_stats();
    assert_eq!(dual, compatibility);
    assert_eq!(dual_stats.dual_runs, dual_stats.calls);
    assert_eq!(dual_stats.native_executions, dual_stats.calls);
    assert_eq!(dual_stats.compatibility_executions, dual_stats.calls);
    assert_eq!(dual_stats.mismatches, 0);
}

#[test]
fn nearby_dynamic_arrows_remain_on_the_complete_quickjs_tier() {
    for callback in ["pattern => pattern.rev()", "pattern => { return pattern; }"] {
        let runtime = runtime();
        evaluate(
            &runtime,
            &format!(r#"s("bd sd hh cp").sometimesBy(1, {callback})"#),
        );
        let graph = runtime.active_pattern().expect("active graph");
        assert!(!graph.is_pure(), "{callback}: callback graph became pure");
        runtime.reset_pattern_transform_ir_stats();
        assert!(!shown(&runtime).is_empty(), "{callback}: empty result");
        assert_eq!(
            runtime.pattern_transform_ir_stats().calls,
            0,
            "{callback}: unsupported source entered callback IR"
        );
    }
}

#[test]
fn source_recognition_uses_captured_intrinsics_not_mutable_globals() {
    let spoofed = runtime();
    evaluate(
        &spoofed,
        r#"
          Function.prototype.toString = () => "pattern => pattern";
          s("bd sd hh cp").sometimesBy(1, pattern => pattern.rev())
        "#,
    );
    spoofed.reset_pattern_transform_ir_stats();
    let spoofed_output = shown(&spoofed);
    assert_eq!(
        spoofed.pattern_transform_ir_stats().calls,
        0,
        "a user replacement spoofed callback-IR recognition"
    );

    let control = runtime();
    evaluate(
        &control,
        r#"s("bd sd hh cp").sometimesBy(1, pattern => pattern.rev())"#,
    );
    assert_eq!(spoofed_output, shown(&control));

    let hidden = runtime();
    evaluate(
        &hidden,
        r#"
          Function.prototype.toString = () => { throw new Error("user hook"); };
          s("bd sd hh cp").sometimesBy(1, pattern => pattern)
        "#,
    );
    hidden.reset_pattern_transform_ir_stats();
    assert!(!shown(&hidden).is_empty());
    assert!(
        hidden.pattern_transform_ir_stats().native_executions > 0,
        "the captured intrinsic was not isolated from the user replacement"
    );
}

#[test]
fn indexed_transformers_keep_their_original_javascript_call_contract() {
    let runtime = runtime();
    evaluate(
        &runtime,
        r#"pure('seed').echoWith(fastcat(2, 2), 1/8, pattern => pattern)"#,
    );
    runtime.reset_pattern_transform_ir_stats();
    let output = shown(&runtime);
    assert_eq!(
        runtime.pattern_transform_ir_stats().calls,
        0,
        "indexed callbacks must stay behind their batch JavaScript boundary"
    );
    assert!(
        !output.is_empty(),
        "the original callback returned no events"
    );
}
