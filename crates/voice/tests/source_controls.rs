use rustel_audio::{DecodedSample, OnsetEvent, SampleId, ScalarBackend, render_pcm};
use rustel_voice::{SampleLookup, SampleResolution, resolve_voice_with_samples};
use serde_json::{Value, json};

const SAMPLE_RATE: u32 = 8_000;
const SAMPLE: SampleId = SampleId(1);
const TABLE: SampleId = SampleId(2);

struct Samples;

impl SampleLookup for Samples {
    fn resolve(&self, name: &str, _index: f64, midi: f64) -> SampleResolution {
        let (id, soundfont, loop_secs) = match name {
            "reference_sample" => (SAMPLE, false, None),
            "gm_reference" => (SAMPLE, true, None),
            "gm_looped" => (SAMPLE, true, Some((0.025, 0.075))),
            "wt_reference" => (TABLE, false, None),
            _ => return SampleResolution::Unknown,
        };
        SampleResolution::Found {
            id,
            transpose: midi - 60.0,
            duration_secs: 0.25,
            loop_secs,
            envelope_peak: if soundfont { 0.3 } else { 1.0 },
            soundfont,
        }
    }
}

fn value(sound: &str, controls: Value) -> Value {
    let mut value = json!({"s": sound, "note": 60});
    value
        .as_object_mut()
        .unwrap()
        .extend(controls.as_object().unwrap().clone());
    value
}

fn resolve(value: &Value, onset: f64, cps: f64) -> OnsetEvent {
    resolve_voice_with_samples(value, 1, 0.4, onset, SAMPLE_RATE, cps, &Samples)
        .expect("reference controls resolve")
}

fn render_events(events: &[OnsetEvent]) -> Vec<f32> {
    let mut backend = ScalarBackend::default();
    let pcm = (0..SAMPLE_RATE / 4)
        .map(|frame| {
            let phase = frame as f32 / SAMPLE_RATE as f32;
            (phase * 220.0 * std::f32::consts::TAU).sin() * (0.2 + phase)
        })
        .collect();
    backend
        .install_sample(
            SAMPLE,
            Box::new(DecodedSample::from_parts(SAMPLE_RATE, 1, pcm).unwrap()),
        )
        .unwrap();
    let table = (0..4096)
        .map(|frame| {
            let phase = (frame % 2048) as f32 / 2048.0;
            if frame < 2048 {
                (phase * std::f32::consts::TAU).sin() * 0.4
            } else {
                (phase * std::f32::consts::TAU * 3.0).sin() * 0.4
            }
        })
        .collect();
    backend
        .install_sample(
            TABLE,
            Box::new(DecodedSample::from_parts(SAMPLE_RATE, 1, table).unwrap()),
        )
        .unwrap();
    render_pcm(&mut backend, SAMPLE_RATE, SAMPLE_RATE as usize / 2, events)
        .expect("reference controls render")
}

fn render(sound: &str, controls: Value) -> Vec<f32> {
    render_events(&[resolve(&value(sound, controls), 0.0, 0.5)])
}

fn audible(pcm: &[f32]) -> bool {
    pcm.iter().any(|sample| sample.abs() > 0.0001)
}

#[test]
fn sampler_controls_change_samples_but_not_soundfonts_or_synths() {
    let sample = render("reference_sample", json!({}));
    assert!(audible(&sample));
    for controls in [
        json!({"speed": 2}),
        json!({"speed": -1}),
        json!({"begin": 0.25}),
        json!({"end": 0.5}),
        json!({"nudge": 0.025}),
        json!({"unit": "c"}),
    ] {
        assert!(
            render("reference_sample", controls.clone()) != sample,
            "sample ignores {controls}"
        );
        for sound in ["gm_reference", "sine", "wt_reference"] {
            assert!(
                render(sound, controls.clone()) == render(sound, json!({})),
                "{sound} consumes sampler control {controls}"
            );
        }
    }
    assert!(!audible(&render("reference_sample", json!({"speed": 0}))));
    assert!(audible(&render("gm_reference", json!({"speed": 0}))));
    let fast = render("reference_sample", json!({"speed": 2}));
    let tail = SAMPLE_RATE as usize * 2 / 5;
    assert!(audible(&sample[tail..]));
    assert!(!audible(&fast[tail..]));
}

