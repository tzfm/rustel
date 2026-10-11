use rustel_core::{Hap, Pattern, Value, controls::default_control_registry, pure};
use rustel_fraction::Fraction;

fn control(pattern: &Pattern, name: &str, value: Pattern) -> Pattern {
    default_control_registry()
        .get(name)
        .unwrap()
        .apply(pattern, Some(value))
}

fn query(pattern: Pattern) -> Hap {
    pattern.query_arc(Fraction::ZERO, Fraction::ONE).remove(0)
}

fn sound() -> Pattern {
    pure(Value::object([("s".into(), Value::Str("sine".into()))]))
}

#[test]
fn direct_controls_bind_independently_and_survive_time_and_visual_metadata() {
    let gain = pure(Value::F64(0.5)).with_slider_binding(41);
    let cutoff = pure(Value::F64(800.0)).with_slider_binding(42);
    let resonance = pure(Value::F64(4.0)).with_slider_binding(43);
    let pattern = control(
        &control(&control(&sound(), "gain", gain), "lpf", cutoff),
        "lpq",
        resonance,
    )
    .fast(Fraction::from(2))
    .with_ui_visual_slot(3)
    .tag("lead".into());
    let hap = query(pattern);
    assert_eq!(hap.live_controls, [41, 42, 43]);
    assert_eq!(hap.ui_visuals_context(), 8);
}

#[test]
fn equal_constant_overwrite_clears_only_the_replaced_parameter() {
    let pattern = control(
        &control(
            &sound(),
            "gain",
            pure(Value::F64(0.5)).with_slider_binding(41),
        ),
        "lpf",
        pure(Value::F64(800.0)).with_slider_binding(42),
    );
    let pattern = control(
        &pattern,
        "lpq",
        pure(Value::F64(4.0)).with_slider_binding(43),
    );
    let overwritten = control(&pattern, "gain", pure(Value::F64(0.5)));
    assert_eq!(query(overwritten).live_controls, [0, 42, 43]);
    assert_eq!(
        query(control(&pattern, "lpf", pure(Value::F64(800.0)))).live_controls,
        [41, 0, 43]
    );
    assert_eq!(
        query(control(&pattern, "lpq", pure(Value::F64(4.0)))).live_controls,
        [41, 42, 0]
    );
}

#[test]
fn arbitrary_maps_drop_bindings_even_when_the_result_is_equal() {
    let raw = pure(Value::F64(0.5)).with_slider_binding(41);
    let mapped = control(&sound(), "gain", raw.fmap(Clone::clone));
    assert_eq!(query(mapped).live_controls, [0; 3]);
    let bound = control(&sound(), "gain", raw);
    assert_eq!(query(bound.fmap(Clone::clone)).live_controls, [0; 3]);
    assert_eq!(
        query(bound.map_haps_native(|hap| Some(hap.clone()))).live_controls,
        [0; 3]
    );
}

#[test]
fn note_sample_index_and_timing_values_do_not_become_audio_bindings() {
    let raw = pure(Value::F64(2.0)).with_slider_binding(41);
    for name in ["note", "n", "speed", "pan"] {
        assert_eq!(
            query(control(&sound(), name, raw.clone())).live_controls,
            [0; 3]
        );
    }
    let pattern = control(&sound().fast(Fraction::from(2)), "gain", raw);
    assert_eq!(query(pattern).live_controls, [41, 0, 0]);
}

fn bound_pitch(value: Value) -> Pattern {
    let pattern = control(
        &control(
            &pure(value),
            "gain",
            pure(Value::F64(0.5)).with_slider_binding(41),
        ),
        "lpf",
        pure(Value::F64(800.0)).with_slider_binding(42),
    );
    control(
        &pattern,
        "lpq",
        pure(Value::F64(4.0)).with_slider_binding(43),
    )
}

#[test]
fn tonal_rewrites_keep_named_audio_bindings_without_binding_transformed_raw_values() {
    use rustel_core::combinators::{scale, scale_transpose, transpose};
    let note = bound_pitch(Value::object([("note".into(), Value::Str("c3".into()))]));
    let scaled = scale(&note, Value::Str("C:major".into()));
    for pattern in [
        transpose(&note, Value::F64(12.0)),
        scaled.clone(),
        scale_transpose(&scaled, Value::F64(2.0)),
    ] {
        let hap = query(pattern);
        assert_eq!(hap.live_controls, [41, 42, 43]);
        assert_eq!(hap.value.get("gain"), Some(&Value::F64(0.5)));
        assert_eq!(hap.value.get("cutoff"), Some(&Value::F64(800.0)));
        assert_eq!(hap.value.get("resonance"), Some(&Value::F64(4.0)));
    }
    let raw = pure(Value::F64(0.5)).with_slider_binding(41);
    let transformed = transpose(&raw, Value::F64(12.0));
    assert_eq!(
        query(control(&sound(), "gain", transformed)).live_controls,
        [0; 3]
    );
}

#[test]
fn frequency_rewrites_and_voicing_keep_only_controls_their_output_retains() {
    use rustel_core::{voicings, xen};
    let pitch = bound_pitch(Value::object([("i".into(), Value::F64(0.0))]));
    let tuned = xen::xen(&pitch, Value::Str("12edo".into()));
    for pattern in [
        tuned.clone(),
        xen::with_base(&tuned, Value::F64(440.0)),
        xen::ftrans(&tuned, Value::F64(1.0)),
    ] {
        assert_eq!(query(pattern).live_controls, [41, 42, 43]);
    }
    let chord = bound_pitch(Value::object([("chord".into(), Value::Str("C".into()))]));
    let voices = voicings::voicing(&chord).query_arc(Fraction::ZERO, Fraction::ONE);
    assert!(voices.len() > 1);
    assert!(voices.iter().all(|hap| hap.live_controls == [41, 42, 43]));
    // rootNotes deliberately returns just the note, removing audio controls.
    let root = query(voicings::root_notes(&chord, Value::F64(3.0)));
    assert_eq!(root.live_controls, [0; 3]);
    assert!(root.value.get("gain").is_none());
}
