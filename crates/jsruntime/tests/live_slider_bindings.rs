use rustel_core::Hap;
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, Slot};
use rustel_transpiler::TranspileOptions;

fn runtime() -> JsRuntime {
    let runtime = JsRuntime::new().unwrap();
    runtime.install_semantic_bindings().unwrap();
    runtime
}

fn evaluate(runtime: &JsRuntime, expression: &str) {
    runtime
        .evaluate_score(expression, &TranspileOptions::default())
        .unwrap();
}

fn query(runtime: &JsRuntime) -> Vec<Hap> {
    runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .unwrap()
}

#[test]
fn direct_gain_and_lpf_follow_distinct_native_cells() {
    let runtime = runtime();
    evaluate(
        &runtime,
        "s('sine').gain(sliderWithID('gain', .5)).lpf(sliderWithID('filter', 800, 100, 4000))",
    );
    let gain = runtime.slider_binding("gain").unwrap();
    let cutoff = runtime.slider_binding("filter").unwrap();
    assert_ne!(gain, cutoff);
    assert_eq!(query(&runtime)[0].live_controls, [gain, cutoff]);
    assert!(runtime.set_slider_value("gain", 0.75).unwrap());
    assert_eq!(runtime.slider_binding("gain"), Some(gain));
    assert_eq!(query(&runtime)[0].live_controls, [gain, cutoff]);
}

#[test]
fn pitch_combinators_after_sliders_keep_their_bound_controls_on_every_voice() {
    let runtime = runtime();
    for expression in [
        "note('c3').gain(sliderWithID('gain', 0)).lpf(sliderWithID('filter', 800)).scale('C:major').transpose(12).scaleTranspose(2).slow(16)",
        "chord('C').gain(sliderWithID('gain', 0)).lpf(sliderWithID('filter', 800)).voicing()",
        "i(0).gain(sliderWithID('gain', 0)).lpf(sliderWithID('filter', 800)).xen('12edo').withBase(440).ftrans(1)",
    ] {
        evaluate(&runtime, expression);
        let bindings = [
            runtime.slider_binding("gain").unwrap(),
            runtime.slider_binding("filter").unwrap(),
        ];
        let haps = query(&runtime);
        assert!(!haps.is_empty(), "{expression}");
        assert!(
            haps.iter().all(|hap| hap.live_controls == bindings),
            "{expression}"
        );
        assert!(runtime.set_slider_value("gain", 0.75).unwrap());
        assert!(
            query(&runtime).iter().all(|hap| {
                hap.live_controls == bindings
                    && hap.value.get("gain").and_then(rustel_core::Value::as_f64) == Some(0.75)
            }),
            "{expression}"
        );
    }
}

#[test]
fn sharing_slider_with_timing_does_not_bind_the_timing_only_branch() {
    let runtime = runtime();
    evaluate(
        &runtime,
        "const x = sliderWithID('shared', 2, 1, 4); stack(s('sine').fast(x).gain(x), s('triangle').fast(x))",
    );
    let token = runtime.slider_binding("shared").unwrap();
    let haps = query(&runtime);
    assert_eq!(
        haps.iter()
            .filter(|hap| hap.live_controls == [token, 0])
            .count(),
        2
    );
    assert_eq!(
        haps.iter()
            .filter(|hap| hap.live_controls == [0; 2])
            .count(),
        2
    );
}

#[test]
fn transformed_and_overwritten_values_cannot_retarget_sustained_voices() {
    let runtime = runtime();
    for expression in [
        "s('sine').gain(sliderWithID('x', .5).mul(2))",
        "s('sine').gain(sliderWithID('x', .5).fmap(x => x))",
        "s('sine').gain(sliderWithID('x', .5)).gain(.5)",
        "s('sine').lpf(sliderWithID('x', 800)).lpf(800)",
        "s('sine').gain(sliderWithID('x', .5)).fmap(x => x)",
        "s('sine').n(sliderWithID('x', 2))",
    ] {
        evaluate(&runtime, expression);
        assert!(
            query(&runtime)
                .iter()
                .all(|hap| hap.live_controls == [0; 2]),
            "{expression}"
        );
    }
}

#[test]
fn tokens_survive_failed_candidates_and_refresh_on_successful_evaluation() {
    let runtime = runtime();
    let score = "s('sine').gain(sliderWithID('same', .5))";
    evaluate(&runtime, score);
    let first = runtime.slider_binding("same").unwrap();
    runtime.snapshot_active_as_last_good();
    assert!(
        runtime
            .evaluate_score(
                "sliderWithID('same', .7); throw new Error('candidate')",
                &TranspileOptions::default()
            )
            .is_err()
    );
    assert_eq!(runtime.slider_binding("same"), Some(first));
    evaluate(&runtime, score);
    let second = runtime.slider_binding("same").unwrap();
    assert!(second > first + 1, "failed candidate token was reused");
    assert_eq!(query(&runtime)[0].live_controls, [second, 0]);
    runtime.restore_last_good_active().unwrap();
    assert_eq!(runtime.slider_binding("same"), Some(first));
    assert_eq!(query(&runtime)[0].live_controls, [first, 0]);
}

#[test]
fn binding_lookup_never_executes_planted_getters_or_mutable_globals() {
    let runtime = runtime();
    evaluate(
        &runtime,
        "Object.defineProperty(sliderValues, 'evil', {get() { throw new Error('getter'); }}); Object.getOwnPropertyDescriptor = () => { throw new Error('patched'); }; s('sine').gain(sliderWithID('plain', .5))",
    );
    assert_eq!(runtime.slider_binding("evil"), None);
    assert!(runtime.slider_binding("plain").is_some());
    assert_eq!(runtime.slider_binding("missing"), None);
}