#[test]
fn sample_unit_c_uses_file_seconds_and_s_keeps_the_ordinary_rate() {
    let ordinary = value("reference_sample", json!({}));
    let seconds = value("reference_sample", json!({"unit": "s"}));
    let cycle = value("reference_sample", json!({"unit": "c"}));
    for cps in [0.5, 2.0] {
        let rate = |value| resolve(value, 0.0, cps).sample.unwrap().playback_rate;
        assert_eq!(rate(&ordinary), 1.0);
        assert_eq!(rate(&seconds), 1.0);
        assert_eq!(rate(&cycle), 0.25);
    }
    assert!(
        render_events(&[resolve(&cycle, 0.0, 0.5)]) == render_events(&[resolve(&cycle, 0.0, 2.0)])
    );
}

#[test]
fn cut_groups_choke_samples_but_do_not_reach_soundfont_zones() {
    for sound in ["reference_sample", "gm_reference", "sine"] {
        let pair = |cut| {
            let first = resolve(&value(sound, json!({"cut": cut})), 0.0, 0.5);
            let second = resolve(&value(sound, json!({"cut": cut})), 0.05, 0.5);
            render_events(&[first, second])
        };
        assert_eq!(
            pair(json!(1)) != pair(Value::Null),
            sound == "reference_sample"
        );
    }
}

#[test]
fn loop_points_need_looping_and_do_not_replace_a_soundfont_loop() {
    let points = json!({"loopBegin": 0.1, "loopEnd": 0.3});
    assert!(render("reference_sample", points.clone()) == render("reference_sample", json!({})));
    let looping = json!({"loop": 1, "loopBegin": 0.1, "loopEnd": 0.3});
    assert!(render("reference_sample", looping.clone()) != render("reference_sample", json!({})));
    assert!(render("gm_looped", looping) == render("gm_looped", json!({})));
    assert!(render("gm_looped", json!({"loop": 0})) == render("gm_looped", json!({})));
    assert!(render("wt_reference", points) == render("wt_reference", json!({})));
}

#[test]
fn wavetable_position_envelopes_and_lfos_have_explicit_activation_rules() {
    let plain = render("wt_reference", json!({}));
    assert!(audible(&plain));
    for controls in [
        json!({"wt": 0.75}),
        json!({"wtattack": 0.05}),
        json!({"wtrate": 3}),
    ] {
        assert!(
            render("wt_reference", controls.clone()) != plain,
            "{controls}"
        );
        for sound in ["reference_sample", "gm_reference", "sine"] {
            assert!(
                render(sound, controls.clone()) == render(sound, json!({})),
                "{sound}: {controls}"
            );
        }
    }
    for controls in [
        json!({"wtattack": 0.05, "wtenv": 0}),
        json!({"wtrate": 3, "wtdepth": 0}),
        json!({"wtdc": 0.5}),
    ] {
        assert!(
            render("wt_reference", controls.clone()) == plain,
            "{controls}"
        );
    }
    assert!(render("wt_reference", json!({"wtdc": 0.5, "wtdepth": 0.5})) != plain);
    let controls = resolve(
        &value("wt_reference", json!({"wtrate": 3, "wtsync": 2})),
        0.0,
        0.5,
    )
    .wavetable
    .unwrap();
    assert_eq!(controls.lfo_rate, 1.0);
    assert_eq!(controls.lfo_depth, 0.5);
}

#[test]
fn wavetable_warp_needs_a_mode_and_ignores_other_source_families() {
    let plain = render("wt_reference", json!({}));
    assert!(render("wt_reference", json!({"warp": 0.5})) == plain);
    assert!(render("wt_reference", json!({"warpmode": "asym", "warp": 0})) != plain);
    for controls in [
        json!({"warpmode": "sync", "warp": 0.5}),
        json!({"warpmode": "sync", "warpattack": 0.05}),
        json!({"warpmode": "sync", "warprate": 3}),
    ] {
        assert!(
            render("wt_reference", controls.clone()) != plain,
            "{controls}"
        );
        for sound in ["reference_sample", "gm_reference", "sine"] {
            assert!(
                render(sound, controls.clone()) == render(sound, json!({})),
                "{sound}: {controls}"
            );
        }
    }
    let inactive = json!({"warpmode": "sync", "warpattack": 0.05, "warpenv": 0, "warprate": 3, "warpdepth": 0});
    assert!(render("wt_reference", inactive) == plain);
}

