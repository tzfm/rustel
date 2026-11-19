use rustel_core::{Hap, Pattern, Value, controls::default_control_registry, pure};
use rustel_fraction::Fraction;

fn control(pattern: &Pattern, name: &str, value: Pattern) -> Pattern {
    default_control_registry()
        .get(name)
        .expect("the control is registered")
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
    assert_eq!(hap.live_controls, [41, 42, 43, 0]);
    assert_eq!(hap.ui_visuals_context(), 8);
}

/// The direct FM route's index is the fourth live binding, under the
/// control's canonical name: `fm`, `fmi` and `fmi1` all reach it, and the
/// chain's other operators do not.
#[test]
fn the_fm_index_binds_live_under_the_direct_route_names() {
    let depth = pure(Value::F64(2.0)).with_slider_binding(44);
    for name in ["fm", "fmi", "fmi1"] {
        let hap = query(control(&sound(), name, depth.clone()));
        assert_eq!(hap.live_controls, [0, 0, 0, 44], "{name} binds");
    }
    assert_eq!(
        query(control(&sound(), "fmi2", depth)).live_controls,
        [0, 0, 0, 0],
        "operator 2's index stays query-time"
    );
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
    assert_eq!(query(overwritten).live_controls, [0, 42, 43, 0]);
    assert_eq!(
        query(control(&pattern, "lpf", pure(Value::F64(800.0)))).live_controls,
        [41, 0, 43, 0]
    );
    assert_eq!(
        query(control(&pattern, "lpq", pure(Value::F64(4.0)))).live_controls,
        [41, 42, 0, 0]
    );
}

#[test]
fn arbitrary_maps_drop_bindings_even_when_the_result_is_equal() {
    let raw = pure(Value::F64(0.5)).with_slider_binding(41);
    let mapped = control(&sound(), "gain", raw.fmap(Clone::clone));
    assert_eq!(query(mapped).live_controls, [0; 4]);
    let bound = control(&sound(), "gain", raw);
    assert_eq!(query(bound.fmap(Clone::clone)).live_controls, [0; 4]);
    assert_eq!(
        query(bound.map_haps_native(|hap| Some(hap.clone()))).live_controls,
        [0; 4]
    );
}

#[test]
fn trusted_host_maps_preserve_only_unchanged_controls_and_keep_callback_ownership() {
    struct Host;
    impl rustel_core::CallbackHost for Host {
        fn call_value(&self, id: usize, value: &Value) -> Result<Value, String> {
            let mut value = value.clone();
            if id == 8 {
                if let Value::Object(controls) = &mut value {
                    controls.insert("gain".into(), Value::F64(0.75));
                } else {
                    value = Value::F64(0.75);
                }
            }
            Ok(value)
        }

        fn call_query(&self, _: usize, _: &rustel_core::State) -> Result<Vec<Hap>, String> {
            unreachable!("this fixture only maps values")
        }
    }
    let raw = pure(Value::F64(0.5)).with_slider_binding(41);
    let bound = control(
        &control(&sound(), "gain", raw.clone()),
        "lpf",
        pure(Value::F64(800.0)).with_slider_binding(42),
    );
    let mapped = bound.fmap_js_preserving_controls(7);
    assert!(mapped.purity().impure);
    assert_eq!(mapped.purity().reachable, [7]);
    rustel_core::with_callback_host(&Host, || {
        assert_eq!(query(mapped).live_controls, [41, 42, 0, 0]);
        assert_eq!(
            query(bound.fmap_js_preserving_controls(8)).live_controls,
            [0, 42, 0, 0]
        );
        assert_eq!(query(bound.fmap_js(7)).live_controls, [0; 4]);
        assert_eq!(
            query(control(
                &sound(),
                "gain",
                raw.fmap_js_preserving_controls(7)
            ))
            .live_controls,
            [41, 0, 0, 0]
        );
        assert_eq!(
            query(control(
                &sound(),
                "gain",
                raw.fmap_js_preserving_controls(8)
            ))
            .live_controls,
            [0; 4]
        );
    });
}

#[test]
fn native_outer_control_bind_preserves_timing_and_tracks_both_dependencies() {
    let receiver = control(
        &sound(),
        "gain",
        pure(Value::F64(0.5)).with_slider_binding(41),
    )
    .slow(Fraction::from(2));
    let mapped = receiver.outer_bind_controls(
        pure(Value::F64(12.0)).fast(Fraction::from(2)),
        |current, input| {
            let Value::Object(mut controls) = current.clone() else {
                unreachable!()
            };
            controls.insert("extra".into(), input.clone());
            Value::Object(controls)
        },
    );
    let haps = mapped.query_arc(Fraction::ZERO, Fraction::ONE);
    assert_eq!(haps.len(), 2);
    for hap in haps {
        assert_eq!(hap.live_controls, [41, 0, 0, 0]);
        assert_eq!(hap.whole.unwrap().end, Fraction::from(2));
        assert_eq!(hap.part.duration(), Fraction::new(1, 2));
    }
    let callbacks = receiver
        .fmap_js(7)
        .outer_bind_controls(pure(Value::F64(12.0)).fmap_js(8), |current, _| {
            current.clone()
        });
    assert!(callbacks.purity().impure);
    assert_eq!(callbacks.purity().reachable, [7, 8]);
}

#[test]
fn note_sample_index_and_timing_values_do_not_become_audio_bindings() {
    let raw = pure(Value::F64(2.0)).with_slider_binding(41);
    for name in ["note", "n", "speed", "pan"] {
        assert_eq!(
            query(control(&sound(), name, raw.clone())).live_controls,
            [0; 4]
        );
    }
    let pattern = control(&sound().fast(Fraction::from(2)), "gain", raw);
    assert_eq!(query(pattern).live_controls, [41, 0, 0, 0]);
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
        assert_eq!(hap.live_controls, [41, 42, 43, 0]);
        assert_eq!(hap.value.get("gain"), Some(&Value::F64(0.5)));
        assert_eq!(hap.value.get("cutoff"), Some(&Value::F64(800.0)));
        assert_eq!(hap.value.get("resonance"), Some(&Value::F64(4.0)));
    }
    let raw = pure(Value::F64(0.5)).with_slider_binding(41);
    let transformed = transpose(&raw, Value::F64(12.0));
    assert_eq!(
        query(control(&sound(), "gain", transformed)).live_controls,
        [0; 4]
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
        assert_eq!(query(pattern).live_controls, [41, 42, 43, 0]);
    }
    let chord = bound_pitch(Value::object([("chord".into(), Value::Str("C".into()))]));
    let voices = voicings::voicing(&chord).query_arc(Fraction::ZERO, Fraction::ONE);
    assert!(voices.len() > 1);
    assert!(
        voices
            .iter()
            .all(|hap| hap.live_controls == [41, 42, 43, 0])
    );
    // rootNotes deliberately returns just the note, removing audio controls.
    let root = query(voicings::root_notes(&chord, Value::F64(3.0)));
    assert_eq!(root.live_controls, [0; 4]);
    assert!(root.value.get("gain").is_none());
}
