use rustel_audio::{ScalarBackend, render_pcm};
use serde_json::{Value, json};

const SAMPLE_RATE: u32 = 48_000;

fn render(sound: &str, controls: Value) -> Vec<f32> {
    let mut value = json!({"s": sound, "note": "c3", "gain": 0.5});
    value
        .as_object_mut()
        .unwrap()
        .extend(controls.as_object().unwrap().clone());
    let event = rustel_voice::resolve_voice(&value, 1, 0.5, 0.0, SAMPLE_RATE, 0.5)
        .expect("native voice resolves");
    let pcm = render_pcm(
        &mut ScalarBackend::new(),
        SAMPLE_RATE,
        SAMPLE_RATE as usize / 2,
        &[event],
    )
    .expect("native voice renders");
    assert!(pcm.iter().all(|sample| sample.is_finite()));
    pcm
}

#[test]
fn pulse_width_and_lfo_controls_change_the_rendered_sound() {
    let fixed = render("pulse", json!({}));
    assert!(fixed.iter().any(|sample| sample.abs() > 0.001));
    assert_eq!(fixed, render("pulse", json!({"pw": 0.5})));
    assert_ne!(fixed, render("pulse", json!({"pw": 0.2})));
    let rate_only = render("pulse", json!({"pwrate": 2}));
    assert_ne!(fixed, rate_only);
    assert_eq!(
        rate_only,
        render("pulse", json!({"pwrate": 2, "pwsweep": 0.3}))
    );
    let sweep_only = render("pulse", json!({"pwsweep": 0.6}));
    assert_ne!(fixed, sweep_only);
    assert_eq!(
        sweep_only,
        render("pulse", json!({"pwrate": 1, "pwsweep": 0.6}))
    );
    assert_eq!(fixed, render("pulse", json!({"pwrate": 2, "pwsweep": 0})));
    assert_eq!(
        render("pulse", json!({"pw": 0.99})),
        render("pulse", json!({"pw": 2}))
    );
    assert_eq!(
        render("pulse", json!({"pw": -0.99})),
        render("pulse", json!({"pw": -2}))
    );
}

#[test]
fn pulse_controls_leave_other_source_families_unchanged() {
    for sound in [
        "sine", "square", "supersaw", "white", "crackle", "sbd", "bd",
    ] {
        assert_eq!(
            render(sound, json!({})),
            render(sound, json!({"pw": 0.2, "pwrate": 2, "pwsweep": 0.6})),
            "pulse controls changed {sound}",
        );
    }
}

#[test]
fn density_changes_crackle_impulses_without_changing_other_sources() {
    let silent = render("crackle", json!({"density": 0}));
    assert!(silent.iter().all(|sample| *sample == 0.0));
    assert_eq!(
        render("crackle", json!({})),
        render("crackle", json!({"density": 0.02}))
    );
    let sparse = render("crackle", json!({"density": 1}));
    let dense = render("crackle", json!({"density": 10}));
    let audible = |pcm: &[f32]| pcm.iter().filter(|sample| sample.abs() > 0.0001).count();
    assert!(audible(&sparse) > 0);
    assert!(audible(&dense) > audible(&sparse) * 4);
    for sound in [
        "white", "pink", "brown", "sine", "supersaw", "pulse", "sbd", "bd",
    ] {
        assert_eq!(
            render(sound, json!({})),
            render(sound, json!({"density": 0})),
            "density changed {sound}",
        );
    }
}