#[test]
fn wavetable_envelope_defaults_depend_on_which_adsr_fields_are_present() {
    let controls = |fields| {
        resolve(&value("wt_reference", fields), 0.0, 0.5)
            .wavetable
            .unwrap()
    };
    let plain = controls(json!({}));
    assert_eq!(plain.pos_env_amount, 0.0);
    assert_eq!(plain.warp_env_amount, 0.0);
    let amount = controls(json!({"wtenv": 0.75}));
    assert_eq!(
        (
            amount.pos_attack,
            amount.pos_decay,
            amount.pos_sustain,
            amount.pos_release
        ),
        (0.0, 0.5, 0.0, 0.1)
    );
    let attack = controls(json!({"wtattack": 0.05}));
    assert_eq!(attack.pos_env_amount, 0.5);
    assert_eq!(
        (
            attack.pos_attack,
            attack.pos_decay,
            attack.pos_sustain,
            attack.pos_release
        ),
        (0.05, 0.001, 1.0, 0.01)
    );
    let decay = controls(json!({"warpdecay": 0.05}));
    assert_eq!(decay.warp_env_amount, 0.5);
    assert_eq!(
        (
            decay.warp_attack,
            decay.warp_decay,
            decay.warp_sustain,
            decay.warp_release
        ),
        (0.001, 0.05, 0.001, 0.01)
    );
}

#[test]
fn wavetable_phase_randomization_is_a_switch() {
    let enabled = render("wt_reference", json!({"wtphaserand": 1}));
    assert!(enabled == render("wt_reference", json!({"wtphaserand": 0.2})));
    assert!(enabled != render("wt_reference", json!({"wtphaserand": 0})));
}

#[test]
fn unison_detune_and_spread_do_not_change_samples_soundfonts_or_basic_oscillators() {
    for sound in ["reference_sample", "gm_reference", "sine", "sawtooth"] {
        let plain = render(sound, json!({}));
        assert!(audible(&plain), "silent fixture: {sound}");
        for controls in [
            json!({"unison": 7}),
            json!({"detune": 12}),
            json!({"spread": 1}),
            json!({"unison": 7, "detune": 12, "spread": 1}),
        ] {
            assert!(
                render(sound, controls.clone()) == plain,
                "{sound} consumes unsupported unison controls: {controls}"
            );
        }
    }
}

#[test]
fn pink_noise_mix_only_reaches_basic_oscillators() {
    for sound in [
        "sine",
        "sawtooth",
        "supersaw",
        "pulse",
        "bytebeat",
        "wt_reference",
        "reference_sample",
        "gm_reference",
        "z_sine",
    ] {
        let plain = render(sound, json!({}));
        assert!(audible(&plain), "silent fixture: {sound}");
        assert_eq!(
            render(sound, json!({"noise": 0.5})) != plain,
            matches!(sound, "sine" | "sawtooth"),
            "{sound}"
        );
    }
}

#[test]
fn vibrato_and_pitch_envelopes_reach_sample_pitch_but_not_bytebeat_or_zzfx() {
    for sound in [
        "sine",
        "supersaw",
        "pulse",
        "wt_reference",
        "reference_sample",
        "gm_reference",
        "bytebeat",
        "z_sine",
    ] {
        let plain = render(sound, json!({}));
        for controls in [json!({"vib": 5, "vibmod": 3}), json!({"penv": 12})] {
            assert_eq!(
                render(sound, controls.clone()) != plain,
                !matches!(sound, "bytebeat" | "z_sine"),
                "{sound}: {controls}"
            );
        }
    }
    let plain = render("sine", json!({}));
    for controls in [
        json!({"vibmod": 3}),
        json!({"panchor": 0}),
        json!({"pcurve": 1}),
    ] {
        assert!(render("sine", controls.clone()) == plain, "{controls}");
    }
    assert!(render("sbd", json!({"vib": 5})) == render("sbd", json!({})));
    assert!(render("sbd", json!({"pdecay": 0.1})) != render("sbd", json!({})));
}

#[test]
fn bytebeat_expression_and_counter_offset_do_not_change_other_families() {
    for controls in [
        json!({"byteBeatExpression": "t"}),
        json!({"byteBeatStartTime": 1000}),
    ] {
        for sound in [
            "bytebeat",
            "sine",
            "wt_reference",
            "reference_sample",
            "gm_reference",
        ] {
            assert_eq!(
                render(sound, controls.clone()) != render(sound, json!({})),
                sound == "bytebeat",
                "{sound}: {controls}"
            );
        }
    }
}

#[test]
fn stretch_processes_oscillators_and_soundfonts_as_well_as_samples() {
    for sound in ["sine", "wt_reference", "reference_sample", "gm_reference"] {
        let shifted = render(sound, json!({"note": 48, "stretch": 1}));
        assert!(audible(&shifted), "silent shifted fixture: {sound}");
        assert!(shifted != render(sound, json!({"note": 48})), "{sound}");
    }
}

#[test]
fn n_selects_source_variants_without_becoming_a_general_pitch_control() {
    for sound in ["sawtooth", "bytebeat", "sine", "pulse", "z_sine"] {
        assert_eq!(
            render(sound, json!({"n": 1})) != render(sound, json!({})),
            matches!(sound, "sawtooth" | "bytebeat"),
            "{sound}"
        );
    }
    let input = resolve(&value("in", json!({"n": 2})), 0.0, 0.5);
    assert!(matches!(
        input.synth,
        Some(rustel_audio::SynthSource::Input { channel: 2 })
    ));
    let bus = resolve(&value("bus", json!({"n": 2})), 0.0, 0.5);
    assert!(matches!(
        bus.synth,
        Some(rustel_audio::SynthSource::Bus { bus: 2 })
    ));
}

#[test]
fn frequency_overrides_note_and_octave_only_reaches_its_supported_sources() {
    for sound in [
        "sine",
        "pulse",
        "bytebeat",
        "sbd",
        "wt_reference",
        "reference_sample",
        "gm_reference",
        "z_sine",
    ] {
        assert!(
            render(sound, json!({"note": 57})) == render(sound, json!({"freq": 220})),
            "frequency precedence: {sound}"
        );
        assert_eq!(
            render(sound, json!({"octave": 1})) != render(sound, json!({})),
            !matches!(sound, "reference_sample" | "gm_reference" | "z_sine"),
            "octave: {sound}"
        );
    }
    assert!(render("white", json!({"freq": 220, "octave": 1})) == render("white", json!({})));
}

#[test]
fn amplitude_defaults_follow_the_source_and_sbd_uses_its_own_envelope() {
    let env = |sound, fields| resolve(&value(sound, fields), 0.0, 0.5).controls.envelope;
    assert_eq!(env("sine", json!({})), rustel_audio::Envelope::default());
    for sound in ["reference_sample", "gm_reference", "wt_reference"] {
        let envelope = env(sound, json!({}));
        assert_eq!(
            (
                envelope.attack_secs,
                envelope.decay_secs,
                envelope.sustain,
                envelope.release_secs
            ),
            (0.001, 0.001, 1.0, 0.01)
        );
        let partial = env(sound, json!({"decay": 0.05}));
        assert_eq!(partial.sustain, 0.001);
        assert!(
            render(sound, json!({"attack": 0.1})) != render(sound, json!({})),
            "{sound}"
        );
    }
    for fields in [
        json!({"attack": 0.1}),
        json!({"sustain": 0.1}),
        json!({"release": 0.2}),
    ] {
        assert!(
            render("sbd", fields.clone()) == render("sbd", json!({})),
            "{fields}"
        );
    }
    assert!(render("sbd", json!({"decay": 0.1})) != render("sbd", json!({})));
    let zzfx = resolve(&value("z_sine", json!({})), 0.0, 0.5);
    let Some(rustel_audio::SynthSource::ZzFx { params }) = zzfx.synth else {
        panic!("ZZFX source")
    };
    assert_eq!(
        (
            params.attack,
            params.decay,
            params.sustain_volume,
            params.release
        ),
        (0.0, 0.0, 0.8, 0.1)
    );
    assert!(render("z_sine", json!({"attack": 0.1})) != render("z_sine", json!({})));
}

#[test]
fn shared_effect_secondary_controls_need_their_enabling_control() {
    for sound in ["sine", "gm_reference"] {
        let plain = render(sound, json!({}));
        for fields in [
            json!({"delaytime": 0.03, "delayfeedback": 0.8}),
            json!({"phaserdepth": 0.5, "phasercenter": 600}),
            json!({"tremolodepth": 0.8, "tremoloshape": "sine"}),
            json!({"transsustain": 0.8}),
            json!({"distortvol": 0.5, "distorttype": "diode"}),
            json!({"compressorRatio": 20}),
            json!({"roomsize": 0.1, "roomfade": 0.01}),
            json!({"resonance": 8, "lpenv": 2, "lprate": 3}),
        ] {
            assert!(render(sound, fields.clone()) == plain, "{sound}: {fields}");
        }
        for fields in [
            json!({"delay": 0.5, "delaytime": 0.03, "delayfeedback": 0.8}),
            json!({"phaserrate": 3, "phaserdepth": 0.5, "phasercenter": 600}),
            json!({"tremolo": 3, "tremolodepth": 0.8, "tremoloshape": "sine"}),
            json!({"transient": 0, "transsustain": 0.8}),
            json!({"distort": 3, "distortvol": 0.5, "distorttype": "diode"}),
            json!({"compressor": -40, "compressorRatio": 20}),
            json!({"room": 0.5, "roomsize": 0.1, "roomfade": 0.01}),
            json!({"cutoff": 600, "resonance": 8, "lpenv": 2, "lprate": 3}),
        ] {
            assert!(render(sound, fields.clone()) != plain, "{sound}: {fields}");
        }
    }
}

#[test]
fn delay_feedback_zero_disables_the_send_and_positive_feedback_is_bounded() {
    assert!(render("sine", json!({"delay": 1, "delayfeedback": 0})) == render("sine", json!({})));
    let event = resolve(
        &value(
            "sine",
            json!({"delay": 1, "delaytime": 2, "delayfeedback": 2}),
        ),
        0.0,
        0.5,
    );
    let delay = event.controls.delay.unwrap();
    assert_eq!(delay.time_secs, 1.0);
    assert_eq!(delay.feedback, 0.98);
}

#[test]
fn filter_lfos_require_a_cutoff_and_explicit_hertz_depth_overrides_relative_depth() {
    for (cutoff, prefix) in [("cutoff", "lp"), ("hcutoff", "hp"), ("bandf", "bp")] {
        let mut fields = json!({});
        fields[cutoff] = json!(600);
        let plain = render("sawtooth", fields.clone());
        fields[format!("{prefix}dc")] = json!(0.25);
        assert!(render("sawtooth", fields.clone()) == plain);
        fields[format!("{prefix}depth")] = json!(0.5);
        assert!(render("sawtooth", fields.clone()) != plain);
        fields[format!("{prefix}depthfrequency")] = json!(300);
        fields[format!("{prefix}depth")] = json!(9);
        fields[format!("{prefix}rate")] = json!(8);
        fields[format!("{prefix}sync")] = json!(2);
        let event = resolve(&value("sawtooth", fields), 0.0, 0.5);
        let lfo = event.controls.lfos.into_iter().flatten().next().unwrap();
        assert_eq!(lfo.depth, 300.0);
        assert_eq!(lfo.frequency_hz, 1.0);
    }
}

#[test]
fn drive_requires_an_enabled_ladder_filter() {
    let plain = render("sawtooth", json!({}));
    assert!(render("sawtooth", json!({"drive": 3, "ftype": "ladder"})) == plain);
    let biquad = render("sawtooth", json!({"cutoff": 600}));
    assert!(render("sawtooth", json!({"cutoff": 600, "drive": 3})) == biquad);
    assert!(
        render(
            "sawtooth",
            json!({"cutoff": 600, "ftype": "ladder", "drive": 3})
        ) != render("sawtooth", json!({"cutoff": 600, "ftype": "ladder"}))
    );
}

#[test]
fn generated_reverb_damping_does_not_modify_a_loaded_custom_response() {
    for fields in [
        json!({"roomlp": 400}),
        json!({"roomdim": 300}),
        json!({"roomfade": 0.2}),
    ] {
        let mut generated = json!({"room": 1, "roomsize": 0.25, "dry": 0});
        let plain = render("sine", generated.clone());
        assert!(audible(&plain));
        generated
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(render("sine", generated) != plain, "{fields}");
        let mut custom = json!({"room": 1, "roomsize": 0.25, "dry": 0, "ir": "reference_sample"});
        let plain = render("sine", custom.clone());
        assert!(audible(&plain));
        custom
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(render("sine", custom) == plain, "{fields}");
    }
}

#[test]
fn native_gain_is_linear_for_synths_and_soundfont_zones() {
    for sound in ["sine", "gm_reference"] {
        let unity = render(sound, json!({"gain": 1}));
        let half = render(sound, json!({"gain": 0.5}));
        assert!(audible(&unity));
        assert!(
            unity
                .iter()
                .zip(half)
                .all(|(full, half)| (full * 0.5 - half).abs() < 1e-6),
            "{sound}"
        );
    }
}

#[test]
fn fm_routes_reach_oscillators_but_not_other_source_families() {
    for sound in [
        "sine",
        "supersaw",
        "pulse",
        "bytebeat",
        "reference_sample",
        "gm_reference",
        "wt_reference",
        "z_sine",
        "sbd",
        "white",
    ] {
        let plain = render(sound, json!({}));
        assert!(audible(&plain), "{sound}");
        let secondary = json!({"fmh": 2, "fmwave": "square", "fmattack": 0.05});
        assert!(render(sound, secondary) == plain, "no route: {sound}");
        assert_eq!(
            render(
                sound,
                json!({"fmi": 2, "fmh": 2, "fmwave": "square", "fmattack": 0.05})
            ) != plain,
            matches!(sound, "sine" | "supersaw" | "pulse" | "bytebeat"),
            "FM support: {sound}"
        );
    }
}

#[test]
fn fm_depth_envelope_needs_adsr_and_defaults_depend_on_decay() {
    let plain = render("sine", json!({"fmi": 2}));
    assert!(render("sine", json!({"fmi": 2, "fmenv": "lin"})) == plain);
    let operator = |fields| {
        resolve(&value("sine", fields), 0.0, 0.5)
            .controls
            .fm
            .unwrap()
            .operators[0]
            .unwrap()
    };
    let flat = operator(json!({"fmi": 2}));
    assert_eq!(flat.harmonicity, 1.0);
    assert_eq!(flat.waveform, rustel_audio::FmWave::Sine);
    assert!(flat.env.is_none());
    assert!(flat.env_exponential);
    let attack = operator(json!({"fmi": 2, "fmattack": 0.05}));
    let env = attack.env.unwrap();
    assert_eq!(
        (
            env.attack_secs,
            env.decay_secs,
            env.sustain,
            env.release_secs
        ),
        (0.05, 0.001, 1.0, 0.01)
    );
    let decay = operator(json!({"fmi": 2, "fmdecay": 0.05, "fmenv": "linear"}));
    assert_eq!(decay.env.unwrap().sustain, 0.001);
    assert!(!decay.env_exponential);
    let exponential = render("sine", json!({"fmi": 2, "fmattack": 0.05}));
    assert!(exponential != plain);
    assert!(render("sine", json!({"fmi": 2, "fmattack": 0.05, "fmenv": "lin"})) != exponential);
}

#[test]
fn explicit_clip_gates_samples_and_soundfonts_to_the_effective_event_duration() {
    // The scheduler has already converted duration/clip from musical cycles
    // to seconds at this boundary. Only a hold-setting control makes a sample
    // use those seconds instead of its complete slice.
    for sound in ["reference_sample", "gm_reference"] {
        let event = |fields| {
            resolve_voice_with_samples(
                &value(sound, fields),
                1,
                0.05,
                0.0,
                SAMPLE_RATE,
                0.5,
                &Samples,
            )
            .unwrap()
        };
        let slice = event(json!({"duration": 0.025}));
        assert_eq!(slice.sample.unwrap().hold, rustel_audio::SampleHold::Slice);
        let clipped = event(json!({"duration": 0.025, "clip": 1}));
        assert_eq!(clipped.sample.unwrap().hold, rustel_audio::SampleHold::Hap);
        let tail = SAMPLE_RATE as usize / 5;
        assert!(audible(&render_events(&[slice])[tail..]), "{sound}");
        assert!(!audible(&render_events(&[clipped])[tail..]), "{sound}");
    }
    let looped = resolve(&value("gm_looped", json!({"duration": 0.025})), 0.0, 0.5);
    assert_eq!(looped.sample.unwrap().hold, rustel_audio::SampleHold::Hap);
}
