//! The control-to-voice resolver shared by every host.
//!
//! One mapping from a hap's value map to a DSP onset event: the CLI/desktop
//! runtime's render and live paths both call THIS code, so "the same pattern
//! sounds the same offline and on a device" is a property of the build rather
//! than something a test has to keep rediscovering.
//!
//! Unsupported sounds and controls return explicit errors rather than
//! plausible-sounding approximations.

#[cfg(test)]
mod classification_tests {
    //! Native resolver policy tests; no scheduler, callback, or JavaScript execution.

    use super::{
        BundledOnly, SampleLookup, SampleResolution, VoiceError, resolve_voice_with_samples,
        resolve_voice_with_samples_detailed,
    };
    use serde_json::{Value, json};

    #[derive(Clone, Copy)]
    enum Lookup {
        Found,
        Loading,
        Failed,
        Unknown,
    }

    impl SampleLookup for Lookup {
        fn resolve(&self, _sound: &str, _index: f64, _midi: f64) -> SampleResolution {
            match self {
                Self::Found => SampleResolution::Found {
                    id: rustel_audio::SampleId(7),
                    transpose: 12.0,
                    duration_secs: 2.0,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    soundfont: false,
                },
                Self::Loading => SampleResolution::Loading,
                Self::Failed => SampleResolution::Failed,
                Self::Unknown => SampleResolution::Unknown,
            }
        }
    }

    fn detailed(
        value: &Value,
        lookup: &dyn SampleLookup,
    ) -> Result<rustel_audio::OnsetEvent, VoiceError> {
        resolve_voice_with_samples_detailed(value, 9, 0.25, 0.125, 0.0625, 48_000, 0.5, lookup)
    }

    fn refusal(value: &Value, lookup: &dyn SampleLookup) -> VoiceError {
        let error = detailed(value, lookup).expect_err("fixture must refuse conversion");
        let legacy = resolve_voice_with_samples(value, 9, 0.25, 0.125, 48_000, 0.5, lookup)
            .expect_err("the String compatibility wrapper must also refuse");
        assert_eq!(
            error.to_string(),
            legacy,
            "classification changed the diagnostic"
        );
        error
    }

    #[test]
    fn supersaw_unison_ceiling_is_an_invalid_control() {
        let event = detailed(&json!({ "s": "supersaw", "unison": 32 }), &BundledOnly)
            .expect("32 voices remain supported");
        assert!(matches!(
            event.synth,
            Some(rustel_audio::SynthSource::Supersaw { voices, .. }) if voices == 32.0
        ));
        for voices in [33, 100] {
            assert_eq!(
                refusal(&json!({ "s": "supersaw", "unison": voices }), &BundledOnly),
                VoiceError::InvalidControl(format!(
                    "unison {voices} exceeds the native supersaw's 32-voice ceiling"
                ))
            );
        }
    }

    #[test]
    fn wavetable_unison_ceiling_is_checked_after_the_asset_resolves() {
        let value = json!({ "s": "wt_fixture", "unison": 32 });
        let event = detailed(&value, &Lookup::Found).expect("32 table voices remain supported");
        let wavetable = event.wavetable.expect("the found asset is a wavetable");
        assert_eq!(wavetable.table, rustel_audio::SampleId(7));
        assert_eq!(wavetable.voices, 32.0);
        assert!(event.sample.is_none());
        for voices in [33, 100] {
            assert_eq!(
                refusal(
                    &json!({ "s": "wt_fixture", "unison": voices }),
                    &Lookup::Found
                ),
                VoiceError::InvalidControl(format!(
                    "unison {voices} exceeds the native wavetable's 32-voice ceiling"
                ))
            );
        }
        let invalid_unison = json!({ "s": "wt_fixture", "unison": 100 });
        assert!(matches!(
            refusal(&invalid_unison, &Lookup::Loading),
            VoiceError::SampleLoading(_)
        ));
        assert!(matches!(
            refusal(&invalid_unison, &Lookup::Failed),
            VoiceError::SampleFailed(_)
        ));
        assert!(matches!(
            refusal(&invalid_unison, &Lookup::Unknown),
            VoiceError::UnknownSound(_)
        ));
    }

    /// An unknown banked sound includes the bank name in its error. This identifies
    /// which bank lacks the sound when a score selects more than one bank.
    #[test]
    fn an_unknown_banked_sound_is_named_with_its_bank() {
        assert_eq!(
            refusal(
                &json!({ "s": "ht", "n": 0, "bank": "BossDR110" }),
                &Lookup::Unknown
            ),
            VoiceError::UnknownSound(
                "unknown sound \"BossDR110_ht\": no sample bank or synth of that name is loaded"
                    .into()
            )
        );
        // An oscillator under a bank with no sample of that name still plays.
        detailed(
            &json!({ "s": "sine", "bank": "BossDR110" }),
            &Lookup::Unknown,
        )
        .expect("a banked oscillator name falls back to the oscillator");
    }

    #[test]
    fn input_bus_and_orbit_limits_keep_their_native_boundary() {
        let input_limit = rustel_audio::input::MAX_INPUT_CHANNELS as i64;
        let bus_limit = rustel_audio::MAX_BUSES as i64;
        for (sound, key, limit) in [
            ("in", "n", input_limit),
            ("bus", "n", bus_limit),
            ("sine", "bus", bus_limit),
            ("sine", "orbit", 16),
        ] {
            for accepted in [0, limit - 1] {
                let mut value = json!({ "s": sound });
                value[key] = json!(accepted);
                detailed(&value, &BundledOnly).expect("the endpoint remains in range");
            }
            for rejected in [-1, limit] {
                let mut value = json!({ "s": sound });
                value[key] = json!(rejected);
                let error = refusal(&value, &BundledOnly);
                assert!(
                    matches!(&error, VoiceError::InvalidControl(message) if message.contains("outside the supported")),
                    "wrong category for {value}: {error:?}"
                );
            }
        }
    }

    #[test]
    fn asset_categories_do_not_depend_on_words_inside_the_sound_name() {
        // These names deliberately contain the old runtime's loading substring.
        // Failed and unknown assets must not be mistaken for retryable loading.
        for name in ["fixture still loading", "wt_fixture still loading"] {
            let value = json!({ "s": name });
            let kind = if name.starts_with("wt_") {
                "wavetable"
            } else {
                "sample"
            };
            assert_eq!(
                refusal(&value, &Lookup::Loading),
                VoiceError::SampleLoading(format!("{kind} \"{name}:0\" is still loading"))
            );
            assert_eq!(
                refusal(&value, &Lookup::Failed),
                VoiceError::SampleFailed(format!("{kind} \"{name}:0\" failed to load"))
            );
            let unknown = if name.starts_with("wt_") {
                format!("unknown wavetable \"{name}\"")
            } else {
                format!("unknown sound {name:?}: no sample bank or synth of that name is loaded")
            };
            assert_eq!(
                refusal(&value, &Lookup::Unknown),
                VoiceError::UnknownSound(unknown)
            );
        }
        assert_eq!(
            refusal(
                &json!({ "s": "sine", "fmi": 1, "fmwave": "still loading" }),
                &BundledOnly
            ),
            VoiceError::InvalidControl("unsupported fmwave: still loading".into())
        );
    }

    #[test]
    fn native_synth_manifest_bypass_keeps_control_validation() {
        assert!(detailed(&json!({ "s": "supersaw", "unison": 32 }), &Lookup::Loading).is_ok());
        assert!(matches!(
            refusal(&json!({ "s": "supersaw", "unison": 100 }), &Lookup::Loading),
            VoiceError::InvalidControl(_)
        ));
    }

    #[test]
    fn malformed_values_slices_and_room_sizes_are_invalid_controls() {
        for value in [
            json!(60),
            json!({ "s": "sine", "gain": "not a number" }),
            json!({ "s": "sine", "room": 1, "roomsize": -1 }),
            json!({
                "s": "sine", "room": 1,
                "roomsize": rustel_audio::reverb::MAX_REVERB_SECONDS + 1.0
            }),
        ] {
            assert!(matches!(
                refusal(&value, &BundledOnly),
                VoiceError::InvalidControl(_)
            ));
        }
        assert!(matches!(
            refusal(
                &json!({ "s": "fixture", "begin": 0.75, "end": 0.25 }),
                &Lookup::Found
            ),
            VoiceError::InvalidControl(message) if message.contains("ends before it starts")
        ));
    }

    #[test]
    fn fm_route_capacity_is_an_invalid_control_not_an_asset_error() {
        let mut value = json!({ "s": "sine" });
        let mut routes = 0;
        for source in 1..=rustel_audio::MAX_FM_OPERATORS {
            for target in 0..=rustel_audio::MAX_FM_OPERATORS {
                let key = if source == target + 1 {
                    if source == 1 {
                        "fmi".to_owned()
                    } else {
                        format!("fmi{source}")
                    }
                } else {
                    format!("fmi{source}{target}")
                };
                value[key] = json!(1);
                routes += 1;
                if routes == rustel_audio::MAX_FM_ROUTES {
                    let event = detailed(&value, &BundledOnly).expect("capacity is inclusive");
                    assert_eq!(
                        event
                            .controls
                            .fm
                            .expect("FM controls")
                            .routes
                            .iter()
                            .flatten()
                            .count(),
                        routes
                    );
                } else if routes > rustel_audio::MAX_FM_ROUTES {
                    assert_eq!(
                        refusal(&value, &BundledOnly),
                        VoiceError::InvalidControl(format!(
                            "FM matrix declares more than {} connections",
                            rustel_audio::MAX_FM_ROUTES
                        ))
                    );
                    return;
                }
            }
        }
        panic!("fixture must contain more possible routes than the native capacity");
    }

    #[test]
    fn successful_conversion_keeps_voice_math_and_string_wrapper_output() {
        let value = json!({
            "s": "supersaw", "unison": 32, "n": 0.25, "spread": 0.5,
            "freq": 220, "octave": 1, "gain": 0.5, "velocity": 0.75, "orbit": 15
        });
        let event = detailed(&value, &BundledOnly).expect("valid native voice");
        let legacy = resolve_voice_with_samples(&value, 9, 0.25, 0.125, 48_000, 0.5, &BundledOnly)
            .expect("valid compatibility-wrapper voice");
        assert_eq!(event, legacy);
        assert_eq!(event.onset_frame, 6_000);
        assert_eq!(event.onset_lead, 0.0);
        assert_eq!(event.freq_hz, 440.0);
        assert_eq!(event.gain, 0.5);
        assert_eq!(event.duration_secs, 0.25);
        assert_eq!(event.controls.velocity, 0.75);
        assert_eq!(event.controls.orbit, 15);
        assert_eq!(
            event.synth,
            Some(rustel_audio::SynthSource::Supersaw {
                voices: 32.0,
                freqspread: 0.25,
                panspread: 0.5,
            })
        );

        let sample = detailed(&json!({ "s": "fixture", "speed": 0.5 }), &Lookup::Found)
            .expect("valid transposed sample")
            .sample
            .expect("sample routing remains selected");
        assert_eq!(sample.sample, rustel_audio::SampleId(7));
        assert_eq!(
            sample.playback_rate, 1.0,
            "0.5 speed times octave-up transposition"
        );
    }
}
#[cfg(test)]
mod tests {
    /// A library still fetching a manifest answers Loading for every name
    /// it has not seen. That must not cost a native synth its onset: the
    /// first `sbd` after a switch was lost exactly this way.
    #[test]
    fn a_native_synth_never_waits_on_a_manifest() {
        struct StillLoading;
        impl super::SampleLookup for StillLoading {
            fn resolve(&self, _s: &str, _n: f64, _midi: f64) -> super::SampleResolution {
                super::SampleResolution::Loading
            }
        }
        let resolve = |value: serde_json::Value| {
            super::resolve_voice_with_samples(&value, 1, 0.25, 0.0, 48_000, 0.5, &StillLoading)
        };
        let synth = resolve(serde_json::json!({ "s": "sbd" })).expect("sbd is a synth");
        assert!(
            matches!(synth.synth, Some(rustel_audio::SynthSource::Sbd { .. })),
            "the oscillator path took it"
        );
        let transposed = resolve(serde_json::json!({ "s": "supersaw", "note": "" }))
            .expect("a falsy synth note takes the oscillator's default pitch");
        assert!(
            matches!(
                transposed.synth,
                Some(rustel_audio::SynthSource::Supersaw { .. })
            ),
            "the empty note did not turn supersaw into a sample"
        );
        assert!(
            (transposed.freq_hz - rustel_core::util::midi_to_freq(36.0) as f32).abs() < 0.001,
            "the falsy note uses the synth default: {} Hz",
            transposed.freq_hz
        );
        let sample = resolve(serde_json::json!({ "s": "bd" })).expect_err("bd is a sample");
        assert!(sample.contains("still loading"), "{sample}");
        let banked = resolve(serde_json::json!({ "s": "sbd", "bank": "tr909" }))
            .expect_err("a banked name is a sample");
        assert!(banked.contains("still loading"), "{banked}");
    }

    #[test]
    fn native_synth_names_match_the_generator_surface() {
        for name in [
            "white", "pink", "brown", "crackle", "bytebeat", "bus", "supersaw", "pulse", "sbd",
            "sine", "sin", "triangle", "tri", "square", "sqr", "sawtooth", "saw", "user",
            "TRIANGLE",
        ] {
            assert!(super::is_native_synth_sound(name), "missed {name:?}");
        }
        for name in ["bd", "hh", "Supersaw", "custom"] {
            assert!(
                !super::is_native_synth_sound(name),
                "misclassified {name:?}"
            );
        }
    }

    // Accept scalar, pair and colon-packed diode amounts. An adjacent distort
    // control keeps its own algorithm.
    #[test]
    fn the_diode_control_names_the_diode_waveshaper() {
        let diode_algorithm = rustel_audio::DISTORTION_ALGORITHMS
            .iter()
            .position(|name| *name == "diode")
            .expect("the diode algorithm is in the table") as u8;
        let controls = |value: serde_json::Value| {
            super::resolve_voice(&value, 1, 0.25, 0.0, 48_000, 0.5)
                .expect("voice resolves")
                .controls
                .distort
                .expect("the diode control built a distortion stage")
        };
        let scalar = controls(serde_json::json!({ "s": "sine", "diode": 1.0 }));
        assert_eq!(scalar.algorithm, diode_algorithm, "diode names its shape");
        assert_eq!(scalar.amount, 1.0);
        assert_eq!(
            f64::from(scalar.postgain),
            1.0,
            "a scalar leaves volume alone"
        );

        let pair = controls(serde_json::json!({ "s": "sine", "diode": [2.5, 0.6] }));
        assert_eq!(pair.algorithm, diode_algorithm);
        assert_eq!(pair.amount, 2.5);
        assert_eq!(pair.postgain, 0.6, "the pair's volume rides");

        let packed = controls(serde_json::json!({ "s": "sine", "diode": "1" }));
        assert_eq!(packed.algorithm, diode_algorithm);
        assert_eq!(packed.amount, 1.0);

        let packed_pair = controls(serde_json::json!({ "s": "sine", "diode": "2.5:.6" }));
        assert_eq!(packed_pair.amount, 2.5);
        assert_eq!(packed_pair.postgain, 0.6);

        // An explicit `distort` keeps its own algorithm and amount.
        let distort = controls(serde_json::json!({
            "s": "sine", "diode": 1.0, "distort": 2.0, "distorttype": "fold"
        }));
        let fold_algorithm = rustel_audio::DISTORTION_ALGORITHMS
            .iter()
            .position(|name| *name == "fold")
            .expect("fold is in the table") as u8;
        assert_eq!(distort.algorithm, fold_algorithm);
        assert_eq!(distort.amount, 2.0);

        // A non-number that is not an amount pair is an error, not silence.
        let refused = super::resolve_voice(
            &serde_json::json!({ "s": "sine", "diode": "loud" }),
            1,
            0.25,
            0.0,
            48_000,
            0.5,
        )
        .expect_err("a word is not a distortion amount");
        assert!(refused.contains("diode"), "{refused}");
    }

    // A diode modulator uses the amount as its base and stays inert without diode.
    #[test]
    fn a_diode_modulator_rides_the_distortion_amount() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "diode", "rate": 2}}}}}}"#)
        };

        let (lfos, _, _) = mods(&lfo(r#""diode": 1.0, "#));
        let slot = lfos[0].expect("a diode modulator resolves when diode is set");
        assert_eq!(slot.target, rustel_audio::ModTarget::Distort);
        assert_eq!(f64::from(slot.param_base), 1.0, "the amount is the base");

        let (lfos, _, _) = mods(&lfo(""));
        assert!(
            lfos[0].is_none(),
            "the diode modulator must be inert with no diode node"
        );
    }

    /// A stretched voice's start time is pulled back by the phase vocoder's
    /// latency before scheduling anything: the reference's 0.04 s, plus the
    /// frames this vocoder emits later than one run a render quantum at a time.
    #[test]
    fn a_stretched_voice_is_pulled_back_by_the_vocoder_latency() {
        let frame = |value: serde_json::Value, at: f64| {
            super::resolve_voice(&value, 1, 0.5, at, 48_000, 0.5)
                .expect("voice resolves")
                .onset_frame
        };
        let plain = frame(serde_json::json!({ "note": "c3" }), 1.0);
        let stretched = frame(serde_json::json!({ "note": "c3", "stretch": 1.7 }), 1.0);
        assert_eq!(plain, 48_000, "unstretched onset should sit on its beat");
        assert_eq!(
            stretched,
            48_000 - 1_920 - 127,
            "a stretched onset must lead its beat by the vocoder latency"
        );

        // An onset that would land before the render clamps rather than
        // failing: WebAudio treats a past start time as "start now".
        let early = frame(serde_json::json!({ "note": "c3", "stretch": 1.7 }), 0.0);
        assert_eq!(early, 0, "a first-beat stretch must clamp, not underflow");
    }

    /// A soundfont with no note of its own plays c3, an octave above the
    /// sampler's default. The two defaults live in different places
    /// upstream, and using the sampler's for both put every noteless
    /// `gm_*` voice an octave low.
    #[test]
    fn a_noteless_soundfont_defaults_an_octave_above_a_noteless_sample() {
        let midi_for = |name: &str| {
            let value = serde_json::json!({ "s": name });
            let object = value.as_object().expect("object");
            super::sample_default_midi(object, name)
        };
        assert_eq!(
            midi_for("gm_flute"),
            48.0,
            "a gm_* soundfont defaults to c3"
        );
        assert_eq!(
            midi_for("bd"),
            36.0,
            "an ordinary sample bank keeps midi 36"
        );
    }

    // Bare note values stay silent in Strudel; only wrapped controls resolve.
    #[test]
    fn a_non_object_hap_value_is_refused_like_strudel() {
        for value in [
            serde_json::json!("c3"),
            serde_json::json!("C3"),
            serde_json::json!(60),
        ] {
            let error = super::resolve_voice(&value, 1, 0.5, 0.0, 48_000, 0.5)
                .expect_err("a bare value must not resolve to a voice");
            assert!(
                error.contains("expected hap.value to be an object"),
                "refusal should carry the hint, got {error:?}"
            );
        }
        // The wrapped form is what a real hap carries, and still plays.
        super::resolve_voice(
            &serde_json::json!({ "note": "c3" }),
            2,
            0.5,
            0.0,
            48_000,
            0.5,
        )
        .expect("an object value still resolves");
    }

    // Applying a control to a bare value stores it under `value`, not `note`.
    // That key must not override the source's default pitch.
    #[test]
    fn a_bare_value_key_is_not_a_note() {
        let hz = |value: serde_json::Value| {
            super::resolve_voice(&value, 1, 0.5, 0.0, 48_000, 0.5)
                .expect("voice resolves")
                .freq_hz
        };
        let default_hz = 440.0 * 2f32.powf((36.0 - 69.0) / 12.0);
        let with_value_key = hz(serde_json::json!({ "release": 0.401, "value": "C3" }));
        assert!(
            (with_value_key - default_hz).abs() < 0.01,
            "a `value` key must not set the pitch: got {with_value_key} Hz, \
         strudel.cc's default is {default_hz} Hz"
        );
        // A real `note` still wins, so this does not blunt the normal path.
        let with_note = hz(serde_json::json!({ "release": 0.401, "note": "c3" }));
        assert!(
            (with_note - default_hz).abs() > 1.0,
            "an explicit note should not collapse to the default"
        );
    }

    // sbd defaults to MIDI 29 (F1); other sources use MIDI 36 (C2).
    #[test]
    fn an_unpitched_sbd_is_f1_and_not_c2() {
        let hz = |value: serde_json::Value| {
            super::resolve_voice(&value, 1, 0.5, 0.0, 48_000, 0.5)
                .expect("voice resolves")
                .freq_hz
        };
        let sbd = hz(serde_json::json!({ "s": "sbd" }));
        let expected = 440.0 * 2f32.powf((29.0 - 69.0) / 12.0);
        assert!(
            (sbd - expected).abs() < 0.01,
            "sbd default was {sbd} Hz, strudel.cc is {expected} Hz"
        );

        // Every other source keeps the shared default.
        let sine = hz(serde_json::json!({ "s": "sine" }));
        let c2 = 440.0 * 2f32.powf((36.0 - 69.0) / 12.0);
        assert!((sine - c2).abs() < 0.01, "sine default moved to {sine} Hz");

        // An explicit note still wins over either default.
        let pitched = hz(serde_json::json!({ "s": "sbd", "note": 60 }));
        let c4 = 440.0 * 2f32.powf((60.0 - 69.0) / 12.0);
        assert!((pitched - c4).abs() < 0.01, "an explicit note was ignored");
    }

    // JS `note || defaultNote` replaces a synth's falsy note. Samples retain it.
    #[test]
    fn a_falsy_note_takes_the_synth_default_but_not_the_samplers() {
        let hz = |value: serde_json::Value| {
            super::resolve_voice(&value, 1, 0.5, 0.0, 48_000, 0.5)
                .expect("voice resolves")
                .freq_hz
        };
        let c2 = 440.0 * 2f32.powf((36.0 - 69.0) / 12.0);
        let f1 = 440.0 * 2f32.powf((29.0 - 69.0) / 12.0);

        // `note(0)` is the reachable one: an empty or null note is refused by
        // the mini parser long before a hap exists, so only zero arrives here.
        let got = hz(serde_json::json!({ "s": "sine", "note": 0 }));
        assert!(
            (got - c2).abs() < 0.01,
            "a zero note gave {got} Hz instead of the default {c2}"
        );
        // The source's own default still applies underneath the fallback.
        let sbd = hz(serde_json::json!({ "s": "sbd", "note": 0 }));
        assert!((sbd - f1).abs() < 0.01, "sbd falsy note gave {sbd} Hz");

        // A real note is untouched, including a legitimately low one.
        let c4 = 440.0 * 2f32.powf((60.0 - 69.0) / 12.0);
        let pitched = hz(serde_json::json!({ "s": "sine", "note": 60 }));
        assert!(
            (pitched - c4).abs() < 0.01,
            "note(60) moved to {pitched} Hz"
        );
        let low = hz(serde_json::json!({ "s": "sine", "note": 1 }));
        let midi_one = 440.0 * 2f32.powf((1.0 - 69.0) / 12.0);
        assert!((low - midi_one).abs() < 0.01, "note(1) moved to {low} Hz");
    }

    #[test]
    fn diagnostic_logging_scope_is_nested_and_panic_safe() {
        super::DIRECT_DIAGNOSTIC_LOGGING.with(|logging| logging.set(Some(true)));
        super::with_diagnostic_policy(false, || {
            assert!(!super::direct_diagnostic_logging());
            super::with_diagnostic_policy(true, || {
                assert!(super::direct_diagnostic_logging());
            });
            assert!(!super::direct_diagnostic_logging());
        });
        assert!(super::direct_diagnostic_logging());

        let _ = std::panic::catch_unwind(|| {
            super::with_diagnostic_policy(false, || panic!("test unwind"));
        });
        assert!(super::direct_diagnostic_logging());
        super::COLLECTED_NOTICES.with(|collected| assert!(collected.borrow().is_none()));
    }

    /// A library host writes nothing to stderr unless it asks to: outside a
    /// scope, a thread follows the process-wide default, which starts off.
    #[test]
    fn a_thread_outside_a_scope_follows_the_quiet_default() {
        std::thread::spawn(|| {
            assert!(!super::default_direct_diagnostic_logging());
            assert!(!super::direct_diagnostic_logging());
            super::with_diagnostic_policy(true, || {
                assert!(super::direct_diagnostic_logging());
            });
            assert!(!super::direct_diagnostic_logging());
        })
        .join()
        .expect("diagnostic policy thread");
    }

    /// With direct logging off, a scope hands its host each distinct notice
    /// once, and a nested scope's notices stay its own.
    #[test]
    fn a_quiet_scope_returns_its_notices_deduplicated() {
        let duck_to_nowhere = || {
            resolve_voice(
                &object(r#"{"s": "sine", "duckorbit": 99}"#).into(),
                0,
                0.5,
                0.0,
                48_000,
                0.5,
            )
            .expect("the voice plays without its duck target")
        };
        let ((), notices) = super::with_diagnostic_policy(false, || {
            duck_to_nowhere();
            let ((), inner) = super::with_diagnostic_policy(false, || {
                duck_to_nowhere();
            });
            assert_eq!(inner.len(), 1);
            duck_to_nowhere();
        });
        assert_eq!(
            notices,
            vec![super::VoiceNotice {
                message: "duck target orbit 99 does not exist".into(),
                record: serde_json::json!({
                    "duck_skipped": { "message": "duck target orbit 99 does not exist" }
                }),
            }]
        );

        let (_, printed) = super::with_diagnostic_policy(true, duck_to_nowhere);
        assert!(printed.is_empty());
    }

    /// A notice whose record has no `message` of its own still reads as a
    /// sentence.
    #[test]
    fn record_only_notices_carry_a_readable_message() {
        let partials = vec![1.0; rustel_audio::MAX_PARTIALS + 1];
        let voices = [
            serde_json::json!({ "s": "sawtooth", "partials": partials }),
            serde_json::json!({ "s": "sine", "room": 0.5, "ir": "nowhere" }),
        ];
        let ((), notices) = super::with_diagnostic_policy(false, || {
            for voice in &voices {
                resolve_voice(voice, 0, 0.5, 0.0, 48_000, 0.5).expect("the voice still plays");
            }
        });
        let messages: Vec<&str> = notices
            .iter()
            .map(|notice| notice.message.as_str())
            .collect();
        assert_eq!(
            messages,
            [
                format!(
                    "{} partials requested; the first {} play",
                    rustel_audio::MAX_PARTIALS + 1,
                    rustel_audio::MAX_PARTIALS
                )
                .as_str(),
                "impulse response 'nowhere' is not a known sound; the generated reverb plays",
            ]
        );
    }

    /// `freq *= 2^octave`. Ignoring it left every octave-transposed pattern
    /// sounding at its written pitch.
    #[test]
    fn octave_transposes_the_resolved_frequency() {
        let hz = |json: &str| {
            resolve_voice(&object(json).into(), 0, 0.5, 0.0, 48_000, 0.5)
                .expect("voice")
                .freq_hz
        };
        let base = hz(r#"{"s": "sine", "note": "c3"}"#);
        for (octave, factor) in [(-1.0f32, 0.5f32), (1.0, 2.0), (2.0, 4.0)] {
            let shifted = hz(&format!(
                r#"{{"s": "sine", "note": "c3", "octave": {octave}}}"#
            ));
            let want = base * factor;
            assert!(
                (shifted - want).abs() < want * 1e-4,
                "octave {octave}: expected {want} Hz, got {shifted}"
            );
        }
        // An explicit freq is transposed too: the multiplier applies after
        // either route.
        let explicit = hz(r#"{"s": "sine", "freq": 200, "octave": 1}"#);
        assert!(
            (explicit - 400.0).abs() < 0.05,
            "an explicit freq was not transposed: {explicit}"
        );
    }

    /// `fmwave` names the modulator's shape per operator, unsuffixed for the
    /// first. An unknown name is a score error rather than a silent sine,
    /// which would leave a typo sounding plausible but wrong.
    #[test]
    fn fmwave_names_resolve_per_operator_and_reject_typos() {
        let resolved = resolve_voice(
            &serde_json::json!({
                "s": "sine", "note": "c3", "fmi": 4, "fmwave": "square",
                "fmi2": 2, "fmwave2": "brown"
            }),
            0,
            0.5,
            0.0,
            48_000,
            0.5,
        )
        .expect("fm with per-operator waveforms");
        let fm = resolved.controls.fm.expect("fm controls");
        assert_eq!(
            fm.operators[0].expect("operator 1").waveform,
            rustel_audio::FmWave::Square
        );
        assert_eq!(
            fm.operators[1].expect("operator 2").waveform,
            rustel_audio::FmWave::Noise(2)
        );
        // Unsuffixed defaults to sine when absent.
        let plain = resolve_voice(
            &serde_json::json!({ "s": "sine", "note": "c3", "fmi": 4 }),
            0,
            0.5,
            0.0,
            48_000,
            0.5,
        )
        .expect("fm without a named waveform");
        assert_eq!(
            plain.controls.fm.expect("fm").operators[0]
                .expect("operator 1")
                .waveform,
            rustel_audio::FmWave::Sine
        );

        let error = resolve_voice(
            &serde_json::json!({ "s": "sine", "note": "c3", "fmi": 4, "fmwave": "sqaure" }),
            0,
            0.5,
            0.0,
            48_000,
            0.5,
        )
        .expect_err("a misspelled waveform must be refused");
        assert!(
            error.contains("unsupported fmwave"),
            "unexpected refusal: {error}"
        );
    }

    /// The bus space stops at MAX_BUSES. Say so rather than folding a high
    /// bus onto a low one, which would mix two unrelated sends into each
    /// other.
    #[test]
    fn a_bus_outside_the_native_range_is_refused_not_folded() {
        let out_of_range = rustel_audio::MAX_BUSES;
        for score in [
            serde_json::json!({ "s": "sine", "note": "c3", "bus": out_of_range }),
            serde_json::json!({ "s": "bus", "n": out_of_range }),
            serde_json::json!({ "s": "sine", "note": "c3", "bus": -1 }),
        ] {
            let err = resolve_voice(&score, 0, 0.5, 0.0, 48_000, 0.5)
                .expect_err("an out-of-range bus should be refused");
            assert!(
                err.contains("outside the supported"),
                "unexpected refusal for {score}: {err}"
            );
        }
        // The last bus in range still resolves.
        let last = resolve_voice(
            &serde_json::json!({ "s": "bus", "n": out_of_range - 1 }),
            0,
            0.5,
            0.0,
            48_000,
            0.5,
        )
        .expect("the last bus is in range");
        assert_eq!(
            last.synth,
            Some(rustel_audio::SynthSource::Bus {
                bus: (out_of_range - 1) as u8
            })
        );
    }

    use super::*;

    fn object(json: &str) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_str(json).expect("test json") {
            serde_json::Value::Object(map) => map,
            other => panic!("expected object, got {other}"),
        }
    }

    // An invalid duck target reports a notice and is skipped; the voice still plays.
    #[test]
    fn unusable_duck_targets_are_skipped_and_never_refuse_the_voice() {
        // Fractional target: no such orbit - voice plays with no duck.
        let duck = duck_controls(&object(r#"{"duckorbit": 0.1}"#)).expect("no refusal");
        assert!(duck.is_none(), "duck(0.1) must degrade to no duck");
        // Out-of-range and non-numeric targets: same skip.
        assert!(
            duck_controls(&object(r#"{"duckorbit": 99}"#))
                .expect("no refusal")
                .is_none()
        );
        assert!(
            duck_controls(&object(r#"{"duckorbit": "x"}"#))
                .expect("no refusal")
                .is_none()
        );
        // A list keeps its valid targets while skipping the bad one.
        let duck = duck_controls(&object(r#"{"duckorbit": [0, 0.1, 2]}"#))
            .expect("no refusal")
            .expect("valid targets remain");
        let orbits: Vec<u8> = duck.targets.iter().flatten().map(|t| t.orbit).collect();
        assert_eq!(orbits, vec![0, 2]);
        // JS object keys are strings: "2" ducks orbit 2.
        let duck = duck_controls(&object(r#"{"duckorbit": "2"}"#))
            .expect("no refusal")
            .expect("string target");
        assert_eq!(duck.targets[0].map(|t| t.orbit), Some(2));
        // Per-index parameter lists use index-0 fallback: depth "1:0.5" over
        // orbits "2:3".
        let duck = duck_controls(&object(
            r#"{"duckorbit": [2, 3], "duckdepth": [1, 0.5], "duckattack": 0.2}"#,
        ))
        .expect("no refusal")
        .expect("multi-target duck");
        let targets: Vec<_> = duck.targets.iter().flatten().collect();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].orbit, 2);
        assert_eq!(targets[0].depth, 1.0);
        assert_eq!(targets[1].orbit, 3);
        assert_eq!(targets[1].depth, 0.5);
        assert_eq!(targets[1].attack_secs, 0.2);
    }

    // Distortion modulators resolve only when the corresponding control builds a node.
    #[test]
    fn the_distortion_worklet_params_are_modulatable_when_their_node_exists() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |control: &str, extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#)
        };

        for (control, base, extra) in [
            ("coarse", 4.0, r#""coarse": 4, "#),
            ("crush", 8.0, r#""crush": 8, "#),
            ("shape", 0.5, r#""shape": 0.5, "#),
            ("shapevol", 1.0, r#""shape": 0.5, "#),
        ] {
            let (lfos, _, _) = mods(&lfo(control, extra));
            let slot = lfos[0].unwrap_or_else(|| panic!("{control} modulator should resolve"));
            assert_eq!(
                f64::from(slot.param_base),
                base,
                "{control} modulator took the wrong param base"
            );
            // Same modulator, control absent: no node, so no slot.
            let (lfos, _, _) = mods(&lfo(control, ""));
            assert!(
                lfos[0].is_none(),
                "{control} modulator must be inert with no {control} node"
            );
        }
    }

    // Synth pitch modulation uses frequency; samples use detune in cents with base 0.
    #[test]
    fn a_pitch_modulator_rides_detune_on_a_sample_and_frequency_on_a_synth() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |sound: &str, control: &str| {
            format!(r#"{{"s": "{sound}", "lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#)
        };

        for control in ["note", "s", "freq", "frequency"] {
            let (lfos, _, _) = mods(&lfo("sawtooth", control));
            let slot = lfos[0].expect("an oscillator resolves the pitch target");
            assert_eq!(
                slot.param_base, 220.0,
                "{control} on an oscillator rides its frequency"
            );
            assert_eq!(slot.min, 20.0 - 220.0, "and takes the frequency clamp");

            let (lfos, _, _) = mods(&lfo("bd", control));
            let slot = lfos[0].expect("a sample resolves the pitch target too");
            // detune reads 0, and a relative depth counts a param at 0 as 1.
            // A relative depth of 2 is then two cents, not two notes.
            assert_eq!(
                slot.param_base, 0.0,
                "{control} on a sample rides detune, which reads 0"
            );
            // 1 is under the 30 the frequency clamp needs, so no clamp is
            // imposed and the range stays the LFO's own: dcoffset·depth up
            // to one depth above it.
            assert_eq!(slot.min, -0.5, "{control} on a sample takes no clamp");
            assert_eq!(slot.max, 0.5, "{control} on a sample takes no clamp");
        }
    }

    // The reference connects bus modulation before the waveshaper, bypassing the
    // frequency clamp that applies to an LFO.
    #[test]
    fn a_bus_modulator_takes_no_frequency_range_where_an_lfo_does() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };

        let (_, _, buses) =
            mods(r#"{"cutoff": 800, "bmod": {"a": {"control": "cutoff", "bus": 0, "depth": 2}}}"#);
        let bus = buses[0].expect("the bus modulator should resolve");
        assert_eq!(bus.min, f32::NEG_INFINITY, "a bus modulator is not clamped");
        assert_eq!(bus.max, f32::INFINITY, "a bus modulator is not clamped");

        // The same target, reached by an LFO, keeps the range the worklet is
        // given: 20 Hz to 24 kHz around the cutoff it adds to.
        let (lfos, _, _) =
            mods(r#"{"cutoff": 800, "lfo": {"a": {"control": "cutoff", "rate": 2}}}"#);
        let lfo = lfos[0].expect("the lfo should resolve");
        assert_eq!(lfo.min, 20.0 - 800.0);
        assert_eq!(lfo.max, 24_000.0 - 800.0);
    }

    /// The vowel node exposes five filter frequencies, but the shared
    /// signal's base and frequency range derive from the first one.
    #[test]
    fn the_vowel_formants_share_a_modulator_based_on_the_first_filter() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "vowel", "rate": 2}}}}}}"#)
        };

        let (lfos, _, _) = mods(&lfo(r#""vowel": "a", "#));
        let slot = lfos[0].expect("vowel should resolve");
        assert_eq!(slot.target, rustel_audio::ModTarget::VowelFreq);
        assert_eq!(slot.param_base, 660.0);
        assert_eq!(slot.min, 20.0 - 660.0);
        assert_eq!(slot.max, 24_000.0 - 660.0);

        let (lfos, _, _) = mods(&lfo(""));
        assert!(
            lfos[0].is_none(),
            "vowel must be inert when no formant bank was built"
        );
    }

    /// The pulse-width LFO exists only for a nonzero resolved sweep. Its
    /// paired defaults are already installed when a modulator reads either
    /// worklet param.
    #[test]
    fn the_pulse_width_lfo_params_are_modulatable_when_that_lfo_exists() {
        let mods = |control: &str, extra: &str| {
            let json =
                format!(r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#);
            modulator_controls(&object(&json), 1.0, 0.1, 2.0, 0.5, 220.0)
                .expect("no refusal")
                .0[0]
        };

        let rate =
            mods("pwrate", r#""s": "pulse", "pwrate": 40, "#).expect("pwrate should resolve");
        assert_eq!(rate.target, rustel_audio::ModTarget::PulseWidthLfoRate);
        assert_eq!(rate.param_base, 40.0);
        assert_eq!(rate.min, 20.0 - 40.0);
        assert_eq!(rate.max, 24_000.0 - 40.0);

        let sweep =
            mods("pwsweep", r#""s": "pulse", "pwsweep": 0.4, "#).expect("pwsweep should resolve");
        assert_eq!(sweep.target, rustel_audio::ModTarget::PulseWidthLfoDepth);
        assert!((sweep.param_base - 0.4).abs() < 1e-6);
        // Supplying only pwrate creates the LFO with the default 0.3 sweep;
        // supplying only pwsweep creates it with rate 1.
        assert!(
            (mods("pwsweep", r#""s": "pulse", "pwrate": 2, "#)
                .expect("default sweep")
                .param_base
                - 0.3)
                .abs()
                < 1e-6
        );
        assert_eq!(
            mods("pwrate", r#""s": "pulse", "pwsweep": 0.4, "#)
                .expect("default rate")
                .param_base,
            1.0
        );

        for control in ["pwrate", "pwsweep"] {
            assert!(mods(control, r#""s": "pulse", "#).is_none());
            assert!(mods(control, r#""s": "pulse", "pwsweep": 0, "pwrate": 2, "#).is_none());
            assert!(mods(control, r#""s": "sine", "pwrate": 2, "#).is_none());
        }
    }

    /// A modulator on the tremolo's depth, skew or shape rides the value the
    /// node holds: the carrier gain's floor `max(1 − depth, 0)`, the skew after
    /// its shape-dependent default, and the shape's index. The modulator's
    /// relative depth counts a param at 0 as 1. With no tremolo in the chain
    /// there is nothing to modulate.
    #[test]
    fn tremolo_modulators_ride_the_param_values_not_the_controls() {
        use rustel_audio::ModTarget::{TremoloDepth, TremoloShape, TremoloSkew};
        let lfo = |extra: &str, control: &str| {
            let json = format!(
                r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2, "depth": 0.5}}}}}}"#
            );
            let (lfos, _, _) =
                modulator_controls(&object(&json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal");
            lfos[0]
        };

        let tremolo = r#""tremolo": 4, "#;
        for (extra, control, target, base) in [
            (tremolo, "tremolodepth", TremoloDepth, 0.0),
            (
                r#""tremolo": 4, "tremolodepth": 0.25, "#,
                "tremolodepth",
                TremoloDepth,
                0.75,
            ),
            (tremolo, "tremoloskew", TremoloSkew, 1.0),
            (
                r#""tremolo": 4, "tremoloshape": 1, "#,
                "tremoloskew",
                TremoloSkew,
                0.5,
            ),
            (
                r#""tremolo": 4, "tremoloskew": 0.3, "#,
                "tremoloskew",
                TremoloSkew,
                0.3,
            ),
            (
                r#""tremolo": 4, "tremoloshape": "saw", "#,
                "tremoloshape",
                TremoloShape,
                3.0,
            ),
            (
                r#""tremolo": 4, "tremoloshape": 7, "#,
                "tremoloshape",
                TremoloShape,
                2.0,
            ),
        ] {
            let slot = lfo(extra, control)
                .unwrap_or_else(|| panic!("{control} should resolve for {extra}"));
            assert_eq!(slot.target, target);
            assert!(
                (f64::from(slot.param_base) - base).abs() < 1e-6,
                "{control} for {extra} rode {} instead of {base}",
                slot.param_base
            );
            let current = if base == 0.0 { 1.0 } else { base };
            assert!(
                (f64::from(slot.depth) - 0.5 * current).abs() < 1e-6,
                "{control} for {extra}: a relative depth of 0.5 spans half the base"
            );
        }

        for control in ["tremolodepth", "tremoloskew", "tremoloshape"] {
            assert!(
                lfo("", control).is_none(),
                "{control} must be inert with no tremolo in the chain"
            );
        }
    }

    /// `delaytime` and `delaysync` both reach the orbit DelayNode's
    /// `delayTime`, whose value is seconds after sync conversion. With no
    /// active delay send that node is never built, so neither target
    /// resolves.
    #[test]
    fn the_orbit_delay_time_is_modulatable_under_both_control_names() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |control: &str, extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#)
        };

        for (control, extra, base) in [
            (
                "delaytime",
                r#""delay": 0.7, "delaytime": 0.2, "delayfeedback": 0.6, "#,
                0.2,
            ),
            // cps is 0.5, so delaysync 0.25 resolves to 0.5 seconds.
            (
                "delaysync",
                r#""delay": 0.7, "delaysync": 0.25, "delayfeedback": 0.6, "#,
                0.5,
            ),
        ] {
            let (lfos, _, _) = mods(&lfo(control, extra));
            let slot = lfos[0].unwrap_or_else(|| panic!("{control} should resolve"));
            assert_eq!(slot.target, rustel_audio::ModTarget::DelayTime);
            assert!((f64::from(slot.param_base) - base).abs() < 1e-6);

            let (lfos, _, _) = mods(&lfo(control, ""));
            assert!(
                lfos[0].is_none(),
                "{control} must be inert when no delay node was built"
            );
        }
    }

    /// `delayfeedback` reaches the feedback GainNode only when the orbit
    /// delay graph exists, and rides the clamped value assigned to its gain.
    #[test]
    fn the_orbit_delay_feedback_gain_is_modulatable_when_built() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "delayfeedback", "rate": 2}}}}}}"#)
        };

        let (lfos, _, _) = mods(&lfo(
            r#""delay": 0.7, "delaytime": 0.2, "delayfeedback": 1.5, "#,
        ));
        let slot = lfos[0].expect("delayfeedback should resolve");
        assert_eq!(slot.target, rustel_audio::ModTarget::DelayFeedback);
        assert!((slot.param_base - 0.98).abs() < 1e-6);

        let (lfos, _, _) = mods(&lfo(""));
        assert!(
            lfos[0].is_none(),
            "delayfeedback must be inert when no delay node was built"
        );
    }

    /// A modulator on a compressor param rides the value the node holds:
    /// ratio 10, knee 10, attack 0.005 and release 0.05 by default, an
    /// explicit setting clamped into its param's range, and no modulation at
    /// all when no `compressor` built the node.
    #[test]
    fn compressor_modulators_ride_the_values_the_node_holds() {
        use rustel_audio::ModTarget::{
            CompressorAttack, CompressorKnee, CompressorRatio, CompressorRelease,
            CompressorThreshold,
        };
        let lfo = |extra: &str, control: &str| {
            let json =
                format!(r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#);
            let (lfos, _, _) =
                modulator_controls(&object(&json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal");
            lfos[0]
        };

        let compressor = r#""compressor": -20, "#;
        for (extra, control, target, base) in [
            (compressor, "compressorRatio", CompressorRatio, 10.0),
            (compressor, "compressorKnee", CompressorKnee, 10.0),
            (compressor, "compressorAttack", CompressorAttack, 0.005),
            (compressor, "compressorRelease", CompressorRelease, 0.05),
            (
                r#""compressor": -20, "compressorRelease": 0.2, "#,
                "compressorRelease",
                CompressorRelease,
                0.2,
            ),
            (
                r#""compressor": -20, "compressorRatio": 50, "#,
                "compressorRatio",
                CompressorRatio,
                20.0,
            ),
            (
                r#""compressor": -120, "#,
                "compressor",
                CompressorThreshold,
                -100.0,
            ),
        ] {
            let slot = lfo(extra, control)
                .unwrap_or_else(|| panic!("{control} should resolve for {extra}"));
            assert_eq!(slot.target, target);
            assert!(
                (f64::from(slot.param_base) - base).abs() < 1e-6,
                "{control} for {extra} rode {} instead of {base}",
                slot.param_base
            );
            assert!(
                (f64::from(slot.depth) - base).abs() < 1e-6,
                "{control} for {extra}: a relative depth of 1 spans the base"
            );
        }

        for control in ["compressorRatio", "compressorRelease"] {
            assert!(
                lfo("", control).is_none(),
                "{control} must be inert when no compressor node was built"
            );
        }
    }

    /// `fmi{k}` rides operator k's index and `fmh{k}` rides its frequency,
    /// `carrier × harmonicity`. Both resolve only when the operator exists.
    #[test]
    fn the_fm_operator_params_are_modulatable_where_their_operator_exists() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |control: &str, extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#)
        };
        let one = r#""fmi": 3, "fmh": 2, "#;
        let two = r#""fmi": 3, "fmi2": 5, "fmh2": 4, "#;

        for (control, base, extra) in [
            ("fmi", 3.0, one),
            // carrier 220 × fmh 2.
            ("fmh", 440.0, one),
            ("fmi2", 5.0, two),
            // carrier 220 × fmh2 4.
            ("fmh2", 880.0, two),
        ] {
            let (lfos, _, _) = mods(&lfo(control, extra));
            let slot = lfos[0].unwrap_or_else(|| panic!("{control} modulator should resolve"));
            assert_eq!(
                f64::from(slot.param_base),
                base,
                "{control} modulator took the wrong param base"
            );
        }

        // Operator 2 named nowhere: nothing to modulate, so both of its
        // params stay inert rather than landing on operator 1.
        for control in ["fmi2", "fmh2"] {
            let (lfos, _, _) = mods(&lfo(control, one));
            assert!(
                lfos[0].is_none(),
                "{control} must be inert when operator 2 was never built"
            );
        }
    }

    /// `lpdepth` and its relatives modulate the LFO that a filter builds for
    /// itself. They resolve only when the pattern sets a cutoff and at least
    /// one of rate/sync/depth/depthfrequency/shape/skew.
    ///
    /// `lprate`, `lpsync` and `lpshape` (and their hp/bp forms) stay refused,
    /// because the reference cannot modulate them.
    #[test]
    fn a_filters_own_lfo_params_are_modulatable_and_the_broken_ones_are_not() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let lfo = |control: &str, extra: &str| {
            format!(r#"{{{extra}"lfo": {{"a": {{"control": "{control}", "rate": 2}}}}}}"#)
        };
        // A cutoff plus one LFO control, so the filter builds its LFO.
        let lp = r#""cutoff": 800, "lprate": 2, "lpdepth": 0.5, "#;

        for (control, base) in [
            // depth resolves to `depth × cutoff` - 0.5 × 800.
            ("lpdepth", 400.0),
            ("lpdepthfrequency", 400.0),
            ("lpdc", -0.5),
            ("lpskew", 0.5),
        ] {
            let (lfos, _, _) = mods(&lfo(control, lp));
            let slot = lfos[0].unwrap_or_else(|| panic!("{control} should resolve"));
            assert_eq!(
                f64::from(slot.param_base),
                base,
                "{control} took the wrong param base"
            );
        }

        // A cutoff but no LFO control: the filter builds no LFO, so there is
        // no node for these to reach.
        for control in ["lpdepth", "lpdc", "lpskew"] {
            let (lfos, _, _) = mods(&lfo(control, r#""cutoff": 800, "#));
            assert!(
                lfos[0].is_none(),
                "{control} must be inert when the filter built no LFO"
            );
        }

        for control in [
            "lprate", "lpsync", "lpshape", "hprate", "hpsync", "hpshape", "bprate", "bpsync",
            "bpshape",
        ] {
            assert!(
                filter_lfo_control(control).is_none(),
                "{control} cannot be modulated strudel.cc and must stay refused"
            );
        }
    }

    #[test]
    fn filter_lfo_shape_names_select_the_documented_waveforms() {
        let shape = |gate: &str, control: &str, value: &str| {
            let json = format!(r#"{{"{gate}": 800, "{control}": "{value}"}}"#);
            let (lfos, _, _) = modulator_controls(&object(&json), 1.0, 0.1, 0.0, 0.5, 220.0)
                .expect("documented filter LFO shape");
            lfos[0].expect("the filter shape builds its LFO")
        };

        for (value, expected) in [
            ("tri", 0),
            ("triangle", 0),
            ("sine", 1),
            ("ramp", 2),
            ("saw", 3),
            ("square", 4),
        ] {
            assert_eq!(shape("cutoff", "lpshape", value).shape, expected, "{value}");
        }
        assert_eq!(shape("hcutoff", "hpshape", "saw").shape, 3);
        assert_eq!(shape("bandf", "bpshape", "square").shape, 4);

        let error = modulator_controls(
            &object(r#"{"cutoff": 800, "lpshape": "hexagon"}"#),
            1.0,
            0.1,
            0.0,
            0.5,
            220.0,
        )
        .expect_err("an unknown documented-choice value must not silently become triangle");
        assert!(error.contains("unsupported lpshape LFO shape \"hexagon\""));
    }

    /// A modulator can aim at another modulator: `lfo({...}, 'cut')` names
    /// one and `lfo({ c: 'lfo_cut', sc: 'rate' })` rides its rate. With no
    /// subControl the rate is the default.
    ///
    /// An lfo that aims at an `env_...` is dropped by design: every LFO is
    /// wired before any envelope node is registered. An env that aims at an
    /// lfo works.
    #[test]
    fn a_modulator_can_aim_at_another_modulator_except_where_strudel_cannot() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        // `cut` sweeps the cutoff; `a` rides one of `cut`'s own params.
        let aimed = |sub: &str| {
            let sc = if sub.is_empty() {
                String::new()
            } else {
                format!(r#""subControl": "{sub}", "#)
            };
            format!(
                r#"{{"cutoff": 800, "lfo": {{
                 "a": {{"control": "lfo_cut", {sc}"rate": 1.5}},
                 "cut": {{"control": "cutoff", "rate": 2, "depth": 0.5}}
               }}}}"#
            )
        };

        use rustel_audio::{ModTarget, ModulatorParam};
        // `cut` comes second in the map, so its id is 1 - and the modulator
        // aiming at it is resolved first, which is exactly why the resolver
        // walks the entries twice.
        for (sub, expected) in [
            ("", ModulatorParam::Rate),
            ("rate", ModulatorParam::Rate),
            ("sync", ModulatorParam::Rate),
            ("depth", ModulatorParam::Depth),
            ("depthabs", ModulatorParam::Depth),
            ("skew", ModulatorParam::Skew),
            ("curve", ModulatorParam::Curve),
            ("dcoffset", ModulatorParam::Dcoffset),
        ] {
            let (lfos, _, _) = mods(&aimed(sub));
            let aiming = lfos
                .iter()
                .flatten()
                .find(|lfo| matches!(lfo.target, ModTarget::LfoParam(..)))
                .unwrap_or_else(|| panic!("subControl '{sub}' should resolve"));
            assert_eq!(
                aiming.target,
                ModTarget::LfoParam(1, expected),
                "subControl '{sub}' picked the wrong param"
            );
        }

        // Naming a modulator that does not exist drops it.
        let (lfos, _, _) =
            mods(r#"{"cutoff": 800, "lfo": {"a": {"control": "lfo_nope", "rate": 1.5}}}"#);
        assert!(
            lfos[0].is_none(),
            "a modulator naming an id nothing declared must be dropped"
        );

        // An lfo cannot reach an env: no env node exists yet.
        let (lfos, _, _) = mods(
            r#"{"cutoff": 800,
            "lfo": {"a": {"control": "env_e", "subControl": "attack", "rate": 1.5}},
            "env": {"e": {"control": "cutoff", "depth": 0.5}}}"#,
        );
        assert!(
            lfos[0].is_none(),
            "an lfo aiming at an env must be dropped - strudel.cc wires every \
         lfo before any envelope node exists"
        );
    }

    /// A stage carries the complete effects chain, not only the filters and
    /// the distortion.
    #[test]
    fn a_stage_carries_the_whole_effects_chain() {
        let stages =
            |json: &str| fx_stages(&object(json), 0.0, 0.0, 0.5, &BundledOnly).expect("resolve");
        let stage = stages(
            r#"{"s": "sine", "FX": [{
            "vowel": "a", "tremolo": 4, "compressor": -20,
            "pan": 0.9, "phaserrate": 2, "stretch": 2, "transient": 0.5,
            "delay": 0.5, "room": 0.6
        }]}"#,
        )[0]
        .expect("the stage resolved");

        assert!(stage.vowel.is_some(), "vowel was dropped");
        assert_eq!(stage.tremolo.map(|t| t.frequency_hz), Some(4.0));
        assert_eq!(stage.compressor.map(|c| c.threshold_db), Some(-20.0));
        // StereoPanner axis: 2*0.9 - 1.
        assert_eq!(stage.pan_x, Some(0.8));
        assert_eq!(stage.phaser.map(|p| p.rate_hz), Some(2.0));
        assert_eq!(stage.stretch, Some(2.0));
        assert_eq!(stage.transient.map(|t| t.attack), Some(0.5));
        assert!(stage.delay.is_some(), "delay was dropped");
        assert_eq!(stage.room.map(|r| r.wet), Some(0.6));
    }

    /// A stage ignores `orbit` and `duckorbit` by design. The stage still
    /// exists and applies its default gain of 0.8.
    #[test]
    fn a_stage_naming_only_an_orbit_still_applies_the_default_gain() {
        let stage = fx_stages(
            &object(r#"{"s": "sine", "FX": [{"orbit": 2, "duckorbit": 1}]}"#),
            0.0,
            0.0,
            0.5,
            &BundledOnly,
        )
        .expect("resolve")[0]
            .expect("the stage still exists");
        assert!((stage.gain - 0.8).abs() < 1e-6, "got {}", stage.gain);
    }

    /// `tremolosync` is in cycles, so the tremolo frequency of a stage
    /// depends on cps.
    #[test]
    fn a_stage_tremolo_sync_follows_the_tempo() {
        let at_cps = |cps: f64| {
            fx_stages(
                &object(r#"{"s": "sine", "FX": [{"tremolosync": 2}]}"#),
                0.0,
                0.0,
                cps,
                &BundledOnly,
            )
            .expect("resolve")[0]
                .expect("stage")
                .tremolo
                .expect("tremolo")
                .frequency_hz
        };
        assert_eq!(at_cps(0.5), 1.0);
        assert_eq!(at_cps(2.0), 4.0);
    }

    /// Each `.FX(...)` stage is a complete effects pass. The stages run
    /// before the hap's own params. A stage that names no gain uses 0.8,
    /// not 1.
    #[test]
    fn fx_stages_resolve_in_order_and_carry_the_default_gain() {
        let stages =
            |json: &str| fx_stages(&object(json), 0.0, 0.0, 0.5, &BundledOnly).expect("resolve");

        let none = stages(r#"{"s": "sine"}"#);
        assert!(none.iter().all(Option::is_none), "no FX means no stages");

        let two = stages(r#"{"s": "sine", "FX": [{"coarse": 4}, {"cutoff": 500, "gain": 0.5}]}"#);
        let first = two[0].expect("first stage");
        assert_eq!(first.coarse, Some(4.0));
        assert!(
            (first.gain - 0.8).abs() < 1e-6,
            "an unnamed gain is 0.8, not 1: got {}",
            first.gain
        );
        let second = two[1].expect("second stage");
        assert!((second.gain - 0.5).abs() < 1e-6);
        assert_eq!(
            second.filters.lowpass.map(|f| f.frequency_hz),
            Some(500.0),
            "the stage carries its own filter"
        );
        assert!(two[2].is_none(), "only two stages were named");

        // Past the capacity a stage is dropped with a log, never silently
        // folded into another one.
        let many = stages(
            r#"{"s": "sine", "FX": [{"coarse": 2}, {"coarse": 3}, {"coarse": 4}, {"coarse": 5}]}"#,
        );
        assert_eq!(many[0].and_then(|s| s.coarse), Some(2.0));
        assert_eq!(many[2].and_then(|s| s.coarse), Some(4.0));
        assert_eq!(many.len(), rustel_audio::MAX_FX_STAGES);
    }

    /// `fxi` names a `.FX()` stage. The last stage has the key 'main'. With
    /// no `.FX()` chain that stage is the hap's own params, so `fxi:'main'`
    /// resolves. An index past the built stages is skipped, not refused.
    #[test]
    fn an_fxi_selects_the_chain_a_modulator_aims_at() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };
        let with_fxi = |fxi: &str| {
            format!(
                r#"{{"crush": 8, "lfo": {{"a": {{"control": "crush", "rate": 2, "fxi": {fxi}}}}}}}"#
            )
        };
        // 'main' and an absent fxi are the same chain, and carry no stage.
        for main in [r#""main""#, "null"] {
            let lfo = mods(&with_fxi(main)).0[0].expect("main-chain modulator");
            assert_eq!(lfo.fxi, None, "fxi {main} must mean the main chain");
        }
        // The stages are numbered from zero and the LAST is keyed 'main',
        // so 0 is the first `.FX()` stage and not an alias for main.
        for (fxi, slot) in [("0", 0u8), ("1", 1), (r#""2""#, 2)] {
            let lfo = mods(&with_fxi(fxi)).0[0].expect("stage modulator");
            assert_eq!(lfo.fxi, Some(slot), "fxi {fxi} must select stage {slot}");
        }
        // Past the capacity there is no stage to aim at.
        assert!(
            mods(&with_fxi("99")).0[0].is_none(),
            "an fxi past the stage capacity must be skipped"
        );
    }

    /// The gain node of each chain holds `gain × velocity`.
    #[test]
    fn a_gain_modulator_rides_gain_times_velocity_of_its_chain() {
        let base = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0)
                .expect("no refusal")
                .0[0]
                .expect("a gain modulator")
                .param_base
        };
        let lfo = |fxi: &str| format!(r#""lfo": {{"a": {{"control": "gain", "fxi": {fxi}}}}}"#);
        let main = format!(r#"{{"gain": 0.6, "velocity": 0.5, {}}}"#, lfo("null"));
        assert_eq!(base(&main), 0.3);
        let stage = format!(r#"{{"FX": [{{"gain": 0.5}}], {}}}"#, lfo("0"));
        assert_eq!(base(&stage), 0.5);
    }

    /// Each filter carries its own LFO on its frequency, built from the
    /// lp/hp/bp controls rather than from `lfo()`. It exists when any of rate,
    /// sync, depth, depthfrequency, shape or skew is set, and only when the
    /// filter itself is in the chain.
    #[test]
    fn each_filter_builds_its_own_frequency_lfo() {
        let mods = |json: &str| {
            modulator_controls(&object(json), 1.0, 0.1, 0.0, 0.5, 220.0).expect("no refusal")
        };

        // depth is relative to the cutoff: 0.8 of 500 Hz.
        let (lfos, _, _) = mods(r#"{"cutoff": 500, "lprate": 4, "lpdepth": 0.8}"#);
        let lfo = lfos[0].expect("a filter LFO");
        assert_eq!(lfo.target, rustel_audio::ModTarget::LowpassFreq);
        assert!((lfo.frequency_hz - 4.0).abs() < 1e-6);
        assert!((lfo.depth - 400.0).abs() < 1e-3, "depth was {}", lfo.depth);
        // The LFO adds to the cutoff, so its range keeps the absolute
        // frequency inside 30..20000.
        assert!((lfo.min - (-470.0)).abs() < 1e-3);
        assert!((lfo.max - 19_500.0).abs() < 1e-3);

        // sync counts cycles: 2 per cycle at cps 0.5 is 1 Hz.
        let (lfos, _, _) = mods(r#"{"cutoff": 500, "lpsync": 2}"#);
        assert!((lfos[0].expect("sync builds one").frequency_hz - 1.0).abs() < 1e-6);

        // depthfrequency is absolute and overrides the relative depth.
        let (lfos, _, _) =
            mods(r#"{"cutoff": 500, "lprate": 3, "lpdepth": 0.8, "lpdepthfrequency": 900}"#);
        assert!((lfos[0].expect("one").depth - 900.0).abs() < 1e-3);

        // No LFO control means no LFO, even with a filter present.
        let (lfos, _, _) = mods(r#"{"cutoff": 500}"#);
        assert!(lfos[0].is_none(), "a bare cutoff must not sweep");

        // ...and no filter means no LFO, however many controls are set.
        let (lfos, _, _) = mods(r#"{"lprate": 4, "lpdepth": 0.8}"#);
        assert!(
            lfos[0].is_none(),
            "lprate without a cutoff has nothing to sweep"
        );

        // Three filters, three LFOs, each on its own target.
        let (lfos, _, _) = mods(
            r#"{"cutoff": 500, "lprate": 2, "hcutoff": 200, "hprate": 3, "bandf": 700, "bprate": 4}"#,
        );
        let targets: Vec<_> = lfos.iter().flatten().map(|l| l.target).collect();
        assert_eq!(
            targets,
            vec![
                rustel_audio::ModTarget::LowpassFreq,
                rustel_audio::ModTarget::HighpassFreq,
                rustel_audio::ModTarget::BandFreq,
            ]
        );

        // A pattern's own lfo() keeps its slot; the filter takes the next.
        let (lfos, _, _) = mods(
            r#"{"cutoff": 500, "lprate": 2, "gain": 0.8, "lfo": {"a": {"control": "gain", "rate": 2}}}"#,
        );
        assert_eq!(
            lfos[0].expect("the pattern's own").target,
            rustel_audio::ModTarget::Gain
        );
        assert_eq!(
            lfos[1].expect("the filter's").target,
            rustel_audio::ModTarget::LowpassFreq
        );
    }
}

/// The policy for threads outside a [`with_diagnostic_policy`] scope. Off,
/// so a library host's stderr stays quiet unless it opts in.
static DEFAULT_DIRECT_DIAGNOSTIC_LOGGING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The most distinct notices one [`with_diagnostic_policy`] scope returns.
const MAX_COLLECTED_NOTICES: usize = 16;

thread_local! {
    /// This thread's scoped override of the process-wide default.
    static DIRECT_DIAGNOSTIC_LOGGING: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    /// The innermost scope's collected notices; `None` outside every scope.
    static COLLECTED_NOTICES: std::cell::RefCell<Option<Vec<VoiceNotice>>> =
        const { std::cell::RefCell::new(None) };
}

/// A recoverable resolver notice: part of a voice was skipped or
/// substituted and the rest still plays.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct VoiceNotice {
    /// One sentence for people.
    pub message: String,
    /// The JSON record direct logging prints, for a host that parses it.
    pub record: serde_json::Value,
}

/// Restores the enclosing scope's policy and notices, also on unwind.
struct DiagnosticPolicyGuard {
    direct: Option<bool>,
    collected: Option<Vec<VoiceNotice>>,
}

impl Drop for DiagnosticPolicyGuard {
    fn drop(&mut self) {
        DIRECT_DIAGNOSTIC_LOGGING.with(|logging| logging.set(self.direct));
        COLLECTED_NOTICES.with(|collected| collected.replace(self.collected.take()));
    }
}

/// Run `resolve` under a host's policy for the resolver's recoverable
/// [`VoiceNotice`]s.
///
/// With `direct_logging` on, each notice's JSON record is written to stderr
/// as a line as it happens. Off, nothing is written: the scope returns its
/// notices with the result, each distinct notice once, in the order first
/// raised, at most sixteen. A nested scope keeps its own notices; discarding
/// them is the caller's choice.
pub fn with_diagnostic_policy<T>(
    direct_logging: bool,
    resolve: impl FnOnce() -> T,
) -> (T, Vec<VoiceNotice>) {
    let direct = DIRECT_DIAGNOSTIC_LOGGING.with(|logging| logging.replace(Some(direct_logging)));
    let collected = COLLECTED_NOTICES.with(|collected| collected.replace(Some(Vec::new())));
    let _guard = DiagnosticPolicyGuard { direct, collected };
    let result = resolve();
    let notices = COLLECTED_NOTICES
        .with(|collected| collected.borrow_mut().take())
        .unwrap_or_default();
    (result, notices)
}

/// Set whether recoverable notices are written straight to stderr on
/// threads outside a [`with_diagnostic_policy`] scope, and whether a new
/// Session starts with direct logging. Off by default; the `rustel` command
/// line turns it on at startup.
pub fn set_default_direct_diagnostic_logging(enabled: bool) {
    DEFAULT_DIRECT_DIAGNOSTIC_LOGGING.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// The process-wide default [`set_default_direct_diagnostic_logging`] set.
pub fn default_direct_diagnostic_logging() -> bool {
    DEFAULT_DIRECT_DIAGNOSTIC_LOGGING.load(std::sync::atomic::Ordering::Relaxed)
}

/// The sounds the native voice engine makes by itself, under the names a
/// score writes in `s("…")`: the oscillators, the noises and the dedicated
/// synths. What a sound browser lists beside the sample banks. `bus`, `in`
/// and the oscillator aliases (`sin`, `saw`, …) are accepted by
/// [`is_native_synth_sound`] but are not sounds anyone browses for.
pub const NATIVE_SYNTH_SOUNDS: &[&str] = &[
    "sine",
    "triangle",
    "square",
    "sawtooth",
    "supersaw",
    "pulse",
    "sbd",
    "white",
    "pink",
    "brown",
    "crackle",
    "bytebeat",
    "zzfx",
    "z_sine",
    "z_sawtooth",
    "z_triangle",
    "z_square",
    "z_tan",
    "z_noise",
];

/// True when a sound name is generated entirely by the native voice engine.
///
/// These names need no preload when no sample bank claims them. Oscillator
/// names are case-insensitive; the dedicated synth sources keep the exact
/// lowercase names accepted by [`resolve_voice_with_samples`].
pub fn is_native_synth_sound(name: &str) -> bool {
    matches!(name, "bus" | "in")
        || NATIVE_SYNTH_SOUNDS.contains(&name)
        || [
            "sine", "sin", "triangle", "tri", "square", "sqr", "sawtooth", "saw", "user",
        ]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// True when the source node has no `frequency` param of its own, so a
/// modulator aimed at one lands on `detune` instead.
///
/// `getNodeParam` falls back to `detune ?? playbackRate` for this case. The
/// param is then in cents, not hertz, and its value is 0, which `connectLFO`
/// reads as 1. A relative depth is that many cents, not that fraction of the
/// note. The 20 Hz..24 kHz clamp does not apply, because 1 is under the 30
/// that the clamp needs.
///
/// Samples and soundfonts land here. The oscillators, the wavetable and the
/// other worklet synths all declare a real `frequency`.
fn source_pitch_rides_detune(object: &serde_json::Map<String, serde_json::Value>) -> bool {
    match object.get("s").and_then(serde_json::Value::as_str) {
        // No sound named at all is the default oscillator.
        None => false,
        Some(name) => !is_native_synth_sound(name) && !name.starts_with("wt_"),
    }
}

/// Whether recoverable notices may be written straight to stderr on this
/// thread.
///
/// The offline render path emits its own skip-and-log notices outside the
/// resolver, and they belong to the same host policy: a full-screen client
/// that disabled direct writes must not have its alternate screen corrupted
/// by a bounce either.
pub fn direct_diagnostic_logging() -> bool {
    DIRECT_DIAGNOSTIC_LOGGING
        .with(std::cell::Cell::get)
        .unwrap_or_else(default_direct_diagnostic_logging)
}

/// Print the notice's `record`, or keep the notice for the enclosing
/// [`with_diagnostic_policy`] scope. Outside every scope, with direct logging
/// off, it is dropped.
fn report_notice(message: impl Into<String>, record: serde_json::Value) {
    if direct_diagnostic_logging() {
        eprintln!("{record}");
        return;
    }
    let notice = VoiceNotice {
        message: message.into(),
        record,
    };
    COLLECTED_NOTICES.with(|collected| {
        if let Some(notices) = collected.borrow_mut().as_mut()
            && notices.len() < MAX_COLLECTED_NOTICES
            && !notices.contains(&notice)
        {
            notices.push(notice);
        }
    });
}

/// Resolve one scheduled onset. `value` is the hap's value as plain JSON
/// (`Number` = MIDI note, `String` = note name, `Object` = control map);
/// `onset_id` only labels error messages.
/// How a sound name resolves against whatever sample library the host has.
///
/// Array banks index with the shared `n`-mod rule and transpose from the
/// hap's midi against C3=36; note-keyed banks pick the closest key and
/// transpose the difference. The LIBRARY owns that math because only it knows the bank
/// shape; the resolver owns everything after (`speed`, slice, gates).
pub enum SampleResolution {
    Found {
        id: rustel_audio::SampleId,
        /// Semitones to repitch; for soundfont zones,
        /// `(100·midi − baseDetune)/100`.
        transpose: f64,
        /// The selected file's natural duration at its own rate.
        duration_secs: f64,
        /// Soundfont zone loop region in seconds, when the zone loops.
        loop_secs: Option<(f64, f64)>,
        /// 1.0 for plain samples, 0.3 for soundfont zones.
        envelope_peak: f64,
        /// A General MIDI soundfont zone rather than a sampler file. A zone
        /// plays whole at `transpose`: the sampler's own `speed`,
        /// `begin`/`end`, `unit`, `cut` and `nudge` do not reach it.
        soundfont: bool,
    },
    /// A known name whose bytes are not loaded yet. Only this onset is
    /// skipped.
    Loading,
    /// A known name whose fetch or decode failed. It stays a sample and each
    /// onset is skipped. It is never reclassified as an oscillator, which
    /// would turn one 404 into a wrong-surface refusal per hap.
    Failed,
    /// Not a sample name this host knows.
    Unknown,
}

pub trait SampleLookup {
    fn resolve(&self, s: &str, n: f64, midi: f64) -> SampleResolution;

    /// The orbit insert for a `.vst()` or `.vsti()` request, as the numbers
    /// the audio engine carries.
    ///
    /// `Ok(None)` plays the note with no plugin and no notice: the plugin
    /// loads now. `Err` plays the note with no plugin and gives the user
    /// the reason. A host with no plugins keeps this default.
    fn insert(
        &self,
        _request: &PluginRequest<'_>,
    ) -> Result<Option<rustel_audio::InsertControls>, String> {
        Err("this host has no plugins".into())
    }
}

/// What a note asks of a plugin, in the names the score wrote.
pub struct PluginRequest<'a> {
    /// True for `.vsti()`: the plugin makes the sound from the note. False
    /// for `.vst()`: the plugin changes the sound of the note.
    pub instrument: bool,
    pub name: &'a str,
    pub preset: Option<&'a str>,
    /// Parameter values by name, each from 0 to 1.
    pub params: &'a [(&'a str, f64)],
}

/// Why one onset could not be converted, independently of its diagnostic text.
///
/// Hosts may reject invalid replacement controls while preserving their existing
/// asset-loading and unknown-name policies. This classifies the first failure in
/// the ordinary resolver order; it does not validate controls hidden behind an
/// unresolved asset or inspect future pattern values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceError {
    /// Malformed or unsupported controls, including native resource ceilings.
    InvalidControl(String),
    /// The selected sample or wavetable has not finished loading.
    SampleLoading(String),
    /// The selected sample or wavetable failed to fetch or decode.
    SampleFailed(String),
    /// No currently loaded sample bank, wavetable or synth resolves this name.
    UnknownSound(String),
}

impl VoiceError {
    fn into_message(self) -> String {
        match self {
            Self::InvalidControl(message)
            | Self::SampleLoading(message)
            | Self::SampleFailed(message)
            | Self::UnknownSound(message) => message,
        }
    }
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidControl(message)
            | Self::SampleLoading(message)
            | Self::SampleFailed(message)
            | Self::UnknownSound(message) => message,
        };
        f.write_str(message)
    }
}

impl std::error::Error for VoiceError {}

impl From<String> for VoiceError {
    fn from(message: String) -> Self {
        Self::InvalidControl(message)
    }
}

/// The lookup for a host with no sample library. It resolves the bundled
/// `bd` and no other name.
pub struct BundledOnly;

impl SampleLookup for BundledOnly {
    fn resolve(&self, s: &str, _n: f64, midi: f64) -> SampleResolution {
        if s == "bd" {
            SampleResolution::Found {
                id: rustel_audio::BUNDLED_BD_SAMPLE_ID,
                transpose: midi - 36.0,
                duration_secs: f64::from(rustel_audio::BundledSample::Bd.duration_secs()),
                loop_secs: None,
                envelope_peak: 1.0,
                soundfont: false,
            }
        } else {
            SampleResolution::Unknown
        }
    }
}

pub fn resolve_voice(
    value: &serde_json::Value,
    onset_id: u64,
    duration_secs: f64,
    target_time: f64,
    sample_rate: u32,
    cps: f64,
) -> Result<rustel_audio::OnsetEvent, String> {
    resolve_voice_with_samples(
        value,
        onset_id,
        duration_secs,
        target_time,
        sample_rate,
        cps,
        &BundledOnly,
    )
}

#[allow(clippy::too_many_arguments)]
/// Say what a lane that cannot be played actually contains.
///
/// A lane whose value is a bare function is the usual cause, and `not a note:
/// "js fn #11 (cb 41)"` tells a musician nothing about their own score. It
/// means a control was named without being called - `.speed` where `.speed(2)`
/// was meant, or a bare `beat` - so `stack` reified a function into a pattern.
/// A mistake in the score rather than an engine divergence; the job is to
/// keep playing and name it.
fn explain_unplayable(value: &str, error: String) -> String {
    if rustel_core::value::is_rendered_function(value) {
        return "a lane is a function, not a sound: a control was named but \
                never called, like `.speed` where `.speed(2)` was meant"
            .to_owned();
    }
    error
}

/// The phase vocoder's latency as the reference reports it, subtracted from
/// a stretched voice's start time.
const STRETCH_LATENCY_SECS: f64 = 0.04;

pub fn resolve_voice_with_samples(
    value: &serde_json::Value,
    onset_id: u64,
    duration_secs: f64,
    target_time: f64,
    sample_rate: u32,
    cps: f64,
    samples: &dyn SampleLookup,
) -> Result<rustel_audio::OnsetEvent, String> {
    resolve_voice_at(
        value,
        onset_id,
        duration_secs,
        target_time,
        target_time,
        sample_rate,
        cps,
        samples,
    )
    .map_err(VoiceError::into_message)
}

/// Resolve a hap's value, as the scheduler and pattern queries produce it.
///
/// The mapping, failure order and result are those of
/// [`resolve_voice_with_samples_detailed`] given the value's
/// `JSON.stringify` form, with `target_time` as the musical time. A value
/// with no JSON form, such as `undefined`, is refused like any other value
/// that is not an object.
pub fn resolve_hap_value(
    value: &rustel_core::Value,
    onset_id: u64,
    duration_secs: f64,
    target_time: f64,
    sample_rate: u32,
    cps: f64,
    samples: &dyn SampleLookup,
) -> Result<rustel_audio::OnsetEvent, VoiceError> {
    let Some(json) = value.json_stringify() else {
        return Err(not_an_object(&value.show()));
    };
    let json = serde_json::from_str(&json)
        .map_err(|error| format!("the hap value's JSON form does not parse: {error}"))?;
    resolve_voice_at(
        &json,
        onset_id,
        duration_secs,
        target_time,
        target_time,
        sample_rate,
        cps,
        samples,
    )
}

/// The refusal of a hap value that is not a control map, naming the value
/// as `shown`.
fn not_an_object(shown: &str) -> VoiceError {
    format!(
        "expected hap.value to be an object, but got \"{shown}\". \
         Hint: append .note() or .s() to the end"
    )
    .into()
}

/// Resolve an onset with the same mapping and failure order as
/// [`resolve_voice_with_samples`], retaining the failure category for the host.
///
/// `target_time` is the clock time of the onset, in seconds. `cycle` is the
/// begin of the onset's whole, in cycles. The musical time of the onset is
/// `cycle / cps`. The tremolo, the LFO of each filter and each `lfo()`
/// modulator with no `retrig` take their start phase from the musical time.
/// The start of the clock does not move their phase. Each other time comes
/// from `target_time`.
///
/// An entry point with no `cycle` uses `target_time` as the musical time.
/// The two times are equal when the clock reads 0 on cycle 0.
#[allow(clippy::too_many_arguments)]
pub fn resolve_voice_with_samples_detailed(
    value: &serde_json::Value,
    onset_id: u64,
    duration_secs: f64,
    target_time: f64,
    cycle: f64,
    sample_rate: u32,
    cps: f64,
    samples: &dyn SampleLookup,
) -> Result<rustel_audio::OnsetEvent, VoiceError> {
    resolve_voice_at(
        value,
        onset_id,
        duration_secs,
        target_time,
        cycle / cps,
        sample_rate,
        cps,
        samples,
    )
}

/// [`resolve_voice_with_samples_detailed`] with the musical time, in seconds,
/// in place of the cycle.
#[allow(clippy::too_many_arguments)]
fn resolve_voice_at(
    value: &serde_json::Value,
    onset_id: u64,
    duration_secs: f64,
    target_time: f64,
    musical_time: f64,
    sample_rate: u32,
    cps: f64,
    samples: &dyn SampleLookup,
) -> Result<rustel_audio::OnsetEvent, VoiceError> {
    // A hap value that is not an object is refused by design. A bare signal
    // head reaches this: `run(3).scale("c:major")` and `irand(7).scale(...)`
    // produce string-valued haps, where `n(...)` and `note(...)` wrap theirs
    // in an object.
    //
    // strudel.cc is silent for these values, so a score that sounds here
    // would go quiet there. The refusal is not a crash: the error becomes a
    // `voice_refused` and the set plays on. The hint names the fix.
    if !value.is_object() {
        // A string interpolates bare rather than quoted in the message.
        return Err(not_an_object(&match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        }));
    }
    let mut plays_plugin = false;
    let (frequency, gain, controls, sample, wavetable, synth) = match value {
        serde_json::Value::Number(midi) => (
            midi_to_hz(
                midi.as_f64()
                    .ok_or_else(|| format!("onset {onset_id} has a non-f64 MIDI note"))?,
            )?,
            1.0,
            rustel_audio::OscillatorControls::default(),
            None,
            None,
            None,
        ),
        serde_json::Value::String(note) => (
            note_to_hz(note).map_err(|error| explain_unplayable(note, error))?,
            1.0,
            rustel_audio::OscillatorControls::default(),
            None,
            None,
            None,
        ),
        serde_json::Value::Object(object) => {
            let gain = optional_f64(object.get("gain"), "gain")?.unwrap_or(0.8);
            let velocity = optional_f64(object.get("velocity"), "velocity")?.unwrap_or(1.0);
            let postgain = optional_f64(object.get("postgain"), "postgain")?.unwrap_or(1.0);
            let pan = optional_f64(object.get("pan"), "pan")?
                .map(|pan| checked_f32(pan.clamp(0.0, 1.0), "pan"))
                .transpose()?;
            // With `.vsti()` the plugin makes the sound: the note needs no
            // sample, wavetable or synth of the engine.
            plays_plugin = object.contains_key("vsti");
            let wavetable = if plays_plugin {
                None
            } else {
                wavetable_controls(object, samples, cps)?
            };
            let sample = if plays_plugin || wavetable.is_some() {
                None
            } else {
                sample_controls(object, samples)?
            };
            let distort = distort_controls(object)?;
            let limit = limit_controls(object)?;
            let delay = delay_controls(object, cps)?;
            let duck = duck_controls(object)?;
            let reverb = reverb_controls(object, samples)?;
            let dry = optional_f64(object.get("dry"), "dry")?
                .map(|d| checked_f32(d, "dry"))
                .transpose()?;
            let stretch = optional_f64(object.get("stretch"), "stretch")?
                .map(|s| checked_f32(s, "stretch"))
                .transpose()?;
            let fm = fm_controls(object)?;
            // Oscillator defaults are [0.001, 0.05, 0.6, 0.01].
            // Bus receivers keep full sustain: [0.001, 0.05, 1, 0.01].
            // Samples and wavetables use [0.001, 0.001, 1, 0.01].
            let envelope = if sample.is_some() || wavetable.is_some() {
                sample_envelope(object)?
            } else {
                synth_envelope(object)?
            };
            // Resolved BEFORE the modulators: an `fmh` modulator rides an
            // operator frequency, which is a multiple of this.
            let frequency = if sample.is_some() {
                0.0
            } else {
                match optional_f64(object.get("freq"), "freq")? {
                    // `freq` is gated by truthiness: an explicit 0 falls
                    // through to the note path.
                    Some(freq) if freq != 0.0 => checked_frequency(freq)?,
                    _ => {
                        // `n` selects a sound variant; this path takes pitch
                        // from `note`. The default is MIDI 29 (F1) for `sbd`
                        // and MIDI 36 (C2) for other synths.
                        let is_sbd =
                            object.get("s").and_then(serde_json::Value::as_str) == Some("sbd");
                        let default_note =
                            serde_json::Value::from(if is_sbd { 29.0 } else { 36.0 });
                        // Preserve `note || defaultNote` truthiness: a zero,
                        // empty or false note selects the synth's default.
                        // The sampler handles numeric MIDI 0 literally.
                        let falsy = |value: &serde_json::Value| match value {
                            serde_json::Value::Number(number) => {
                                number.as_f64().is_none_or(|note| note == 0.0)
                            }
                            serde_json::Value::String(text) => text.is_empty(),
                            serde_json::Value::Null => true,
                            serde_json::Value::Bool(flag) => !flag,
                            _ => false,
                        };
                        // A bare `value` key does not supply a note pitch.
                        let note = object
                            .get("note")
                            .filter(|value| !falsy(value))
                            .unwrap_or(&default_note);
                        match note {
                            serde_json::Value::Number(number) => {
                                midi_to_hz(number.as_f64().ok_or_else(|| {
                                    format!("onset {onset_id} has a non-f64 MIDI note")
                                })?)?
                            }
                            serde_json::Value::String(note) => note_to_hz(note)?,
                            _ => {
                                return Err(format!(
                                    "onset {onset_id} has a non-scalar note control"
                                )
                                .into());
                            }
                        }
                    }
                }
            };
            // `freq *= 2^octave` - applied after either route, so an
            // explicit `freq` is transposed too, and a sample keeps the 0.0
            // that marks it as pitched by playback rate.
            let frequency = match optional_f64(object.get("octave"), "octave")? {
                Some(octave) if frequency != 0.0 => {
                    checked_frequency(frequency * 2f64.powf(octave))?
                }
                _ => frequency,
            };
            let (lfos, envs, bus_mods) = modulator_controls(
                object,
                duration_secs,
                envelope.release_secs,
                musical_time,
                cps,
                frequency,
            )?;
            let synth = if plays_plugin {
                None
            } else {
                synth_source(object, duration_secs, target_time)?
            };
            // One cycle is one bar of 4 quarter notes.
            let clocked = |plugin: rustel_audio::InsertControls| {
                plugin.with_clock(musical_time * cps * 4.0, (cps * 240.0) as f32)
            };
            let orbit = optional_f64(object.get("orbit"), "orbit")?.unwrap_or(1.0);
            if !(0.0..16.0).contains(&orbit) {
                return Err(format!("orbit {orbit} is outside the supported 0..16 range").into());
            }
            let modulator_release = optional_f64(object.get("release"), "release")?
                .unwrap_or(0.01)
                .max(optional_f64(object.get("FXrelease"), "FXrelease")?.unwrap_or(0.0));
            let controls = rustel_audio::OscillatorControls {
                preview_epoch: 0,
                choke_only: false,
                piano: false,
                live_controls: [0; 2],
                // `if (noise) { getNoiseMix(...) }` - absent or zero skips the
                // mix entirely rather than crossfading at zero.
                noise: checked_f32(
                    optional_f64(object.get("noise"), "noise")?.unwrap_or(0.0),
                    "noise",
                )?,
                bus_mods,
                // Buses are a fixed 0..MAX_BUSES. Refuse an out-of-range bus.
                // A clamp would fold bus 20 onto bus 15 and silently mix two
                // separate sends.
                bus: match optional_f64(object.get("bus"), "bus")? {
                    Some(bus) if !(0.0..rustel_audio::MAX_BUSES as f64).contains(&bus) => {
                        return Err(format!(
                            "bus {bus} is outside the supported 0..{} range",
                            rustel_audio::MAX_BUSES
                        )
                        .into());
                    }
                    other => other.map(|bus| bus as u8),
                },
                channels: channel_route(object)?,
                busgain: checked_f32(
                    optional_f64(object.get("busgain"), "busgain")?.unwrap_or(1.0),
                    "busgain",
                )?,
                waveform: if plays_plugin
                    || sample.is_some()
                    || wavetable.is_some()
                    || synth.is_some()
                {
                    rustel_audio::Waveform::Sine
                } else {
                    oscillator_waveform(object)?
                },
                // ZzFX bakes the envelope controls into its generated buffer.
                // A neutral outer envelope avoids shaping the note twice.
                envelope: if matches!(synth, Some(rustel_audio::SynthSource::ZzFx { .. })) {
                    rustel_audio::Envelope {
                        attack_secs: 0.0,
                        decay_secs: 0.0,
                        sustain: 1.0,
                        release_secs: 0.0,
                    }
                } else {
                    envelope
                },
                // Modulator lifetime uses raw `release` (default 0.01 s),
                // extended by `FXrelease`, rather than the resolved amplitude
                // envelope's source-specific release.
                modulator_release_secs: checked_f32(modulator_release, "modulator release")?,
                // Gate times retain AudioParam's f32 precision while the
                // render clock stays f64. Rounding affects which quantum
                // opens or closes the gate.
                worklet_begin_secs: checked_f32(target_time, "worklet begin")?,
                lfo_end_secs: checked_f32(
                    target_time + duration_secs + modulator_release,
                    "lfo end",
                )?,
                filter_lfo_end_secs: checked_f32(target_time + duration_secs, "filter lfo end")?,
                velocity: checked_f32(velocity, "velocity")?,
                postgain: checked_f32(postgain, "postgain")?,
                pan,
                filters: filter_controls(object)?,
                distort,
                limit,
                delay,
                duck,
                reverb,
                dry,
                stretch,
                fm,
                orbit: orbit as u8,
                insert_orbit: None,
                effects: effect_controls(object, samples).map(|effect| effect.map(clocked)),
                instrument: object
                    .get("vsti")
                    .and_then(|plugin| plugin_controls(plugin, "vsti", samples))
                    .map(clocked),
                lfos,
                envs,
                phaser: phaser_controls(object, target_time)?,
                // The shaper is built whenever `transient` is
                // present, letting `transsustain` default to 0.
                fx_stages: fx_stages(object, target_time, musical_time, cps, samples)?,
                transient: transient_controls(object)?,
                tremolo: tremolo_controls(object, musical_time, cps)?,
                vowel: vowel_controls(object)?,
                vibrato: match optional_f64(object.get("vib"), "vib")? {
                    Some(vib) if vib > 0.0 => Some(rustel_audio::VibratoControls {
                        freq_hz: checked_f32(vib, "vib")?,
                        // vibmod is in semitones.
                        cents: checked_f32(
                            optional_f64(object.get("vibmod"), "vibmod")?.unwrap_or(0.5) * 100.0,
                            "vibmod",
                        )?,
                    }),
                    _ => None,
                },
                pitch_env: pitch_env_controls(object)?,
                djf: optional_f64(object.get("djf"), "djf")?
                    .map(|v| checked_f32(v, "djf"))
                    .transpose()?,
                partials: if sample.is_some() || wavetable.is_some() || synth.is_some() {
                    None
                } else {
                    partials_controls(object)?
                },
                compressor: compressor_controls(object)?,
                coarse: optional_f64(object.get("coarse"), "coarse")?
                    .map(|c| checked_f32(c, "coarse"))
                    .transpose()?,
                crush: optional_f64(object.get("crush"), "crush")?
                    .map(|c| checked_f32(c, "crush"))
                    .transpose()?,
                shape: match optional_f64(object.get("shape"), "shape")? {
                    None => None,
                    Some(shape) => Some(rustel_audio::ShapeControls {
                        shape: checked_f32(shape, "shape")?,
                        postgain: checked_f32(
                            optional_f64(object.get("shapevol"), "shapevol")?.unwrap_or(1.0),
                            "shapevol",
                        )?,
                    }),
                },
            };
            (frequency, gain, controls, sample, wavetable, synth)
        }
        _ => {
            return Err(format!(
                "onset {onset_id} is not an explicit note/frequency control; scalar audio supports note(...), n(...), or freq(...) only"
            ).into());
        }
    };

    // An instrument plugin makes the sound of the note. The plugin gets the
    // pitch, the level and the length of the note. The engine voice is
    // silent, also while the plugin loads.
    let (gain, controls) = if plays_plugin {
        let mut controls = controls;
        controls.instrument = controls.instrument.map(|instrument| {
            instrument.with_note(rustel_audio::InsertNote {
                pitch: (69.0 + 12.0 * (frequency / 440.0).log2()) as f32,
                velocity: (gain * f64::from(controls.velocity)).clamp(0.0, 1.0) as f32,
                frames: (duration_secs * f64::from(sample_rate))
                    .ceil()
                    .clamp(1.0, f64::from(u32::MAX)) as u32,
            })
        });
        (0.0, controls)
    } else {
        (gain, controls)
    };

    // `clip` (public alias `legato`) belongs to Hap duration semantics; the
    // scheduler carries that effective duration here. Looking for a synthetic
    // `legato` value in the event object is a false green: the registered
    // alias writes `clip`.
    if !gain.is_finite()
        || gain.abs() > f64::from(f32::MAX)
        || !duration_secs.is_finite()
        || duration_secs < 0.0
        || duration_secs > f64::from(f32::MAX)
    {
        return Err(format!(
            "onset {onset_id} has an invalid gain/duration ({gain}/{duration_secs})"
        )
        .into());
    }
    if sample.is_none() && frequency >= f64::from(sample_rate) / 2.0 {
        return Err(format!(
            "onset {onset_id} frequency {frequency}Hz reaches or exceeds the {sample_rate}Hz Nyquist limit"
        ).into());
    }
    // Compensate the main stretch stage's latency before converting the
    // onset to frames: the reference's 40 ms, plus the frames this vocoder
    // trails a render-quantum one by, so the shifted voice lands where the
    // reference's does. Separate `.FX()` stretch stages keep their delay.
    // A compensated onset before the render begins starts at time zero.
    let target_time = if value.get("stretch").is_some_and(|value| !value.is_null()) {
        let lag_secs =
            f64::from(rustel_audio::stretch::QUANTUM_LAG_FRAMES) / f64::from(sample_rate);
        (target_time - STRETCH_LATENCY_SECS - lag_secs).max(0.0)
    } else {
        target_time
    };
    let exact_frame = target_time * f64::from(sample_rate);
    // Snap near-integer frame positions before ceil() so floating-point
    // noise cannot delay an on-grid onset by a frame. Other onsets use the
    // first frame at or after their time and retain the fractional lead.
    let nearest = exact_frame.round();
    let exact_frame = if (exact_frame - nearest).abs() < 1e-6 {
        nearest
    } else {
        exact_frame
    };
    let onset_frame = exact_frame.ceil();
    if !onset_frame.is_finite() || onset_frame < 0.0 || onset_frame > u64::MAX as f64 {
        return Err(
            format!("onset {onset_id} has an unrepresentable target time {target_time}").into(),
        );
    }
    let event = rustel_audio::OnsetEvent::new(
        onset_frame as u64,
        frequency as f32,
        gain as f32,
        duration_secs as f32,
    )
    .with_onset_lead((onset_frame - exact_frame) as f32)
    .with_controls(controls);
    let mut event = if let Some(sample) = sample {
        event.with_sample(sample)
    } else {
        event
    };
    event.wavetable = wavetable;
    event.synth = synth;
    Ok(event)
}

/// Resolve the dedicated synth sources with their pinned trigger defaults.
fn synth_source(
    object: &serde_json::Map<String, serde_json::Value>,
    duration_secs: f64,
    target_time: f64,
) -> Result<Option<rustel_audio::SynthSource>, String> {
    let Some(name) = object.get("s").and_then(|s| s.as_str()) else {
        return Ok(None);
    };
    match name {
        // `density` matters only to crackle.
        "white" | "pink" | "brown" | "crackle" => {
            let kind = match name {
                "white" => 0u8,
                "pink" => 1,
                "brown" => 2,
                _ => 3,
            };
            Ok(Some(rustel_audio::SynthSource::Noise {
                kind,
                density: checked_f32(
                    optional_f64(object.get("density"), "density")?.unwrap_or(0.02),
                    "density",
                )?,
            }))
        }
        // Seven names, one assembly. The full parameter story lives with
        // the generator in `rustel_audio::zzfx`.
        "zzfx" | "z_sine" | "z_sawtooth" | "z_triangle" | "z_square" | "z_tan" | "z_noise" => {
            let mut params = rustel_audio::zzfx::ZzfxParams::default();
            if let Some(serde_json::Value::Array(list)) = object.get("zzfx") {
                // A raw `zzfx([...])` array overrides EVERYTHING: shorter
                // arrays keep the generator's own defaults for the rest,
                // longer ones are ignored past twenty.
                for (index, entry) in list.iter().enumerate().take(20) {
                    let value = entry
                        .as_f64()
                        .ok_or_else(|| format!("zzfx array entry {index} must be a number"))?;
                    params.set_raw(index, value);
                }
            } else {
                let get = |key: &str| -> Result<Option<f64>, String> {
                    optional_f64(object.get(key), key)
                };
                let attack = get("attack")?.unwrap_or(0.0);
                let decay = get("decay")?.unwrap_or(0.0);
                params.volume = 0.25;
                // `zrand` defaults to 0 here - deterministic - even though
                // the generator's own default is 0.05.
                params.randomness = get("zrand")?.unwrap_or(0.0);
                // `freq ?? midiToFreq(note ?? 36)`: zzfx resolves its own
                // pitch, and its default is MIDI 36 (C2). `!freq` also sends
                // an explicit 0 through the note path.
                match get("freq")?.filter(|freq| *freq != 0.0) {
                    Some(freq) => params.frequency = freq,
                    None => {
                        let midi = match object.get("note") {
                            Some(serde_json::Value::String(note)) => {
                                Some(rustel_core::util::note_to_midi(note, 3)?)
                            }
                            Some(serde_json::Value::Number(midi)) => midi.as_f64(),
                            None => Some(36.0),
                            // Anything else leaves freq unset and the
                            // generator falls back to its 220.
                            Some(_) => None,
                        };
                        if let Some(midi) = midi {
                            params.frequency = rustel_core::util::midi_to_freq(midi);
                        }
                    }
                }
                params.attack = attack;
                params.decay = decay;
                // `sustain` names the sustain VOLUME (default 0.8); the
                // sustain TIME is whatever the hap leaves after attack+decay.
                params.sustain = (duration_secs - attack - decay).max(0.0);
                params.sustain_volume = get("sustain")?.unwrap_or(0.8);
                params.release = get("release")?.unwrap_or(0.1);
                params.slide = get("slide")?.unwrap_or(0.0);
                params.delta_slide = get("deltaSlide")?.unwrap_or(0.0);
                params.pitch_jump = get("pitchJump")?.unwrap_or(0.0);
                params.pitch_jump_time = get("pitchJumpTime")?.unwrap_or(0.0);
                // `lfo` doubles as the modulator map elsewhere; only a NUMBER
                // is zzfx's repeat time.
                params.repeat_time = object
                    .get("lfo")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                // The docs say `.noise()`, but the control is `znoise` - the
                // `noise` key is the oscillator pink-mix control and zzfx
                // never sees it.
                params.noise = get("znoise")?.unwrap_or(0.0);
                params.modulation = get("zmod")?.unwrap_or(0.0);
                params.bit_crush = get("zcrush")?.unwrap_or(0.0);
                params.delay = get("zdelay")?.unwrap_or(0.0);
                params.tremolo = get("tremolo")?.unwrap_or(0.0);
                // `['sine','triangle','sawtooth','tan','noise'].indexOf(s) || 0`
                // keeps a miss: the miss is -1, and `-1 || 0` is -1. `z_square`
                // and bare `zzfx` run shape -1, which lands on the triangle
                // branch. Square is that triangle through curve 0.
                let stripped = name.strip_prefix("z_").unwrap_or(name);
                params.shape = match stripped {
                    "sine" => 0.0,
                    "triangle" => 1.0,
                    "sawtooth" => 2.0,
                    "tan" => 3.0,
                    "noise" => 4.0,
                    _ => -1.0,
                };
                params.shape_curve = if stripped == "square" {
                    0.0
                } else {
                    get("curve")?.unwrap_or(1.0)
                };
            }
            Ok(Some(rustel_audio::SynthSource::ZzFx { params }))
        }
        "bytebeat" => {
            // `defaultBeats[n % defaultBeats.length]`: `n` picks the built-in
            // expression when the score supplies none of its own. A custom
            // `byteBeatExpression` arrives as literal text (the transpiler
            // exempts `bbexpr` from mini-notation) and is compiled here, on
            // the producer side, into the fixed program the callback walks.
            // An expression the compiler cannot take is refused with the
            // reason. It never falls through to a built-in.
            let n = optional_f64(object.get("n"), "n")?.unwrap_or(0.0);
            let count = f64::from(rustel_audio::BYTEBEAT_EXPRESSIONS);
            let program = match object.get("byteBeatExpression") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(text)) => {
                    Some(rustel_audio::bytebeat::compile(text)?)
                }
                Some(other) => {
                    // `'0'` and friends numerify under miniAllStrings, and the
                    // worklet's `new Function` body stringifies them back.
                    Some(rustel_audio::bytebeat::compile(&other.to_string())?)
                }
            };
            // Presence is semantic: the worklet resets its counter only when
            // this value is non-null, then floors it into `initialOffset`.
            let start_offset =
                optional_f64(object.get("byteBeatStartTime"), "byteBeatStartTime")?.map(f64::floor);
            Ok(Some(rustel_audio::SynthSource::ByteBeat {
                expression: n.rem_euclid(count) as u8,
                program,
                start_offset,
            }))
        }
        "in" => {
            // The audio input: `n` picks the channel of the selected device,
            // mono, one channel per voice. No length - the event is the
            // window, as for a synth.
            let channel = optional_f64(object.get("n"), "n")?.unwrap_or(0.0);
            if !(0.0..rustel_audio::input::MAX_INPUT_CHANNELS as f64).contains(&channel) {
                return Err(format!(
                    "in:{channel} is outside the supported 0..{} input channels",
                    rustel_audio::input::MAX_INPUT_CHANNELS
                ));
            }
            Ok(Some(rustel_audio::SynthSource::Input {
                channel: channel as u8,
            }))
        }
        "bus" => {
            // The bus number rides `n` (`n ?? 0`), not the name.
            let bus = optional_f64(object.get("n"), "n")?.unwrap_or(0.0);
            if !(0.0..rustel_audio::MAX_BUSES as f64).contains(&bus) {
                return Err(format!(
                    "bus {bus} is outside the supported 0..{} range",
                    rustel_audio::MAX_BUSES
                ));
            }
            Ok(Some(rustel_audio::SynthSource::Bus { bus: bus as u8 }))
        }
        "supersaw" => {
            let unison = optional_f64(object.get("unison"), "unison")?.unwrap_or(5.0);
            let voices = unison.clamp(1.0, 100.0);
            if voices.ceil() > 32.0 {
                return Err(format!(
                    "unison {voices} exceeds the native supersaw's 32-voice ceiling"
                ));
            }
            // `detune = detune ?? n ?? 0.18`.
            let detune = match optional_f64(object.get("detune"), "detune")? {
                Some(detune) => detune,
                None => optional_f64(object.get("n"), "n")?.unwrap_or(0.18),
            };
            let spread = optional_f64(object.get("spread"), "spread")?.unwrap_or(0.6);
            Ok(Some(rustel_audio::SynthSource::Supersaw {
                voices: voices as f32,
                freqspread: checked_f32(detune, "detune")?,
                panspread: if voices > 1.0 {
                    checked_f32(spread.clamp(0.0, 1.0), "spread")?
                } else {
                    0.0
                },
            }))
        }
        "pulse" => {
            let pulsewidth = optional_f64(object.get("pw"), "pw")?.unwrap_or(0.5);
            Ok(Some(rustel_audio::SynthSource::Pulse {
                pulsewidth: checked_f32(pulsewidth, "pw")?,
                width_lfo: pulse_width_lfo_controls(object, target_time)?,
            }))
        }
        "sbd" => {
            let decay = optional_f64(object.get("decay"), "decay")?.unwrap_or(0.5);
            let pdecay = optional_f64(object.get("pdecay"), "pdecay")?.unwrap_or(0.5);
            let penv = optional_f64(object.get("penv"), "penv")?.unwrap_or(36.0);
            // `end = holdEnd + 0.01` shortened by clip×duration.
            let mut stop = decay + 0.01;
            if let Some(clip) = optional_f64(object.get("clip"), "clip")? {
                stop = stop.min(clip * duration_secs);
            }
            Ok(Some(rustel_audio::SynthSource::Sbd {
                decay_secs: checked_f32(decay, "decay")?,
                pdecay_secs: checked_f32(pdecay.max(0.001), "pdecay")?,
                penv_semitones: checked_f32(penv, "penv")?,
                stop_secs: checked_f32(stop.max(0.011), "clip")?,
            }))
        }
        _ => Ok(None),
    }
}

/// Pulse LFO defaults: naming only `pwrate` supplies sweep 0.3; naming only
/// `pwsweep` supplies rate 1. A zero sweep builds no LFO node.
fn pulse_width_lfo_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    target_time: f64,
) -> Result<Option<rustel_audio::PulseWidthLfoControls>, String> {
    let rate = optional_f64(object.get("pwrate"), "pwrate")?;
    let sweep = optional_f64(object.get("pwsweep"), "pwsweep")?;
    let (rate, sweep) = match (rate, sweep) {
        (None, None) => return Ok(None),
        (Some(rate), None) => (rate, 0.3),
        (None, Some(sweep)) => (1.0, sweep),
        (Some(rate), Some(sweep)) => (rate, sweep),
    };
    if sweep == 0.0 {
        return Ok(None);
    }
    Ok(Some(rustel_audio::PulseWidthLfoControls {
        frequency_hz: checked_f32(rate, "pwrate")?,
        depth: checked_f32(sweep, "pwsweep")?,
        time_secs: checked_f32(target_time, "pulse-width LFO time")?,
    }))
}

/// The SIMPLE fm chain (`fmi`/`fm` + `fmh` + optional fm ADSR). The
/// 8-operator matrix refuses loudly.
/// The ADSR an FM operator applies to its own depth, from whichever of the
/// four values the pattern set. Defaults follow the per-kind envelope defaults for
/// the FM path.
fn fm_envelope(values: [Option<f64>; 4]) -> Result<rustel_audio::Envelope, String> {
    let (attack, decay, sustain, release) = adsr_values(values, (0.001, 0.001, 1.0, 0.01));
    Ok(rustel_audio::Envelope {
        attack_secs: checked_f32(attack, "fm attack")?,
        decay_secs: checked_f32(decay, "fm decay")?,
        sustain: checked_f32(sustain, "fm sustain")?,
        release_secs: checked_f32(release, "fm release")?,
    })
}

fn fm_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::FmControls>, String> {
    const { assert!(rustel_audio::MAX_FM_OPERATORS <= 9) };
    // `applyFM` walks the whole i x j matrix. The diagonal `i == j + 1` is
    // spelled `fmi{i}` - the plain chain, operator i into the one below - and
    // everything else is `fmi{i}{j}`, an arbitrary connection. Operator 1 is
    // unsuffixed throughout, and target 0 is the carrier's own frequency.
    let suffix = |n: u8| if n == 1 { String::new() } else { n.to_string() };

    // `fmwave{n}` names the modulator's own shape: any oscillator type or
    // noise name, defaulting to sine. An unknown name is a score error
    // rather than a silently wrong sine.
    let fm_waveform = |n: u8| -> Result<rustel_audio::FmWave, String> {
        let key = format!("fmwave{}", suffix(n));
        let Some(value) = object.get(&key).filter(|value| !value.is_null()) else {
            return Ok(rustel_audio::FmWave::Sine);
        };
        let Some(name) = value.as_str() else {
            return Err(format!("{key} must be a waveform name"));
        };
        Ok(match name {
            "sine" => rustel_audio::FmWave::Sine,
            "triangle" | "tri" => rustel_audio::FmWave::Triangle,
            "square" => rustel_audio::FmWave::Square,
            "sawtooth" | "saw" => rustel_audio::FmWave::Sawtooth,
            "white" => rustel_audio::FmWave::Noise(0),
            "pink" => rustel_audio::FmWave::Noise(1),
            "brown" => rustel_audio::FmWave::Noise(2),
            "crackle" => rustel_audio::FmWave::Noise(3),
            other => return Err(format!("unsupported {key}: {other}")),
        })
    };

    let mut routes = [None; rustel_audio::MAX_FM_ROUTES];
    let mut used = [false; rustel_audio::MAX_FM_OPERATORS];
    let mut count = 0usize;
    for i in 1..=(rustel_audio::MAX_FM_OPERATORS as u8) {
        for j in 0..=(rustel_audio::MAX_FM_OPERATORS as u8) {
            let diagonal = i == j + 1;
            // The fixed operator range makes every route key at most five
            // ASCII bytes; absent routes need no temporary String allocation.
            let mut key = *b"fmi00";
            key[3] = b'0' + i;
            key[4] = b'0' + j;
            let len = if diagonal {
                if i == 1 { 3 } else { 4 }
            } else {
                5
            };
            let control = std::str::from_utf8(&key[..len]).expect("FM route keys are ASCII");
            let Some(amount) = optional_f64(object.get(control), control)? else {
                continue;
            };
            // `if (!amt) continue` - a zero connection is never made.
            if amount == 0.0 {
                continue;
            }
            if count == routes.len() {
                return Err(format!(
                    "FM matrix declares more than {} connections",
                    routes.len()
                ));
            }
            routes[count] = Some(rustel_audio::FmRoute {
                source: i,
                target: j,
                amount: checked_f32(amount, control)?,
                // Only the diagonal has an `fmi{n}` name a modulator can
                // address, so only it carries a modulation slot.
                mod_slot: diagonal.then_some(i - 1),
            });
            count += 1;
            // An operator is built the first time a route names it, at
            // either end. Target 0 is the carrier, which is not an operator.
            used[usize::from(i) - 1] = true;
            if j > 0 {
                used[usize::from(j) - 1] = true;
            }
        }
    }
    if count == 0 {
        return Ok(None);
    }

    let mut operators = [None; rustel_audio::MAX_FM_OPERATORS];
    for (slot, operator) in operators.iter_mut().enumerate() {
        if !used[slot] {
            continue;
        }
        let n = slot as u8 + 1;
        let s = suffix(n);
        let values = [
            optional_f64(object.get(&format!("fmattack{s}")), "fmattack")?,
            optional_f64(object.get(&format!("fmdecay{s}")), "fmdecay")?,
            optional_f64(object.get(&format!("fmsustain{s}")), "fmsustain")?,
            optional_f64(object.get(&format!("fmrelease{s}")), "fmrelease")?,
        ];
        *operator = Some(rustel_audio::FmOperator {
            harmonicity: checked_f32(
                optional_f64(object.get(&format!("fmh{s}")), "fmh")?.unwrap_or(1.0),
                "fmh",
            )?,
            waveform: fm_waveform(n)?,
            env: if values.iter().any(Option::is_some) {
                Some(fm_envelope(values)?)
            } else {
                None
            },
            env_exponential: object
                .get(&format!("fmenv{s}"))
                .and_then(serde_json::Value::as_str)
                .is_none_or(|kind| kind != "lin" && kind != "linear"),
        });
    }

    Ok(Some(rustel_audio::FmControls { operators, routes }))
}

/// The whole bank rule: `s` becomes `{bank}_{s}`.
///
/// Returns `None` when there is no bank to apply, so the caller keeps the name
/// it already had. Alias banks (tr909 -> RolandTR909) resolve inside the
/// library lookup, after registration lowercased the keys.
pub fn bank_prefixed(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<String>, String> {
    if name.is_empty() {
        return Ok(None);
    }
    match object.get("bank") {
        Some(serde_json::Value::String(bank)) => Ok(Some(format!("{bank}_{name}"))),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(other) => match other.as_f64() {
            // `.bank('9000')` numerifies under miniAllStrings; the prefix
            // rule stringifies it right back.
            Some(bank) => Ok(Some(format!("{bank}_{name}"))),
            _ => Err("bank must be a scalar bank name".to_owned()),
        },
    }
}

/// `wt_`-named sounds route to the wavetable oscillator.
fn wavetable_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    samples: &dyn SampleLookup,
    cps: f64,
) -> Result<Option<rustel_audio::WavetableControls>, VoiceError> {
    let Some(sound) = object.get("s") else {
        return Ok(None);
    };
    let name = sound
        .as_str()
        .ok_or_else(|| "s must be a scalar sound name".to_owned())?;
    // The bank prefix goes on first: `s` becomes `{bank}_{s}` before any
    // dispatch on the name. `s("basique").bank("wt_digital")` is then
    // `wt_digital_basique` when this function tests for a wavetable. A test
    // on the raw `s` would send that spelling down the sample path and play
    // the table's raw PCM as a one-shot. The documented example for `warp`
    // uses the bank form.
    let banked;
    let name = match bank_prefixed(object, name)? {
        Some(prefixed) => {
            banked = prefixed;
            banked.as_str()
        }
        None => name,
    };
    if !name.starts_with("wt_") {
        return Ok(None);
    }
    let n = optional_f64(object.get("n"), "n")?.unwrap_or(0.0);
    // Table selection uses the same bank index math; pitch comes from the
    // frequency path, so the midi argument only matters for note-keyed
    // banks, which wavetable collections do not use.
    let id = match samples.resolve(name, n, 36.0) {
        SampleResolution::Found { id, .. } => id,
        SampleResolution::Loading => {
            return Err(VoiceError::SampleLoading(format!(
                "wavetable \"{name}:{n}\" is still loading"
            )));
        }
        SampleResolution::Failed => {
            return Err(VoiceError::SampleFailed(format!(
                "wavetable \"{name}:{n}\" failed to load"
            )));
        }
        SampleResolution::Unknown => {
            return Err(VoiceError::UnknownSound(format!(
                "unknown wavetable \"{name}\""
            )));
        }
    };
    let unison = optional_f64(object.get("unison"), "unison")?.unwrap_or(1.0);
    let voices = unison.max(1.0);
    if voices.ceil() > 32.0 {
        return Err(
            format!("unison {voices} exceeds the native wavetable's 32-voice ceiling").into(),
        );
    }
    // Position envelope: amount defaults to 0.5 when any wt-ADSR value is
    // present, 0 otherwise; ADSR resolution uses the linear defaults
    // [0, 0.5, 0, 0.1].
    let pos_values = [
        optional_f64(object.get("wtattack"), "wtattack")?,
        optional_f64(object.get("wtdecay"), "wtdecay")?,
        optional_f64(object.get("wtsustain"), "wtsustain")?,
        optional_f64(object.get("wtrelease"), "wtrelease")?,
    ];
    let pos_env_amount = match optional_f64(object.get("wtenv"), "wtenv")? {
        Some(amount) => amount,
        None if pos_values.iter().any(Option::is_some) => 0.5,
        None => 0.0,
    };
    let (pos_attack, pos_decay, pos_sustain, pos_release) =
        adsr_values(pos_values, (0.0, 0.5, 0.0, 0.1));
    // Position LFO: depth defaults to 0.5 when any LFO input (frequency /
    // shape / skew) is present, 0 otherwise; wtsync overrides wtrate as
    // cycles-per-second-relative.
    let wtrate = optional_f64(object.get("wtrate"), "wtrate")?;
    let wtsync = optional_f64(object.get("wtsync"), "wtsync")?;
    let wtshape = object.get("wtshape").filter(|value| !value.is_null());
    let wtskew = optional_f64(object.get("wtskew"), "wtskew")?;
    let has_lfo_input =
        wtrate.is_some() || wtsync.is_some() || wtshape.is_some() || wtskew.is_some();
    let lfo_depth = match optional_f64(object.get("wtdepth"), "wtdepth")? {
        Some(depth) => depth,
        None if has_lfo_input => 0.5,
        None => 0.0,
    };
    let lfo_rate = match wtsync {
        Some(sync) => cps * sync,
        None => wtrate.unwrap_or(1.0),
    };
    let lfo_shape = match wtshape {
        None => 0u8,
        Some(serde_json::Value::Number(shape)) => {
            (shape.as_f64().unwrap_or(0.0).rem_euclid(5.0)) as u8
        }
        Some(serde_json::Value::String(shape)) => match shape.as_str() {
            "tri" | "triangle" => 0,
            "sine" => 1,
            "ramp" => 2,
            "saw" => 3,
            "square" => 4,
            other => {
                return Err(format!("unsupported wavetable LFO shape \"{other}\"").into());
            }
        },
        Some(_) => {
            return Err(VoiceError::InvalidControl(
                "wtshape must be a name or number".into(),
            ));
        }
    };
    let phaserand = match optional_f64(object.get("wtphaserand"), "wtphaserand")? {
        Some(value) => {
            if value != 0.0 {
                1.0
            } else {
                0.0
            }
        }
        None if voices > 1.0 => 1.0,
        None => 0.0,
    };
    // Warp: the same envelope-and-LFO pair as the position above - only the
    // control prefix differs.
    let warp_values = [
        optional_f64(object.get("warpattack"), "warpattack")?,
        optional_f64(object.get("warpdecay"), "warpdecay")?,
        optional_f64(object.get("warpsustain"), "warpsustain")?,
        optional_f64(object.get("warprelease"), "warprelease")?,
    ];
    let warp_env_amount = match optional_f64(object.get("warpenv"), "warpenv")? {
        Some(amount) => amount,
        None if warp_values.iter().any(Option::is_some) => 0.5,
        None => 0.0,
    };
    let (warp_attack, warp_decay, warp_sustain, warp_release) =
        adsr_values(warp_values, (0.0, 0.5, 0.0, 0.1));
    let warprate = optional_f64(object.get("warprate"), "warprate")?;
    let warpsync = optional_f64(object.get("warpsync"), "warpsync")?;
    let warpshape = object.get("warpshape").filter(|value| !value.is_null());
    let warpskew = optional_f64(object.get("warpskew"), "warpskew")?;
    let warp_has_lfo_input =
        warprate.is_some() || warpsync.is_some() || warpshape.is_some() || warpskew.is_some();
    let warp_lfo_depth = match optional_f64(object.get("warpdepth"), "warpdepth")? {
        Some(depth) => depth,
        None if warp_has_lfo_input => 0.5,
        None => 0.0,
    };
    let warp_lfo_rate = match warpsync {
        Some(sync) => cps * sync,
        None => warprate.unwrap_or(1.0),
    };
    let warp_lfo_shape = match warpshape {
        None => 0u8,
        Some(serde_json::Value::Number(shape)) => {
            (shape.as_f64().unwrap_or(0.0).rem_euclid(5.0)) as u8
        }
        Some(serde_json::Value::String(shape)) => match shape.as_str() {
            "tri" | "triangle" => 0,
            "sine" => 1,
            "ramp" => 2,
            "saw" => 3,
            "square" => 4,
            other => {
                return Err(format!("unsupported wavetable warp LFO shape \"{other}\"").into());
            }
        },
        Some(_) => {
            return Err(VoiceError::InvalidControl(
                "warpshape must be a name or number".into(),
            ));
        }
    };
    // `warpmode` takes a name or an index; anything unrecognised is NONE,
    // matching `Warpmode[name.toUpperCase()] ?? Warpmode.NONE`.
    let warp_mode = match object.get("warpmode").filter(|value| !value.is_null()) {
        None => rustel_audio::WarpMode::None,
        Some(serde_json::Value::String(name)) => rustel_audio::WarpMode::from_name(name),
        Some(serde_json::Value::Number(index)) => {
            rustel_audio::WarpMode::from_index(index.as_f64().unwrap_or(0.0) as i32)
        }
        Some(_) => {
            return Err(VoiceError::InvalidControl(
                "warpmode must be a name or number".into(),
            ));
        }
    } as u8;
    Ok(Some(rustel_audio::WavetableControls {
        table: id,
        frame_len: 2048,
        voices: voices as f32,
        lfo_shape,
        phaserand,
        freqspread: checked_f32(
            optional_f64(object.get("detune"), "detune")?.unwrap_or(0.18),
            "detune",
        )?,
        panspread: checked_f32(
            optional_f64(object.get("spread"), "spread")?.unwrap_or(0.7),
            "spread",
        )?,
        position: checked_f32(optional_f64(object.get("wt"), "wt")?.unwrap_or(0.0), "wt")?,
        pos_env_amount: checked_f32(pos_env_amount, "wtenv")?,
        pos_attack: checked_f32(pos_attack, "wtattack")?,
        pos_decay: checked_f32(pos_decay, "wtdecay")?,
        pos_sustain: checked_f32(pos_sustain, "wtsustain")?,
        pos_release: checked_f32(pos_release, "wtrelease")?,
        lfo_depth: checked_f32(lfo_depth, "wtdepth")?,
        lfo_rate: checked_f32(lfo_rate, "wtrate")?,
        lfo_skew: checked_f32(wtskew.unwrap_or(0.5), "wtskew")?,
        warp: checked_f32(
            optional_f64(object.get("warp"), "warp")?.unwrap_or(0.0),
            "warp",
        )?,
        warp_mode,
        warp_env_amount: checked_f32(warp_env_amount, "warpenv")?,
        warp_attack: checked_f32(warp_attack, "warpattack")?,
        warp_decay: checked_f32(warp_decay, "warpdecay")?,
        warp_sustain: checked_f32(warp_sustain, "warpsustain")?,
        warp_release: checked_f32(warp_release, "warprelease")?,
        warp_lfo_depth: checked_f32(warp_lfo_depth, "warpdepth")?,
        warp_lfo_rate: checked_f32(warp_lfo_rate, "warprate")?,
        warp_lfo_skew: checked_f32(warpskew.unwrap_or(0.5), "warpskew")?,
        warp_lfo_dc: checked_f32(
            optional_f64(object.get("warpdc"), "warpdc")?.unwrap_or(0.0),
            "warpdc",
        )?,
        warp_lfo_shape,
        lfo_dc: checked_f32(
            optional_f64(object.get("wtdc"), "wtdc")?.unwrap_or(0.0),
            "wtdc",
        )?,
    }))
}

/// ADSR resolution with explicit defaults: all-absent returns the defaults;
/// otherwise sustain falls back to 1 when decay is absent and to the 0.001
/// floor when decay was given, floors included throughout.
fn adsr_values(values: [Option<f64>; 4], defaults: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    let [a, d, s, r] = values;
    if a.is_none() && d.is_none() && s.is_none() && r.is_none() {
        return defaults;
    }
    let sustain = match s {
        Some(s) => s,
        None if (a.is_some() && d.is_none()) || (a.is_none() && d.is_none()) => 1.0,
        None => 0.001,
    };
    (
        a.unwrap_or(0.0).max(0.001),
        d.unwrap_or(0.0).max(0.001),
        sustain.min(1.0),
        r.unwrap_or(0.0).max(0.01),
    )
}

/// Why a `begin`/`end` pair cannot be played, said in the score's terms.
///
/// The pair is a position in the sound: 0 at its start, 1 at its end. A
/// score rarely writes one directly. `slice(n, ...)` turns a slice number
/// into it, so the numbers that arrive here are often ones nobody typed:
/// `slice(8, "100")` asks for piece 100 of eight, which is 12.5 to 12.625
/// of the way through. A message that states only the failed inequality
/// does not explain the 100 or the 8 that the user wrote.
fn slice_refusal(begin: f64, end: f64) -> String {
    let tidy = |value: f64| {
        let text = format!("{value:.4}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    let ends_first = end <= begin;
    let (begin, end) = (tidy(begin), tidy(end));
    if ends_first {
        return format!("this plays the sound from {begin} to {end}, which ends before it starts");
    }
    format!(
        "this plays the sound from {begin} to {end}, and a position in a sound runs from 0 to 1 \
         - with slice(n, …) the pieces are numbered 0 to n-1"
    )
}

fn sample_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    samples: &dyn SampleLookup,
) -> Result<Option<rustel_audio::SampleControls>, VoiceError> {
    let Some(sound) = object.get("s") else {
        return Ok(None);
    };
    let name = sound
        .as_str()
        .ok_or_else(|| "s must be a scalar sound name".to_owned())?;
    // The entire bank rule: `s` becomes `{bank}_{s}`. Alias banks
    // (tr909 → RolandTR909) resolve inside the library lookup, after
    // registration lowercased the keys.
    let banked;
    let plain = name;
    let name = match object.get("bank") {
        Some(serde_json::Value::String(bank)) if !name.is_empty() => {
            banked = format!("{bank}_{name}");
            banked.as_str()
        }
        Some(serde_json::Value::Null) | None => name,
        Some(other) => match other.as_f64() {
            // `.bank('9000')` numerifies under miniAllStrings; the prefix
            // rule stringifies it right back.
            Some(bank) if !name.is_empty() => {
                banked = format!("{bank}_{name}");
                banked.as_str()
            }
            _ => {
                return Err(VoiceError::InvalidControl(
                    "bank must be a scalar bank name".into(),
                ));
            }
        },
    };
    // Built-in synth names are oscillator sources, never unbanked samples.
    // Decide that before parsing sample pitch: upstream's synth path treats a
    // falsy note (including the empty string produced by a fractional
    // transpose of a named note) as its default pitch. Parsing it here as a
    // sample note rejected the whole replacement before the oscillator path
    // could apply that fallback. An explicit bank still takes the sample
    // route, so `s("supersaw").bank("my_bank")` keeps its bank semantics.
    let banked_name = !std::ptr::eq(name, plain);
    if !banked_name && is_native_synth_sound(name) {
        return Ok(None);
    }
    // Sample pitch: freq wins, then note, then a default that depends on the
    // kind of sound. An ordinary sample bank falls back to midi 36; a gm_*
    // soundfont falls back to c3, which is midi 48 - the soundfont loader
    // carries its own default and it is an octave up from the sampler's.
    let default_midi = sample_default_midi(object, name);
    let midi = match object.get("freq") {
        Some(serde_json::Value::Number(freq)) => {
            let freq = freq
                .as_f64()
                .ok_or_else(|| "freq must be a finite number".to_owned())?;
            69.0 + 12.0 * (checked_frequency(freq)? / 440.0).log2()
        }
        _ => match object.get("note") {
            Some(serde_json::Value::String(note)) => rustel_core::util::note_to_midi(note, 3)
                .map_err(|error| explain_unplayable(note, error))?,
            Some(serde_json::Value::Number(note)) => note
                .as_f64()
                .filter(|note| note.is_finite())
                .ok_or_else(|| "note must be a finite MIDI number".to_owned())?,
            _ => default_midi,
        },
    };
    let n = optional_f64(object.get("n"), "n")?.unwrap_or(0.0);
    let (id, transpose, natural_duration_secs, loop_secs, envelope_peak, soundfont) =
        match samples.resolve(name, n, midi) {
            SampleResolution::Found {
                id,
                transpose,
                duration_secs,
                loop_secs,
                envelope_peak,
                soundfont,
            } => (
                id,
                transpose,
                duration_secs,
                loop_secs,
                envelope_peak,
                soundfont,
            ),
            SampleResolution::Loading => {
                // Never fatal: the runtime's per-voice skip-and-log drops
                // this one onset, like a late-fetch skip.
                return Err(VoiceError::SampleLoading(format!(
                    "sample \"{name}:{n}\" is still loading"
                )));
            }
            SampleResolution::Failed => {
                return Err(VoiceError::SampleFailed(format!(
                    "sample \"{name}:{n}\" failed to load"
                )));
            }
            // Not a sample: the oscillator path decides whether the name
            // means anything (and refuses loudly when it does not).
            SampleResolution::Unknown => return Ok(None),
        };
    // A soundfont zone plays at its note's pitch, from its start: `speed`,
    // `begin`/`end`, `unit`, `cut` and `nudge` do not reach it.
    let sampler_control = |key: &str| object.get(key).filter(|_| !soundfont);
    let speed = optional_f64(sampler_control("speed"), "speed")?.unwrap_or(1.0);
    // The reference returns before loading or inspecting any other sample
    // controls when speed is exactly zero. Preserve that per-hap no-op rather
    // than turning one silent event into an error for its entire scheduler
    // tick.
    if speed == 0.0 {
        return Ok(Some(rustel_audio::SampleControls {
            sample: id,
            playback_rate: 1.0,
            begin: 0.0,
            end: 1.0,
            hold: rustel_audio::SampleHold::Slice,
            muted: true,
            loop_secs: None,
            envelope_peak: 1.0,
            reversed: false,
            nudge_secs: 0.0,
            cut: None,
        }));
    }
    // A hap-level `transpose` control is not read: transpose derives from
    // note/midi, so the key is ignored here too.
    let begin = optional_f64(sampler_control("begin"), "begin")?.unwrap_or(0.0);
    let end = optional_f64(sampler_control("end"), "end")?.unwrap_or(1.0);
    if !(0.0..=1.0).contains(&begin) || !(0.0..=1.0).contains(&end) || end <= begin {
        return Err(slice_refusal(begin, end).into());
    }
    // `playbackRate = |speed| * 2^(transpose/12)`; the library already
    // derived `transpose` from this hap's midi and the bank's shape.
    // `unit: 'c'` scales the rate by the buffer duration; other units are
    // ignored.
    let mut rate = speed.abs() * 2.0f64.powf(transpose / 12.0);
    if matches!(sampler_control("unit"), Some(serde_json::Value::String(u)) if u == "c") {
        rate *= natural_duration_secs;
    }
    if !rate.is_finite() || rate <= 0.0 {
        return Err(format!("sample playback rate {rate} is not finite and positive").into());
    }
    let slice_duration = natural_duration_secs * (end - begin) / rate;
    if !slice_duration.is_finite() || slice_duration > 24.0 * 60.0 * 60.0 {
        return Err(
            format!("sample slice lasts {slice_duration} seconds; maximum is 86400").into(),
        );
    }
    let playback_rate = checked_f32(rate, "sample playback rate")?;
    if playback_rate == 0.0 {
        return Err(VoiceError::InvalidControl(
            "sample speed is too small for native f32 playback".into(),
        ));
    }
    Ok(Some(rustel_audio::SampleControls {
        sample: id,
        playback_rate,
        begin: checked_f32(begin, "begin")?,
        end: checked_f32(end, "end")?,
        // A looping source is gated by the HAP: the buffer source loops and
        // the gain envelope ends the note. A PRESENT `loop` control (even 0,
        // which does not loop) also switches the duration to the hap
        // (`clip == null && loop == null`).
        hold: if loop_secs.is_some()
            || object.get("loop").is_some_and(|value| !value.is_null())
            || object.get("clip").is_some_and(|value| !value.is_null())
            || object.get("release").is_some_and(|value| !value.is_null())
        {
            rustel_audio::SampleHold::Hap
        } else {
            rustel_audio::SampleHold::Slice
        },
        muted: false,
        loop_secs: match loop_secs {
            Some((start, end)) => Some((
                checked_f32(start, "soundfont loop start")?,
                checked_f32(end, "soundfont loop end")?,
            )),
            // `loop` truthy: loopStart/loopEnd = loopBegin/loopEnd × the
            // buffer's natural duration.
            None if optional_f64(object.get("loop"), "loop")?.is_some_and(|l| l != 0.0) => {
                let loop_begin = optional_f64(object.get("loopBegin"), "loopBegin")?.unwrap_or(0.0);
                let loop_end = optional_f64(object.get("loopEnd"), "loopEnd")?.unwrap_or(1.0);
                Some((
                    checked_f32(loop_begin * natural_duration_secs, "loopBegin")?,
                    checked_f32(loop_end * natural_duration_secs, "loopEnd")?,
                ))
            }
            None => None,
        },
        envelope_peak: checked_f32(envelope_peak, "soundfont envelope peak")?,
        reversed: speed < 0.0,
        cut: optional_f64(sampler_control("cut"), "cut")?
            .map(|cut| checked_f32(cut, "cut"))
            .transpose()?,
        // Negative nudge would need the source to start before the hap
        // frame; clamp to 0 (the envelope timing is unchanged either way).
        nudge_secs: checked_f32(
            optional_f64(sampler_control("nudge"), "nudge")?
                .unwrap_or(0.0)
                .max(0.0),
            "nudge",
        )?,
    }))
}

fn oscillator_waveform(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<rustel_audio::Waveform, VoiceError> {
    let Some(value) = object.get("s") else {
        return Ok(rustel_audio::Waveform::Triangle);
    };
    let name = value
        .as_str()
        .ok_or_else(|| "s must be a scalar oscillator name".to_owned())?
        .to_ascii_lowercase();
    match name.as_str() {
        "sine" | "sin" => Ok(rustel_audio::Waveform::Sine),
        "triangle" | "tri" => Ok(rustel_audio::Waveform::Triangle),
        "square" | "sqr" => Ok(rustel_audio::Waveform::Square),
        "sawtooth" | "saw" => Ok(rustel_audio::Waveform::Sawtooth),
        // 'user' without partials logs and falls back to triangle; with
        // partials the additive path overrides the waveform.
        "user" => Ok(rustel_audio::Waveform::Triangle),
        // Every source has been tried by now: a sample bank, a synth, an
        // oscillator. The message says that, not what the oscillator table
        // holds, and it names the sound that was looked for. Under `.bank`
        // that is `{bank}_{s}`, which shows the bank that has no such sound.
        _ => {
            let sound = value
                .as_str()
                .and_then(|plain| bank_prefixed(object, plain).ok().flatten())
                .unwrap_or(name);
            Err(VoiceError::UnknownSound(format!(
                "unknown sound {sound:?}: no sample bank or synth of that name is loaded"
            )))
        }
    }
}

fn synth_envelope(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<rustel_audio::Envelope, String> {
    let attack = optional_f64(object.get("attack"), "attack")?;
    let decay = optional_f64(object.get("decay"), "decay")?;
    let sustain = optional_f64(object.get("sustain"), "sustain")?;
    let release = optional_f64(object.get("release"), "release")?;
    let (attack, decay, sustain, release) =
        if attack.is_none() && decay.is_none() && sustain.is_none() && release.is_none() {
            let sustain = if object.get("s").and_then(serde_json::Value::as_str) == Some("bus") {
                1.0
            } else {
                0.6
            };
            (0.001, 0.05, sustain, 0.01)
        } else {
            let resolved_sustain = sustain.unwrap_or(
                if (attack.is_some() && decay.is_none()) || (attack.is_none() && decay.is_none()) {
                    1.0
                } else {
                    0.001
                },
            );
            (
                attack.unwrap_or(0.0).max(0.001),
                decay.unwrap_or(0.0).max(0.001),
                resolved_sustain.min(1.0),
                release.unwrap_or(0.0).max(0.01),
            )
        };
    Ok(rustel_audio::Envelope {
        attack_secs: checked_f32(attack, "attack")?,
        decay_secs: checked_f32(decay, "decay")?,
        sustain: checked_f32(sustain, "sustain")?,
        release_secs: checked_f32(release, "release")?,
    })
}

fn sample_envelope(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<rustel_audio::Envelope, String> {
    let attack = optional_f64(object.get("attack"), "attack")?;
    let decay = optional_f64(object.get("decay"), "decay")?;
    let sustain = optional_f64(object.get("sustain"), "sustain")?;
    let release = optional_f64(object.get("release"), "release")?;
    let (attack, decay, sustain, release) =
        if attack.is_none() && decay.is_none() && sustain.is_none() && release.is_none() {
            (0.001, 0.001, 1.0, 0.01)
        } else {
            let resolved_sustain = sustain.unwrap_or(
                if (attack.is_some() && decay.is_none()) || (attack.is_none() && decay.is_none()) {
                    1.0
                } else {
                    0.001
                },
            );
            (
                attack.unwrap_or(0.0).max(0.001),
                decay.unwrap_or(0.0).max(0.001),
                resolved_sustain.min(1.0),
                release.unwrap_or(0.0).max(0.01),
            )
        };
    Ok(rustel_audio::Envelope {
        attack_secs: checked_f32(attack, "attack")?,
        decay_secs: checked_f32(decay, "decay")?,
        sustain: checked_f32(sustain, "sustain")?,
        release_secs: checked_f32(release, "release")?,
    })
}

/// Per-orbit delay send: active
/// iff `delay > 0 && delaytime > 0 && delayfeedback > 0`, with delaytime
/// defaulting to `delaysync / cps` (delaysync 3/16, feedback 0.5).
fn delay_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    cps: f64,
) -> Result<Option<rustel_audio::DelayControls>, String> {
    // The send is gated on `delay > 0`, which is simply false for a
    // non-numeric value (bare `.delay()` nests the previous value as an
    // object) - the voice plays with no send rather than being refused.
    let wet = match object.get("delay") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Number(n)) => match n.as_f64() {
            Some(wet) if wet.is_finite() => wet,
            _ => return Err("delay must be a finite number".to_owned()),
        },
        Some(serde_json::Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                return Ok(None);
            }
        }
        Some(_) => return Ok(None),
    };
    let feedback = optional_f64(object.get("delayfeedback"), "delayfeedback")?.unwrap_or(0.5);
    let sync = optional_f64(object.get("delaysync"), "delaysync")?.unwrap_or(3.0 / 16.0);
    let time = match optional_f64(object.get("delaytime"), "delaytime")? {
        Some(time) => time,
        None => {
            if !(cps.is_finite() && cps > 0.0) {
                return Err(format!("delaysync needs a positive finite cps, got {cps}"));
            }
            sync / cps
        }
    };
    if !(wet > 0.0 && time > 0.0 && feedback > 0.0) {
        return Ok(None);
    }
    Ok(Some(rustel_audio::DelayControls {
        wet: checked_f32(wet, "delay")?,
        time_secs: checked_f32(time.min(1.0), "delaytime")?,
        feedback: checked_f32(feedback.clamp(0.0, 0.98), "delayfeedback")?,
    }))
}

/// Reverb controls: `room` enables the effect; roomsize, roomfade, roomlp and
/// roomdim default to 2, 0.1, 15000 and 1000. `ir` selects a sample-backed
/// impulse response, with optional `irspeed` and `irbegin` controls.
fn reverb_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    samples: &dyn SampleLookup,
) -> Result<Option<rustel_audio::ReverbControls>, String> {
    let Some(room) = optional_f64(object.get("room"), "room")? else {
        return Ok(None);
    };
    if room <= 0.0 {
        return Ok(None);
    }
    // `ir` names a sound whose buffer becomes the convolver's impulse
    // response. A name still loading or unknown falls back to the generated
    // IR with a log - this path never blocks a voice on a fetch.
    let ir = match object.get("ir").and_then(serde_json::Value::as_str) {
        None => None,
        Some(name) => {
            let index = optional_f64(object.get("i"), "i")?.unwrap_or(1.0);
            match samples.resolve(name, index, 36.0) {
                SampleResolution::Found { id, .. } => Some(rustel_audio::reverb::IrParams {
                    sample: id,
                    speed: checked_f32(
                        optional_f64(object.get("irspeed"), "irspeed")?.unwrap_or(1.0),
                        "irspeed",
                    )?,
                    begin: checked_f32(
                        optional_f64(object.get("irbegin"), "irbegin")?.unwrap_or(0.0),
                        "irbegin",
                    )?,
                }),
                other => {
                    let (state, reason) = match other {
                        SampleResolution::Loading => ("loading", "is still loading"),
                        SampleResolution::Failed => ("failed", "failed to load"),
                        SampleResolution::Unknown => ("unknown", "is not a known sound"),
                        SampleResolution::Found { .. } => unreachable!(),
                    };
                    report_notice(
                        format!("impulse response '{name}' {reason}; the generated reverb plays"),
                        serde_json::json!({ "ir_fallback": { "name": name, "state": state } }),
                    );
                    None
                }
            }
        }
    };
    let size = optional_f64(object.get("roomsize"), "roomsize")?.unwrap_or(2.0);
    if !(0.0..=f64::from(rustel_audio::reverb::MAX_REVERB_SECONDS)).contains(&size) {
        return Err(format!(
            "roomsize {size} is outside the native reverb's 0..{} second ceiling",
            rustel_audio::reverb::MAX_REVERB_SECONDS
        ));
    }
    Ok(Some(rustel_audio::ReverbControls {
        ir,
        wet: checked_f32(room, "room")?,
        size_secs: checked_f32(size, "roomsize")?,
        fade_secs: checked_f32(
            optional_f64(object.get("roomfade"), "roomfade")?.unwrap_or(0.1),
            "roomfade",
        )?,
        lp_start_hz: checked_f32(
            optional_f64(object.get("roomlp"), "roomlp")?.unwrap_or(15_000.0),
            "roomlp",
        )?,
        lp_end_hz: checked_f32(
            optional_f64(object.get("roomdim"), "roomdim")?.unwrap_or(1_000.0),
            "roomdim",
        )?,
    }))
}

fn duck_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::DuckControls>, String> {
    let Some(target) = object.get("duckorbit").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    // JS numeric coercion for one list slot: numbers pass through, numeric
    // strings parse (JS object keys are strings), booleans coerce.
    fn coerce(value: &serde_json::Value) -> Option<f64> {
        match value {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
            serde_json::Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    // `[x].flat()` - every duck field may be a scalar or the mini `:` list;
    // index i falls back to index 0.
    fn list(value: Option<&serde_json::Value>) -> Vec<Option<f64>> {
        match value {
            None | Some(serde_json::Value::Null) => vec![None],
            Some(serde_json::Value::Array(items)) => items.iter().map(coerce).collect(),
            Some(other) => vec![coerce(other)],
        }
    }
    let orbits = list(Some(target));
    let onsets = list(object.get("duckonset"));
    let attacks = list(object.get("duckattack"));
    let depths = list(object.get("duckdepth"));
    let at = |values: &[Option<f64>], idx: usize, fallback: f64| -> f64 {
        values
            .get(idx)
            .copied()
            .flatten()
            .or_else(|| values.first().copied().flatten())
            .unwrap_or(fallback)
    };
    let mut targets = [None; rustel_audio::MAX_DUCK_TARGETS];
    let mut filled = 0usize;
    for (idx, orbit) in orbits.iter().enumerate() {
        // A target that is not a whole orbit number in 0..16 is reported
        // and skipped. The voice always plays: a refusal here would silence
        // a live line over `.duck(0.1)`.
        let valid = orbit.filter(|o| (0.0..16.0).contains(o) && o.fract() == 0.0);
        let Some(orbit_value) = valid else {
            let shown = orbit
                .map(|o| o.to_string())
                .unwrap_or_else(|| "non-numeric".to_owned());
            let message = format!("duck target orbit {shown} does not exist");
            report_notice(
                message.clone(),
                serde_json::json!({ "duck_skipped": { "message": message } }),
            );
            continue;
        };
        if filled >= targets.len() {
            let message = "more duck targets than the native capacity";
            report_notice(
                message,
                serde_json::json!({ "duck_skipped": { "message": message } }),
            );
            break;
        }
        targets[filled] = Some(rustel_audio::DuckTarget {
            orbit: orbit_value as u8,
            onset_secs: checked_f32(at(&onsets, idx, 0.0), "duckonset")?,
            attack_secs: checked_f32(at(&attacks, idx, 0.1), "duckattack")?,
            depth: checked_f32(at(&depths, idx, 1.0), "duckdepth")?,
        });
        filled += 1;
    }
    if filled == 0 {
        return Ok(None);
    }
    Ok(Some(rustel_audio::DuckControls { targets }))
}

/// Which param of a filter's own LFO a control names. `depth` is reachable
/// under two names (`lpdepth` and `lpdepthfrequency`). They differ only in
/// how the base is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterLfoParam {
    Depth,
    Dc,
    Skew,
}

/// Split `lpdepth`, `hpskew`, … into the filter and the param. Returns `None`
/// for anything else, INCLUDING `lprate`/`lpsync`, which are not modulatable
/// at all (see `resolve_target_in`).
fn filter_lfo_control(control: &str) -> Option<(rustel_audio::FilterLfoKind, FilterLfoParam)> {
    let kind = match control.get(..2)? {
        "lp" => rustel_audio::FilterLfoKind::Lowpass,
        "hp" => rustel_audio::FilterLfoKind::Highpass,
        "bp" => rustel_audio::FilterLfoKind::Bandpass,
        _ => return None,
    };
    let param = match &control[2..] {
        "depth" | "depthfrequency" => FilterLfoParam::Depth,
        "dc" => FilterLfoParam::Dc,
        "skew" => FilterLfoParam::Skew,
        _ => return None,
    };
    Some((kind, param))
}

/// Resolve the names and numeric ids shared by every native LFO.
///
/// Keeping this conversion beside the voice graph prevents one LFO family
/// from accepting a documented name while another silently falls back to its
/// default shape.
fn lfo_shape_index(value: Option<&serde_json::Value>, control: &str) -> Result<Option<u8>, String> {
    match value.filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(serde_json::Value::Number(shape)) => shape
            .as_f64()
            .filter(|shape| shape.is_finite())
            .map(|shape| (shape as i64).rem_euclid(5) as u8)
            .map(Some)
            .ok_or_else(|| format!("{control} must be a finite number or an LFO shape name")),
        Some(serde_json::Value::String(shape)) => match shape.as_str() {
            "tri" | "triangle" => Ok(Some(0)),
            "sine" => Ok(Some(1)),
            "ramp" => Ok(Some(2)),
            "saw" => Ok(Some(3)),
            "square" => Ok(Some(4)),
            other => Err(format!("unsupported {control} LFO shape \"{other}\"")),
        },
        Some(_) => Err(format!(
            "{control} must be a finite number or an LFO shape name"
        )),
    }
}

/// Outcome of reading a modulator control that might name ANOTHER modulator.
enum ModulatorTarget {
    /// Not `lfo_…`/`env_…` at all - an ordinary control.
    NotAModulator,
    Resolved(rustel_audio::ModTarget, f64),
    /// It named a modulator, but there is none by that id - logged, and the
    /// modulator is dropped.
    Missing,
}

/// `control: "lfo_cut"` with `subControl: "rate"` aims at the `lfo()` the
/// pattern named `cut`. With no subControl the bare `lfo` entry applies.
/// That entry is the rate, so the rate is the default here too.
///
/// The param base is the target modulator's current value for that param.
fn resolve_modulator_target(
    from_kind: &str,
    control: &str,
    sub_control: Option<&str>,
    lfo_ids: &[String],
    env_ids: &[String],
    lfos: &[Option<rustel_audio::LfoMod>],
    envs: &[Option<rustel_audio::EnvMod>],
) -> ModulatorTarget {
    let (kind, name) = match control.split_once('_') {
        Some(("lfo", name)) => ("lfo", name),
        Some(("env", name)) => ("env", name),
        _ => return ModulatorTarget::NotAModulator,
    };
    // Every LFO is wired before a single envelope, so an lfo aiming at
    // `env_…` finds no node and is dropped. An env aiming at an lfo works
    // because its target has already been registered.
    if from_kind == "lfo" && kind == "env" {
        return ModulatorTarget::Missing;
    }
    let ids = if kind == "lfo" { lfo_ids } else { env_ids };
    let Some(id) = ids
        .iter()
        .position(|candidate| candidate == name)
        .filter(|index| *index < rustel_audio::MAX_VOICE_MODS)
    else {
        return ModulatorTarget::Missing;
    };
    let id = id as u8;

    if kind == "lfo" {
        use rustel_audio::ModulatorParam;
        let param = match sub_control {
            None | Some("rate") | Some("sync") => ModulatorParam::Rate,
            Some("depth" | "depthabs") => ModulatorParam::Depth,
            Some("skew") => ModulatorParam::Skew,
            Some("curve") => ModulatorParam::Curve,
            Some("dcoffset") => ModulatorParam::Dcoffset,
            Some(_) => return ModulatorTarget::Missing,
        };
        // The target has to exist for its params to have values at all.
        let Some(target) = lfos.iter().flatten().find(|lfo| lfo.id == Some(id)) else {
            return ModulatorTarget::Missing;
        };
        let base = match param {
            ModulatorParam::Rate => target.frequency_hz,
            ModulatorParam::Depth => target.depth,
            ModulatorParam::Skew => target.skew,
            ModulatorParam::Curve => target.curve,
            ModulatorParam::Dcoffset => target.dcoffset,
        };
        ModulatorTarget::Resolved(
            rustel_audio::ModTarget::LfoParam(id, param),
            f64::from(base),
        )
    } else {
        use rustel_audio::EnvelopeParam;
        let param = match sub_control {
            None | Some("depth" | "depthabs") => EnvelopeParam::Depth,
            Some("attack") => EnvelopeParam::Attack,
            Some("decay") => EnvelopeParam::Decay,
            Some("sustain") => EnvelopeParam::Sustain,
            Some("release") => EnvelopeParam::Release,
            Some(_) => return ModulatorTarget::Missing,
        };
        let Some(target) = envs.iter().flatten().find(|env| env.id == Some(id)) else {
            return ModulatorTarget::Missing;
        };
        let base = match param {
            EnvelopeParam::Depth => target.depth,
            EnvelopeParam::Attack => target.attack_secs,
            EnvelopeParam::Decay => target.decay_secs,
            EnvelopeParam::Sustain => target.sustain,
            EnvelopeParam::Release => target.release_secs,
        };
        ModulatorTarget::Resolved(
            rustel_audio::ModTarget::EnvParam(id, param),
            f64::from(base),
        )
    }
}

type VoiceModulators = (
    [Option<rustel_audio::LfoMod>; rustel_audio::MAX_VOICE_MODS],
    [Option<rustel_audio::EnvMod>; rustel_audio::MAX_VOICE_MODS],
    [Option<rustel_audio::BusMod>; rustel_audio::MAX_VOICE_MODS],
);

/// Resolve `value.lfo` and `value.env`, the modulator maps that `modulate()`
/// attaches, and the `bmod` bus modulators. Each entry adds to one target
/// param. A target that the native graph cannot reach is skipped with a
/// notice; the voice still plays.
fn modulator_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    duration_secs: f64,
    release_secs: f32,
    // The musical time of the onset, in seconds. An LFO built here with no
    // `retrig` starts at the phase of this time.
    musical_time: f64,
    cps: f64,
    // The carrier's frequency - what an FM operator's own frequency is a
    // multiple of, and so the base an `fmh` modulator rides.
    carrier_hz: f64,
) -> Result<VoiceModulators, String> {
    let mut lfos = [None; rustel_audio::MAX_VOICE_MODS];
    let mut envs = [None; rustel_audio::MAX_VOICE_MODS];
    let mut bus_mods = [None; rustel_audio::MAX_VOICE_MODS];
    let skip = |kind: &str, reason: &str| {
        report_notice(
            format!("{kind} modulator skipped: {reason}"),
            serde_json::json!({ "modulator_skipped": { "kind": kind, "message": reason } }),
        );
    };
    let entry_f64 = |entry: &serde_json::Map<String, serde_json::Value>, key: &str| {
        entry.get(key).and_then(serde_json::Value::as_f64)
    };
    /// `phaserdepth`, defaulting when absent, as in the reference.
    fn phaser_depth(object: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
        Some(
            object
                .get("phaserdepth")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.75),
        )
    }

    /// The tremolo's LFO frequency, however the pattern spelled it, and `None`
    /// when there is no tremolo in the chain to modulate.
    fn tremolo_rate(object: &serde_json::Map<String, serde_json::Value>, cps: f64) -> Option<f64> {
        if let Some(sync) = object
            .get("tremolosync")
            .and_then(serde_json::Value::as_f64)
        {
            return Some(sync * cps);
        }
        object
            .get("tremolo")
            .or_else(|| object.get("tremolorate"))
            .and_then(serde_json::Value::as_f64)
    }

    // (target, param_base, is_frequency_param) for a modulator control name.
    // Resolved against `object` for the main chain, or against one `.FX()`
    // stage's own map when a modulator names an `fxi`: a target's base value
    // is the parameter it will ADD to, and a stage's filter is not the main
    // chain's filter of the same name.
    let resolve_target_in = |object: &serde_json::Map<String, serde_json::Value>,
                             control: &str|
     -> Option<(rustel_audio::ModTarget, f64, bool)> {
        match control {
            "cutoff" | "lpf" => Some((
                rustel_audio::ModTarget::LowpassFreq,
                object.get("cutoff").and_then(serde_json::Value::as_f64)?,
                true,
            )),
            "hcutoff" | "hpf" => Some((
                rustel_audio::ModTarget::HighpassFreq,
                object.get("hcutoff").and_then(serde_json::Value::as_f64)?,
                true,
            )),
            "bandf" | "bpf" => Some((
                rustel_audio::ModTarget::BandFreq,
                object.get("bandf").and_then(serde_json::Value::as_f64)?,
                true,
            )),
            // The vowel node exposes an ARRAY of five filter frequency
            // params; one depth/range derives from the first, and the same
            // signal drives all five.
            "vowel" => {
                let vowel = vowel_controls(object).ok().flatten()?;
                Some((
                    rustel_audio::ModTarget::VowelFreq,
                    f64::from(vowel.freqs[0]),
                    true,
                ))
            }
            // The gain node of each chain holds `gain × velocity`.
            "gain" => {
                let read = |key, default| {
                    optional_f64(object.get(key), key)
                        .ok()
                        .flatten()
                        .unwrap_or(default)
                };
                Some((
                    rustel_audio::ModTarget::Gain,
                    read("gain", 0.8) * read("velocity", 1.0),
                    false,
                ))
            }
            // The orbit's DJ filter, which exists only once a `djf` trigger
            // has made it - `orbitBus.getDjf` is create-on-first-use.
            "djf" => Some((
                rustel_audio::ModTarget::Djf,
                object.get("djf").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            // Both spellings reach the shared orbit DelayNode's `delayTime`.
            // The node exists only when this trigger built its delay send;
            // its param already holds the resolved seconds value, including
            // the `delaysync / cps` conversion and one-second clamp.
            "delaytime" | "delaysync" => {
                let delay = delay_controls(object, cps).ok().flatten()?;
                Some((
                    rustel_audio::ModTarget::DelayTime,
                    f64::from(delay.time_secs),
                    false,
                ))
            }
            // The feedback GainNode belongs to the same conditional orbit
            // graph. Its base is the value actually assigned to `gain`, after
            // The reference's 0..0.98 clamp.
            "delayfeedback" => {
                let delay = delay_controls(object, cps).ok().flatten()?;
                Some((
                    rustel_audio::ModTarget::DelayFeedback,
                    f64::from(delay.feedback),
                    false,
                ))
            }
            // `note` and `s` both name the source node's frequency, so
            // either sweeps the pitch. The base is the frequency the note
            // resolved to. A source with no frequency param is different:
            // the modulator rides `detune` from a base of 0. See
            // `source_pitch_rides_detune`.
            "note" | "s" | "frequency" | "freq" => Some((
                rustel_audio::ModTarget::Frequency,
                if source_pitch_rides_detune(object) {
                    0.0
                } else {
                    carrier_hz
                },
                true,
            )),
            // Only the wavetable and supersaw oscillators have `detune` and
            // `spread` params. No other source has such a param to ride.
            "detune" | "spread" => {
                let source = object.get("s").and_then(serde_json::Value::as_str)?;
                if !source.starts_with("wt_") && source != "supersaw" {
                    return None;
                }
                if control == "detune" {
                    Some((
                        rustel_audio::ModTarget::SourceFreqspread,
                        object
                            .get("detune")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.18),
                        false,
                    ))
                } else {
                    Some((
                        rustel_audio::ModTarget::SourcePanspread,
                        object
                            .get("spread")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(if source == "supersaw" { 0.6 } else { 0.7 }),
                        false,
                    ))
                }
            }
            // The wavetable's warp amount and its warp LFO, resolved on the
            // same rules as the position pair below - `warpdc` is absent for
            // the same reason `wtdc` is.
            "warp" | "warprate" | "warpsync" | "warpdepth" | "warpskew" => {
                let source = object.get("s").and_then(serde_json::Value::as_str)?;
                if !source.starts_with("wt_") {
                    return None;
                }
                let field = |name: &str| object.get(name).and_then(serde_json::Value::as_f64);
                if control == "warp" {
                    return Some((
                        rustel_audio::ModTarget::WavetableWarp,
                        field("warp").unwrap_or(0.0),
                        false,
                    ));
                }
                let has_lfo_input = ["warprate", "warpsync", "warpshape", "warpskew"]
                    .iter()
                    .any(|name| object.get(*name).is_some_and(|value| !value.is_null()));
                let depth = match field("warpdepth") {
                    Some(depth) => depth,
                    None if has_lfo_input => 0.5,
                    None => return None,
                };
                if depth == 0.0 {
                    return None;
                }
                match control {
                    "warpdepth" => Some((rustel_audio::ModTarget::WarpLfoDepth, depth, false)),
                    "warpskew" => Some((
                        rustel_audio::ModTarget::WarpLfoSkew,
                        field("warpskew").unwrap_or(0.5),
                        false,
                    )),
                    _ => Some((
                        rustel_audio::ModTarget::WarpLfoRate,
                        match field("warpsync") {
                            Some(sync) => sync * cps,
                            None => field("warprate").unwrap_or(1.0),
                        },
                        true,
                    )),
                }
            }
            // The wavetable's table position, and its own position LFO.
            // `wtdc` is missing on purpose: the reference cannot modulate it
            // (a `dc`/`dcoffset` param-name mismatch throws), so neither can
            // a score here.
            "wt" | "wtrate" | "wtsync" | "wtdepth" | "wtskew" => {
                let source = object.get("s").and_then(serde_json::Value::as_str)?;
                if !source.starts_with("wt_") {
                    return None;
                }
                let field = |name: &str| object.get(name).and_then(serde_json::Value::as_f64);
                if control == "wt" {
                    return Some((
                        rustel_audio::ModTarget::WavetablePosition,
                        field("wt").unwrap_or(0.0),
                        false,
                    ));
                }
                // The position LFO is built only when a depth survives:
                // an explicit `wtdepth`, or 0.5 when any other LFO input is
                // present. Without it there is no node to modulate.
                let has_lfo_input = ["wtrate", "wtsync", "wtshape", "wtskew"]
                    .iter()
                    .any(|name| object.get(*name).is_some_and(|value| !value.is_null()));
                let depth = match field("wtdepth") {
                    Some(depth) => depth,
                    None if has_lfo_input => 0.5,
                    None => return None,
                };
                if depth == 0.0 {
                    return None;
                }
                match control {
                    "wtdepth" => Some((rustel_audio::ModTarget::WtLfoDepth, depth, false)),
                    "wtskew" => Some((
                        rustel_audio::ModTarget::WtLfoSkew,
                        field("wtskew").unwrap_or(0.5),
                        false,
                    )),
                    // wtrate and wtsync both name the LFO's frequency.
                    _ => Some((
                        rustel_audio::ModTarget::WtLfoRate,
                        match field("wtsync") {
                            Some(sync) => sync * cps,
                            None => field("wtrate").unwrap_or(1.0),
                        },
                        true,
                    )),
                }
            }
            // A filter's own LFO (`lpdepth`, `lpdepthfrequency`, `lpdc`,
            // `lpskew` and their hp/bp forms). The LFO exists only where the
            // filter built one, which needs both a cutoff and at least one
            // LFO control.
            //
            // `lprate`, `lpsync` and `lpshape` are not here. The reference
            // throws when a modulator aims at them and loses the modulator
            // (verified against the browser). The first two name params its
            // LFO worklet never declares; `lpshape` indexes an array with a
            // fractional value. There is no working behaviour to match, and
            // reproducing the crash would break the never-die rule.
            name if filter_lfo_control(name).is_some() => {
                let (kind, param) = filter_lfo_control(name)?;
                let prefix = kind.prefix();
                let cutoff_key = match kind {
                    rustel_audio::FilterLfoKind::Lowpass => "cutoff",
                    rustel_audio::FilterLfoKind::Highpass => "hcutoff",
                    rustel_audio::FilterLfoKind::Bandpass => "bandf",
                };
                let frequency = object.get(cutoff_key).and_then(serde_json::Value::as_f64)?;
                let field = |suffix: &str| {
                    object
                        .get(&format!("{prefix}{suffix}"))
                        .and_then(serde_json::Value::as_f64)
                };
                // The filter builds its LFO only when one of these is set.
                if ["rate", "sync", "depth", "depthfrequency", "shape", "skew"]
                    .iter()
                    .all(|suffix| field(suffix).is_none())
                {
                    return None;
                }
                let base = match param {
                    FilterLfoParam::Depth => {
                        field("depthfrequency").unwrap_or(field("depth").unwrap_or(1.0) * frequency)
                    }
                    FilterLfoParam::Dc => field("dc").unwrap_or(-0.5),
                    // The skew default comes from the LFO itself, not the
                    // filter - the filter passes `skew` straight through.
                    FilterLfoParam::Skew => field("skew").unwrap_or(0.5),
                };
                let target = match param {
                    FilterLfoParam::Depth => rustel_audio::ModTarget::FilterLfoDepth(kind),
                    FilterLfoParam::Dc => rustel_audio::ModTarget::FilterLfoDc(kind),
                    FilterLfoParam::Skew => rustel_audio::ModTarget::FilterLfoSkew(kind),
                };
                Some((target, base, false))
            }
            // `fmi{k}` rides the gain carrying operator k's modulation index
            // (`fm_{k}_gain`); `fmh{k}` rides the operator oscillator's
            // frequency (`fm_{k}`), whose value is carrier × harmonicity.
            //
            // Both exist only where the operator does. An operator is built
            // only when the pattern names its index. A modulator on an
            // operator that was not built is reported and skipped.
            name if name.starts_with("fmi") || name.starts_with("fmh") => {
                let index_key = |slot: u8| {
                    if slot == 0 {
                        "fmi".to_string()
                    } else {
                        format!("fmi{}", slot + 1)
                    }
                };
                let is_index = name.starts_with("fmi");
                let slot = rustel_audio::fm_operator_slot(&name[3..])?;
                // The operator has to exist to be modulated at all.
                let index = object
                    .get(&index_key(slot))
                    .and_then(serde_json::Value::as_f64)?;
                if is_index {
                    Some((rustel_audio::ModTarget::FmIndex(slot), index, false))
                } else {
                    let harmonicity_key = if slot == 0 {
                        "fmh".to_string()
                    } else {
                        format!("fmh{}", slot + 1)
                    };
                    let harmonicity = object
                        .get(&harmonicity_key)
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0);
                    Some((
                        rustel_audio::ModTarget::FmFreq(slot),
                        carrier_hz * harmonicity,
                        true,
                    ))
                }
            }
            // Q targets exist only when their filter node exists; the base
            // is the node's Q param, default 1.
            "resonance" | "lpq" => {
                object.get("cutoff").and_then(serde_json::Value::as_f64)?;
                Some((
                    rustel_audio::ModTarget::LowpassQ,
                    object
                        .get("resonance")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    false,
                ))
            }
            "hresonance" | "hpq" => {
                object.get("hcutoff").and_then(serde_json::Value::as_f64)?;
                Some((
                    rustel_audio::ModTarget::HighpassQ,
                    object
                        .get("hresonance")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    false,
                ))
            }
            "bandq" | "bpq" => {
                object.get("bandf").and_then(serde_json::Value::as_f64)?;
                Some((
                    rustel_audio::ModTarget::BandQ,
                    object
                        .get("bandq")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    false,
                ))
            }
            // The pan node exists only when the pan control is present; its
            // param base is the StereoPanner value 2·pan − 1.
            "pan" => {
                let pan = object.get("pan").and_then(serde_json::Value::as_f64)?;
                Some((rustel_audio::ModTarget::Pan, 2.0 * pan - 1.0, false))
            }
            // The coarse/crush/shape worklets, like the filters above, are
            // only in the chain when their own control is set, so the
            // modulator resolves only then and adds to that control's value.
            "coarse" => Some((
                rustel_audio::ModTarget::Coarse,
                object.get("coarse").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            "crush" => Some((
                rustel_audio::ModTarget::Crush,
                object.get("crush").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            "shape" => Some((
                rustel_audio::ModTarget::Shape,
                object.get("shape").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            // shapevol is the same node's postgain param, so it needs `shape`
            // present too; its base is the shapevol default of 1.
            "shapevol" => {
                object.get("shape").and_then(serde_json::Value::as_f64)?;
                Some((
                    rustel_audio::ModTarget::ShapeVol,
                    object
                        .get("shapevol")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    false,
                ))
            }
            // The distortion worklet, like the others, is only in the chain
            // when `distort` is set. Its param is the raw amount; the worklet
            // takes expm1 of it per sample. `diode` names the same worklet on
            // the diode algorithm, so it modulates the same way; a modulator
            // on a voice that set neither is skipped, exactly as `distort`'s is.
            "distort" | "distorttype" => Some((
                rustel_audio::ModTarget::Distort,
                object.get("distort").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            "diode" => {
                let amount = match optional_value(object.get("diode")).map(diode_controls) {
                    Some(Ok(controls)) => f64::from(controls.amount),
                    _ => return None,
                };
                Some((rustel_audio::ModTarget::Distort, amount, false))
            }
            "distortvol" => {
                object.get("distort").and_then(serde_json::Value::as_f64)?;
                Some((
                    rustel_audio::ModTarget::DistortVol,
                    object
                        .get("distortvol")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1.0),
                    false,
                ))
            }
            // Send gains into the orbit's shared delay and reverb. The buses
            // belong to the orbit; these gains belong to the voice, so a
            // modulator moves this voice's contribution alone.
            "delay" => Some((
                rustel_audio::ModTarget::DelaySend,
                object.get("delay").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            "room" => Some((
                rustel_audio::ModTarget::RoomSend,
                object.get("room").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            // The phaser exists when `phaserrate` is set and `phaserdepth`
            // is above zero. `phaser` is an alias for
            // phaserrate, so `.phaser(2)` is what puts it in the chain.
            "phaserrate" | "phaser" => {
                let rate = object
                    .get("phaserrate")
                    .and_then(serde_json::Value::as_f64)?;
                phaser_depth(object).filter(|depth| *depth > 0.0)?;
                Some((rustel_audio::ModTarget::PhaserRate, rate, true))
            }
            "phasersweep" => {
                object
                    .get("phaserrate")
                    .and_then(serde_json::Value::as_f64)?;
                phaser_depth(object).filter(|depth| *depth > 0.0)?;
                Some((
                    rustel_audio::ModTarget::PhaserSweep,
                    // The param is the LFO's depth - sweep*2; a relative
                    // depth is a fraction of that.
                    2.0 * object
                        .get("phasersweep")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(2000.0),
                    false,
                ))
            }
            "phasercenter" => {
                object
                    .get("phaserrate")
                    .and_then(serde_json::Value::as_f64)?;
                phaser_depth(object).filter(|depth| *depth > 0.0)?;
                Some((
                    rustel_audio::ModTarget::PhaserCenter,
                    // The param this adds to is the notch's own frequency,
                    // which `getPhaser` sets to the centre plus a 282 Hz
                    // offset. A relative depth and the frequency range use
                    // that sum as the base, not the centre.
                    object
                        .get("phasercenter")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(1000.0)
                        + 282.0,
                    true,
                ))
            }
            "phaserdepth" => {
                object
                    .get("phaserrate")
                    .and_then(serde_json::Value::as_f64)?;
                let depth = phaser_depth(object).filter(|depth| *depth > 0.0)?;
                // The param is the notch's Q, which the depth control sets;
                // a relative depth is a fraction of THAT, not of the depth.
                let q = 2.0 - (depth * 2.0).clamp(0.0, 1.9);
                Some((rustel_audio::ModTarget::PhaserDepth, q, false))
            }
            // The compressor's five params, present once `compressor` has
            // put the node in the chain. Each base is the setting
            // `compressor_controls` puts on that node, defaults and range
            // clamps included.
            "compressor" => Some((
                rustel_audio::ModTarget::CompressorThreshold,
                f64::from(compressor_controls(object).ok().flatten()?.threshold_db),
                false,
            )),
            "compressorRatio" | "compressorratio" => Some((
                rustel_audio::ModTarget::CompressorRatio,
                f64::from(compressor_controls(object).ok().flatten()?.ratio),
                false,
            )),
            "compressorKnee" | "compressorknee" => Some((
                rustel_audio::ModTarget::CompressorKnee,
                f64::from(compressor_controls(object).ok().flatten()?.knee_db),
                false,
            )),
            "compressorAttack" | "compressorattack" => Some((
                rustel_audio::ModTarget::CompressorAttack,
                f64::from(compressor_controls(object).ok().flatten()?.attack_secs),
                false,
            )),
            "compressorRelease" | "compressorrelease" => Some((
                rustel_audio::ModTarget::CompressorRelease,
                f64::from(compressor_controls(object).ok().flatten()?.release_secs),
                false,
            )),
            // Tremolo. `tremolosync` is an alias for the rate that counts
            // cycles instead of seconds; both name the worklet's frequency.
            "tremolo" | "tremolosync" | "tremolorate" => {
                let base = tremolo_rate(object, cps)?;
                Some((rustel_audio::ModTarget::TremoloRate, base, true))
            }
            // Depth, skew and shape name params on the tremolo's nodes, so
            // each base is the value `tremolo_controls` puts there, defaults
            // included; `tremolodepth` names the carrier gain's floor.
            "tremolodepth" => {
                let tremolo = tremolo_controls(object, musical_time, cps).ok().flatten()?;
                Some((
                    rustel_audio::ModTarget::TremoloDepth,
                    f64::from(tremolo.gain_floor()),
                    false,
                ))
            }
            "tremoloskew" => {
                let tremolo = tremolo_controls(object, musical_time, cps).ok().flatten()?;
                Some((
                    rustel_audio::ModTarget::TremoloSkew,
                    f64::from(tremolo.skew),
                    false,
                ))
            }
            "tremoloshape" => {
                let tremolo = tremolo_controls(object, musical_time, cps).ok().flatten()?;
                Some((
                    rustel_audio::ModTarget::TremoloShape,
                    f64::from(tremolo.shape),
                    false,
                ))
            }
            // Vibrato: `vib` is the oscillator rate, `vibmod` the gain that
            // turns it into cents.
            "vib" | "vibrato" | "v" => Some((
                rustel_audio::ModTarget::VibratoRate,
                object.get("vib").and_then(serde_json::Value::as_f64)?,
                true,
            )),
            "vibmod" => {
                object.get("vib").and_then(serde_json::Value::as_f64)?;
                Some((
                    rustel_audio::ModTarget::VibratoDepth,
                    // The param is a gain in cents; vibmod is in semitones.
                    100.0
                        * object
                            .get("vibmod")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.5),
                    false,
                ))
            }
            // `pw` is a param of the pulse source, so it exists only when
            // that source does.
            "pw" => Some((
                rustel_audio::ModTarget::PulseWidth,
                object.get("pw").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            // `pwrate` and `pwsweep` name params on the pulse source's own
            // LFO worklet. That node exists only for a pulse whose resolved
            // sweep is nonzero; its paired defaults are applied before the
            // param base is read.
            "pwrate" | "pwsweep" => {
                if object.get("s").and_then(serde_json::Value::as_str) != Some("pulse") {
                    return None;
                }
                // This arm reads only the rate and the depth. The time has no effect.
                let lfo = pulse_width_lfo_controls(object, musical_time)
                    .ok()
                    .flatten()?;
                if control == "pwrate" {
                    Some((
                        rustel_audio::ModTarget::PulseWidthLfoRate,
                        f64::from(lfo.frequency_hz),
                        true,
                    ))
                } else {
                    Some((
                        rustel_audio::ModTarget::PulseWidthLfoDepth,
                        f64::from(lfo.depth),
                        false,
                    ))
                }
            }
            // `dry` is the direct path's gain; it exists only when the
            // pattern set it, since otherwise there is no node to reach.
            "dry" => Some((
                rustel_audio::ModTarget::Dry,
                object.get("dry").and_then(serde_json::Value::as_f64)?,
                false,
            )),
            // The post GainNode always exists.
            "postgain" => Some((
                rustel_audio::ModTarget::Postgain,
                object
                    .get("postgain")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(1.0),
                false,
            )),
            _ => None,
        }
    };
    // Each filter carries its own LFO on its frequency, built from the
    // lp/hp/bp-prefixed controls. It is the same
    // shape as a `lfo()` modulator on that filter, so it becomes one: rate,
    // depth and shape resolve here and the audio side runs the machinery it
    // already has.
    //
    // Present when any of rate, sync, depth, depthfrequency, shape or skew
    // is set.
    let filter_lfo = |kind: rustel_audio::FilterLfoKind,
                      gate: &str,
                      target: rustel_audio::ModTarget,
                      slot: &mut usize,
                      lfos: &mut [Option<rustel_audio::LfoMod>]|
     -> Result<(), String> {
        let prefix = kind.prefix();
        let field = |name: &str| {
            object
                .get(&format!("{prefix}{name}"))
                .and_then(serde_json::Value::as_f64)
        };
        let Some(frequency) = object.get(gate).and_then(serde_json::Value::as_f64) else {
            return Ok(());
        };
        let shape_control = format!("{prefix}shape");
        let shape = lfo_shape_index(object.get(&shape_control), &shape_control)?;
        let (rate, sync, depth, depth_frequency, skew) = (
            field("rate"),
            field("sync"),
            field("depth"),
            field("depthfrequency"),
            field("skew"),
        );
        if rate.is_none()
            && sync.is_none()
            && depth.is_none()
            && depth_frequency.is_none()
            && shape.is_none()
            && skew.is_none()
        {
            return Ok(());
        }
        if *slot >= rustel_audio::MAX_VOICE_MODS {
            return Ok(());
        }
        // `sync` counts cycles rather than seconds; absent both, the LFO runs
        // at one cycle per period.
        let frequency_hz = match sync {
            Some(sync) => sync * cps,
            None => rate.unwrap_or(cps),
        };
        let mod_depth = depth_frequency.unwrap_or(depth.unwrap_or(1.0) * frequency);
        let dcoffset = field("dc").unwrap_or(-0.5);
        // The LFO adds to the filter frequency, so its range is expressed
        // relative to that: the absolute cutoff stays within 30..20000.
        let min = -frequency + 30.0;
        let max = 20_000.0 - frequency;
        let phase0 = (musical_time * frequency_hz).rem_euclid(1.0);
        lfos[*slot] = Some(rustel_audio::LfoMod {
            fxi: None,
            target,
            frequency_hz: checked_f32(frequency_hz, "filter lfo rate")?,
            phase0: checked_f32(phase0, "filter lfo phase")?,
            depth: checked_f32(mod_depth, "filter lfo depth")?,
            dcoffset: checked_f32(dcoffset, "filter lfo dcoffset")?,
            skew: checked_f32(skew.unwrap_or(0.5), "filter lfo skew")?,
            curve: 1.0,
            shape: shape.unwrap_or(0),
            min: checked_f32(min, "filter lfo min")?,
            max: checked_f32(max, "filter lfo max")?,
            param_base: checked_f32(frequency, "filter lfo base")?,
            filter: Some(kind),
            // A filter builds this one itself; the pattern never named it, so
            // nothing can aim at it by id.
            id: None,
        });
        *slot += 1;
        Ok(())
    };

    // A modulator can have a name, so another modulator can aim at it:
    // `lfo({...}, 'cut')` then `lfo({ c: 'lfo_cut', sc: 'rate' })`. The
    // node map is keyed `lfo_{id}` and `subControl` picks the param. Ids
    // here are positions in the pattern's own map. They are assigned before
    // anything is resolved, so a modulator can name one declared after it.
    let modulator_ids = |kind: &str| {
        let mut ids: Vec<String> = Vec::new();
        if let Some(serde_json::Value::Object(map)) = object.get(kind) {
            for id in map.keys() {
                if id != "__ids" {
                    ids.push(id.clone());
                }
            }
        }
        ids
    };
    let lfo_ids = modulator_ids("lfo");
    let env_ids = modulator_ids("env");

    // A modulator may name one declared after it, so the entries are walked
    // twice: everything that stands alone is built first, then everything
    // that aims at one of those.
    let mut slots = [0usize; 2];
    for link_pass in [false, true] {
        for (kind_index, (kind, out_len)) in [("lfo", true), ("env", false)].into_iter().enumerate()
        {
            let Some(serde_json::Value::Object(map)) = object.get(kind) else {
                continue;
            };
            let ids = if out_len { &lfo_ids } else { &env_ids };
            let slot = &mut slots[kind_index];
            for (id, entry) in map {
                if id == "__ids" {
                    continue;
                }
                // Names another modulator, so it has to wait for the second
                // walk - by then every stand-alone modulator exists.
                let aims_at_a_modulator = entry
                    .get("control")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|control| {
                        control.starts_with("lfo_") || control.starts_with("env_")
                    });
                if aims_at_a_modulator != link_pass {
                    continue;
                }
                // Past the per-voice capacity an id has nowhere to live, and the
                // modulator itself would be skipped below anyway.
                let own_id = ids
                    .iter()
                    .position(|name| name == id)
                    .filter(|index| *index < rustel_audio::MAX_VOICE_MODS)
                    .map(|index| index as u8);
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                // `fxi` selects which chain a modulator aims at: the main
                // one ('main', which is also the key of the LAST stage), or
                // an `.FX()` stage by index. A stage that was never built has
                // nothing to modulate and the modulator is dropped rather
                // than silently landing on the main chain's parameter of the
                // same name.
                let fxi = match entry.get("fxi").filter(|value| !value.is_null()) {
                    None => None,
                    Some(serde_json::Value::String(name)) if name == "main" => None,
                    Some(value) => {
                        let Some(index) = value
                            .as_f64()
                            .or_else(|| value.as_str().and_then(|name| name.parse::<f64>().ok()))
                            .filter(|index| index.fract() == 0.0 && *index >= 0.0)
                        else {
                            skip(kind, &format!("fxi {value} is not an .FX() stage index"));
                            continue;
                        };
                        // The chains are numbered from ZERO and the last one
                        // is keyed 'main', so `fxi: 0` is the first `.FX()`
                        // stage rather than an alias for main.
                        let slot = index as usize;
                        if slot >= rustel_audio::MAX_FX_STAGES {
                            skip(kind, &format!("fxi {index} is past the stage capacity"));
                            continue;
                        }
                        // A stage that was never built simply never reads
                        // its bucket, dropping a modulator whose chain has no
                        // matching parameter.
                        Some(slot as u8)
                    }
                };
                let control = entry
                    .get("control")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("lfo");
                let sub_control = entry
                    .get("subControl")
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.is_empty());
                let resolved = match resolve_modulator_target(
                    kind,
                    control,
                    sub_control,
                    &lfo_ids,
                    &env_ids,
                    &lfos,
                    &envs,
                ) {
                    ModulatorTarget::NotAModulator => {
                        let source = match fxi.and_then(|slot| {
                            object
                                .get("FX")
                                .and_then(serde_json::Value::as_array)
                                .and_then(|stages| stages.get(usize::from(slot)))
                                .and_then(serde_json::Value::as_object)
                        }) {
                            Some(stage) => stage,
                            None => object,
                        };
                        resolve_target_in(source, control)
                    }
                    // Another modulator's rate is a param called `frequency`
                    // too; its depth, skew and the envelope's stages are not.
                    ModulatorTarget::Resolved(target, base) => Some((
                        target,
                        base,
                        matches!(
                            target,
                            rustel_audio::ModTarget::LfoParam(
                                _,
                                rustel_audio::ModulatorParam::Rate
                            )
                        ),
                    )),
                    ModulatorTarget::Missing => None,
                };
                let Some((target, base, is_frequency)) = resolved else {
                    skip(
                        kind,
                        &format!("modulator target '{control}' is not modulatable natively yet"),
                    );
                    continue;
                };
                // A param base of exactly 0 counts as 1.
                let current = if base == 0.0 { 1.0 } else { base };
                let depth_rel = entry_f64(entry, "depth").unwrap_or(1.0);
                let depth = entry_f64(entry, "depthabs").unwrap_or(depth_rel * current);
                // Only frequency params >= 30 Hz get clamped to the audible
                // range (relative to the base the mod ADDS to).
                let (range_min, range_max) = if is_frequency && current >= 30.0 {
                    (Some(20.0 - current), Some(24_000.0 - current))
                } else {
                    (None, None)
                };
                let slot_index = {
                    if *slot >= rustel_audio::MAX_VOICE_MODS {
                        skip(kind, "more modulators than the native per-voice capacity");
                        continue;
                    }
                    let index = *slot;
                    *slot += 1;
                    index
                };
                if out_len {
                    // LFO defaults and resolution.
                    let dcoffset = entry_f64(entry, "dcoffset").unwrap_or(-0.5);
                    let min = range_min.unwrap_or(dcoffset * depth);
                    let max = range_max.unwrap_or(dcoffset * depth + depth);
                    // frequency = sync !== undefined ? sync·cps : rate.
                    let frequency = match entry_f64(entry, "sync") {
                        Some(sync) => sync * cps,
                        None => entry_f64(entry, "rate").unwrap_or(1.0),
                    };
                    let retrig = entry_f64(entry, "retrig").unwrap_or(0.0);
                    let time = if retrig > 0.5 { 0.0 } else { musical_time };
                    let phaseoffset = entry_f64(entry, "phaseoffset").unwrap_or(0.0);
                    let phase0 = (time * frequency + phaseoffset).rem_euclid(1.0);
                    let shape = match entry.get("shape") {
                        None | Some(serde_json::Value::Null) => 0u8,
                        Some(serde_json::Value::Number(n)) => {
                            (n.as_f64().unwrap_or(0.0) as i64).rem_euclid(5) as u8
                        }
                        Some(serde_json::Value::String(name)) => match name.as_str() {
                            "tri" | "triangle" => 0,
                            "sine" => 1,
                            "ramp" => 2,
                            "saw" => 3,
                            "square" => 4,
                            _ => 0,
                        },
                        _ => 0,
                    };
                    lfos[slot_index] = Some(rustel_audio::LfoMod {
                        fxi,
                        target,
                        frequency_hz: checked_f32(frequency, "lfo rate")?,
                        phase0: checked_f32(phase0, "lfo phase")?,
                        depth: checked_f32(depth, "lfo depth")?,
                        dcoffset: checked_f32(dcoffset, "lfo dcoffset")?,
                        skew: checked_f32(entry_f64(entry, "skew").unwrap_or(0.5), "lfo skew")?,
                        curve: checked_f32(entry_f64(entry, "curve").unwrap_or(1.0), "lfo curve")?,
                        shape,
                        min: checked_f32(min, "lfo min")?,
                        max: checked_f32(max, "lfo max")?,
                        // The value the param holds, zero included. A NaN
                        // sample restores the param default from this value.
                        param_base: checked_f32(base, "lfo base")?,
                        // A pattern's own `lfo()`, not a filter's: `lpdepth` and
                        // its relatives must not reach this one even when it
                        // happens to modulate the same filter frequency.
                        filter: None,
                        id: own_id,
                    });
                } else {
                    let min = range_min.unwrap_or(-1e9);
                    let max = range_max.unwrap_or(1e9);
                    envs[slot_index] = Some(rustel_audio::EnvMod {
                        fxi,
                        target,
                        attack_secs: checked_f32(
                            entry_f64(entry, "attack").unwrap_or(0.005),
                            "env attack",
                        )?,
                        decay_secs: checked_f32(
                            entry_f64(entry, "decay").unwrap_or(0.14),
                            "env decay",
                        )?,
                        sustain: checked_f32(
                            entry_f64(entry, "sustain").unwrap_or(0.0),
                            "env sustain",
                        )?,
                        release_secs: checked_f32(
                            entry_f64(entry, "release").unwrap_or(0.1),
                            "env release",
                        )?,
                        a_curve: checked_f32(
                            entry_f64(entry, "acurve").unwrap_or(0.0),
                            "env acurve",
                        )?,
                        d_curve: checked_f32(
                            entry_f64(entry, "dcurve").unwrap_or(0.0),
                            "env dcurve",
                        )?,
                        r_curve: checked_f32(
                            entry_f64(entry, "rcurve").unwrap_or(0.0),
                            "env rcurve",
                        )?,
                        depth: checked_f32(depth, "env depth")?,
                        min: checked_f32(min, "env min")?,
                        max: checked_f32(max, "env max")?,
                        // Worklet susTime = endWithRelease − begin: the hap
                        // duration plus the VOICE's amplitude release,
                        // not the mod env's own release.
                        sustain_secs: checked_f32(duration_secs, "env span")? + release_secs,
                        param_base: checked_f32(current, "env base")?,
                        id: own_id,
                    });
                }
            }
        }
    }
    // Filter LFOs take whatever slots the pattern's own modulators left, so a
    // score using both keeps its `lfo()` calls and still gets its filter
    // sweep, up to the per-voice capacity.
    let mut filter_slot = lfos.iter().position(Option::is_none).unwrap_or(lfos.len());
    for (kind, gate, target) in [
        (
            rustel_audio::FilterLfoKind::Lowpass,
            "cutoff",
            rustel_audio::ModTarget::LowpassFreq,
        ),
        (
            rustel_audio::FilterLfoKind::Highpass,
            "hcutoff",
            rustel_audio::ModTarget::HighpassFreq,
        ),
        (
            rustel_audio::FilterLfoKind::Bandpass,
            "bandf",
            rustel_audio::ModTarget::BandFreq,
        ),
    ] {
        filter_lfo(kind, gate, target, &mut filter_slot, &mut lfos)?;
    }

    // Bus modulators, walked last and on their own: their signal is another
    // pattern's audio rather than another modulator, so nothing here can name
    // a slot that does not exist yet.
    if let Some(serde_json::Value::Object(map)) = object.get("bmod") {
        let mut slot = 0usize;
        for (id, entry) in map {
            if id == "__ids" {
                continue;
            }
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let Some(bus) = entry_f64(entry, "bus").or_else(|| entry_f64(entry, "b")) else {
                skip("bmod", "a bus modulator names no bus");
                continue;
            };
            if !(0.0..rustel_audio::MAX_BUSES as f64).contains(&bus) {
                skip(
                    "bmod",
                    &format!(
                        "bus {bus} is outside the native 0..{} range",
                        rustel_audio::MAX_BUSES
                    ),
                );
                continue;
            }
            let control = entry
                .get("control")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("gain");
            let Some((target, base, _is_frequency)) = resolve_target_in(object, control) else {
                skip(
                    "bmod",
                    &format!("modulator target '{control}' is not modulatable natively yet"),
                );
                continue;
            };
            if slot >= rustel_audio::MAX_VOICE_MODS {
                skip("bmod", "more modulators than the native per-voice capacity");
                continue;
            }
            // A param base of exactly 0 counts as 1, and depthabs wins over
            // the relative depth.
            let current = if base == 0.0 { 1.0 } else { base };
            let depth_rel = entry_f64(entry, "depth").unwrap_or(1.0);
            let depth = entry_f64(entry, "depthabs").unwrap_or(depth_rel * current);
            // A bus modulator takes no range. `connectBusModulator` builds the
            // clamp (a waveshaper carrying `clamp(x·max, min, max)`) and then
            // connects the target to the node it handed the shaper, not to the
            // shaper. The shaped signal goes nowhere and the param gets the
            // unclamped signal. An LFO or an envelope on the same target is
            // clamped, because each passes its range into the worklet.
            let (min, max) = (f64::NEG_INFINITY, f64::INFINITY);
            bus_mods[slot] = Some(rustel_audio::BusMod {
                fxi: None,
                bus: bus as u8,
                target,
                // `/0.3` is the bus signal's fixed normalisation, not a
                // tuning choice: `gainNode(sign(depth)·|depth| / 0.3)`.
                depth: checked_f32(depth / 0.3, "bmod depth")?,
                dc: checked_f32(entry_f64(entry, "dc").unwrap_or(0.0), "bmod dc")?,
                min: min as f32,
                max: max as f32,
                param_base: checked_f32(current, "bmod param base")?,
            });
            slot += 1;
        }
    }
    Ok((lfos, envs, bus_mods))
}

/// `partials`: an array of magnitudes, or a count of all-1 magnitudes
/// (`n` doubles as that count on synth sounds), scaled by the base
/// waveform's Fourier terms and rotated by optional `phases`.
/// The result is peak-normalized like createPeriodicWave's default.
fn partials_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::PartialsControls>, String> {
    let kind = match object.get("s").and_then(serde_json::Value::as_str) {
        Some(name) => name.to_ascii_lowercase(),
        None => return Ok(None),
    };
    // Sine and a stock waveform without partials skip the custom wave;
    // 'user' without partials falls back to triangle.
    if !matches!(
        kind.as_str(),
        "sawtooth" | "saw" | "square" | "sqr" | "triangle" | "tri" | "user"
    ) {
        return Ok(None);
    }
    let raw = object
        .get("partials")
        .filter(|v| !v.is_null())
        .or_else(|| object.get("n").filter(|v| !v.is_null()));
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mags: Vec<f64> = match raw {
        serde_json::Value::Array(items) => {
            items.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
        }
        other => match other.as_f64() {
            Some(count) if count >= 1.0 => {
                vec![1.0; (count as usize).min(rustel_audio::MAX_PARTIALS)]
            }
            _ => return Ok(None),
        },
    };
    if mags.is_empty() {
        return Ok(None);
    }
    if mags.len() > rustel_audio::MAX_PARTIALS {
        report_notice(
            format!(
                "{} partials requested; the first {} play",
                mags.len(),
                rustel_audio::MAX_PARTIALS
            ),
            serde_json::json!({
                "partials_truncated": {
                    "requested": mags.len(),
                    "kept": rustel_audio::MAX_PARTIALS
                }
            }),
        );
    }
    let phases: Vec<f64> = match object.get("phases") {
        Some(serde_json::Value::Array(items)) => {
            items.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
        }
        _ => Vec::new(),
    };
    let mut real = [0.0f32; rustel_audio::MAX_PARTIALS];
    let mut imag = [0.0f32; rustel_audio::MAX_PARTIALS];
    let len = mags.len().min(rustel_audio::MAX_PARTIALS);
    for (k, mag) in mags.iter().take(len).enumerate() {
        let n = (k + 1) as f64;
        let (r, i) = match kind.as_str() {
            "sawtooth" | "saw" => (0.0, -1.0 / n),
            "square" | "sqr" => (0.0, if (k + 1) % 2 == 0 { 0.0 } else { 1.0 / n }),
            "triangle" | "tri" => (if (k + 1) % 2 == 0 { 0.0 } else { 1.0 / (n * n) }, 0.0),
            _ => (0.0, 1.0), // 'user'
        };
        let mut r = r * mag;
        let mut i = i * mag;
        let phase = phases.get(k).copied().unwrap_or(0.0);
        if phase != 0.0 {
            let c = (std::f64::consts::TAU * phase).cos();
            let s = (std::f64::consts::TAU * phase).sin();
            let (r0, i0) = (r, i);
            r = c * r0 - s * i0;
            i = s * r0 + c * i0;
        }
        real[k] = r as f32;
        imag[k] = i as f32;
    }
    // createPeriodicWave normalizes to peak 1 by default.
    let mut peak = 0.0f64;
    for step in 0..2048 {
        let phi = step as f64 / 2048.0;
        let mut sum = 0.0f64;
        for k in 0..len {
            let angle = std::f64::consts::TAU * (k + 1) as f64 * phi;
            sum += f64::from(real[k]) * angle.cos() + f64::from(imag[k]) * angle.sin();
        }
        peak = peak.max(sum.abs());
    }
    // A list whose every magnitude is zero still builds a periodic wave in
    // the browser: an all-zero table, so the note is silent rather than a
    // stock waveform. `randL` hands one over at cycle zero.
    let norm = if peak > 0.0 { (1.0 / peak) as f32 } else { 0.0 };
    Ok(Some(rustel_audio::PartialsControls {
        real,
        imag,
        len: len as u8,
        norm,
    }))
}

/// The transient shaper's controls, shared by the main chain and by each
/// `.FX()` stage. `transsustain` defaults to 0 when only `transient` is
/// present.
fn transient_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::TransientControls>, String> {
    let Some(attack) = optional_f64(object.get("transient"), "transient")? else {
        return Ok(None);
    };
    Ok(Some(rustel_audio::TransientControls {
        attack: checked_f32(attack, "transient")?,
        sustain: checked_f32(
            optional_f64(object.get("transsustain"), "transsustain")?.unwrap_or(0.0),
            "transsustain",
        )?,
    }))
}

/// Compressor defaults. Shared by the main chain and by each `.FX()` stage,
/// which carries its own compressor node.
fn compressor_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::CompressorControls>, String> {
    let Some(threshold) = optional_f64(object.get("compressor"), "compressor")? else {
        return Ok(None);
    };
    // A DynamicsCompressorNode's settings are AudioParams, and each declares
    // a range that the browser clamps into. The same clamp is required here:
    // a `ratio` of 0 divides by zero in the gain computer and fills the
    // render with NaN. `compressor("-20:0:10:.002:.02")` reads as a ratio of
    // 0, because the second field is the ratio and not the knee. Chromium
    // plays it, because its AudioParam never lets the ratio below 1.
    //
    // Ranges from the DynamicsCompressorNode definition: threshold [-100, 0],
    // knee [0, 40], ratio [1, 20], attack [0, 1], release [0, 1].
    let clamped = |value: f64, low: f64, high: f64| value.clamp(low, high);
    Ok(Some(rustel_audio::CompressorControls {
        threshold_db: checked_f32(clamped(threshold, -100.0, 0.0), "compressor")?,
        ratio: checked_f32(
            clamped(
                optional_f64(object.get("compressorRatio"), "compressorRatio")?.unwrap_or(10.0),
                1.0,
                20.0,
            ),
            "compressorRatio",
        )?,
        knee_db: checked_f32(
            clamped(
                optional_f64(object.get("compressorKnee"), "compressorKnee")?.unwrap_or(10.0),
                0.0,
                40.0,
            ),
            "compressorKnee",
        )?,
        attack_secs: checked_f32(
            clamped(
                optional_f64(object.get("compressorAttack"), "compressorAttack")?.unwrap_or(0.005),
                0.0,
                1.0,
            ),
            "compressorAttack",
        )?,
        release_secs: checked_f32(
            clamped(
                optional_f64(object.get("compressorRelease"), "compressorRelease")?.unwrap_or(0.05),
                0.0,
                1.0,
            ),
            "compressorRelease",
        )?,
    }))
}

fn phaser_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    target_time: f64,
) -> Result<Option<rustel_audio::PhaserControls>, String> {
    let Some(rate) = optional_f64(object.get("phaserrate"), "phaserrate")? else {
        return Ok(None);
    };
    let depth = optional_f64(object.get("phaserdepth"), "phaserdepth")?.unwrap_or(0.75);
    if depth <= 0.0 {
        return Ok(None);
    }
    let center = optional_f64(object.get("phasercenter"), "phasercenter")?.unwrap_or(1000.0);
    let sweep = optional_f64(object.get("phasersweep"), "phasersweep")?.unwrap_or(2000.0);
    Ok(Some(rustel_audio::PhaserControls {
        rate_hz: checked_f32(rate, "phaserrate")?,
        depth: checked_f32(depth, "phaserdepth")?,
        center_hz: checked_f32(center, "phasercenter")?,
        sweep_cents: checked_f32(sweep, "phasersweep")?,
        time_secs: checked_f32(target_time, "phaser time")?,
    }))
}

/// Tremolo frequency comes from `tremolosync·cps` when
/// sync is set, else `tremolo`. Skew defaults to 1 (a ramp) unless a shape
/// was given. The start phase comes from the musical time (`cycle / cps`).
fn tremolo_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    musical_time: f64,
    cps: f64,
) -> Result<Option<rustel_audio::TremoloControls>, String> {
    let sync = optional_f64(object.get("tremolosync"), "tremolosync")?;
    let frequency = match sync {
        Some(sync) => sync * cps,
        None => match optional_f64(object.get("tremolo"), "tremolo")? {
            Some(rate) => rate,
            None => return Ok(None),
        },
    };
    let depth = optional_f64(object.get("tremolodepth"), "tremolodepth")?.unwrap_or(1.0);
    let shape_value = object.get("tremoloshape");
    let has_shape = !matches!(shape_value, None | Some(serde_json::Value::Null));
    let shape = match shape_value {
        None | Some(serde_json::Value::Null) => 0u8,
        Some(serde_json::Value::Number(n)) => {
            (n.as_f64().unwrap_or(0.0) as i64).rem_euclid(5) as u8
        }
        Some(serde_json::Value::String(name)) => match name.as_str() {
            "tri" | "triangle" => 0,
            "sine" => 1,
            "ramp" => 2,
            "saw" => 3,
            "square" => 4,
            _ => 0,
        },
        _ => 0,
    };
    let skew = optional_f64(object.get("tremoloskew"), "tremoloskew")?.unwrap_or(if has_shape {
        0.5
    } else {
        1.0
    });
    let phase_offset = optional_f64(object.get("tremolophase"), "tremolophase")?.unwrap_or(0.0);
    Ok(Some(rustel_audio::TremoloControls {
        frequency_hz: checked_f32(frequency, "tremolo")?,
        depth: checked_f32(depth, "tremolodepth")?,
        skew: checked_f32(skew, "tremoloskew")?,
        shape,
        phase_offset: checked_f32(phase_offset, "tremolophase")?,
        time_secs: checked_f32(musical_time, "tremolo time")?,
    }))
}

/// Tells the user why a note plays with no plugin.
fn plugin_skipped(call: &str, reason: &str) {
    report_notice(
        format!("{call} skipped: {reason}"),
        serde_json::json!({ "vst_skipped": { "message": reason } }),
    );
}

/// The effects a note asks for, in chain order. The hap carries a list
/// under `vst`, one plugin object for each `.vst()` call. An effect the
/// host cannot serve leaves its stage empty, and the note goes through the
/// other effects.
fn effect_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    samples: &dyn SampleLookup,
) -> [Option<rustel_audio::InsertControls>; rustel_audio::EFFECT_CHAIN] {
    let mut effects = [None; rustel_audio::EFFECT_CHAIN];
    let Some(serde_json::Value::Array(chain)) = object.get("vst") else {
        return effects;
    };
    if chain.len() > effects.len() {
        let most = effects.len();
        plugin_skipped(
            "vst",
            &format!("a note goes through {most} effect plugins at most"),
        );
    }
    for (stage, plugin) in effects.iter_mut().zip(chain) {
        *stage = plugin_controls(plugin, "vst", samples);
    }
    effects
}

/// The plugin one plugin object of a hap asks for, under `call`: `vst` for
/// an effect, `vsti` for an instrument.
///
/// The object is `{ name, preset, params: { key: value } }`. A request the
/// host cannot serve is not an error: the note plays with no plugin, and
/// the user gets one notice with the reason.
fn plugin_controls(
    plugin: &serde_json::Value,
    call: &str,
    samples: &dyn SampleLookup,
) -> Option<rustel_audio::InsertControls> {
    let plugin = plugin.as_object()?;
    let skip = |reason: &str| plugin_skipped(call, reason);
    let Some(name) = plugin.get("name").and_then(serde_json::Value::as_str) else {
        skip("the plugin name is not text");
        return None;
    };
    let preset = match plugin.get("preset") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(preset)) => Some(preset.clone()),
        Some(serde_json::Value::Number(preset)) => Some(preset.to_string()),
        Some(_) => {
            skip("the preset name is not text");
            return None;
        }
    };
    let mut params = Vec::new();
    if let Some(serde_json::Value::Object(values)) = plugin.get("params") {
        for (key, value) in values {
            match optional_f64(Some(value), key) {
                Ok(Some(value)) => params.push((key.as_str(), value)),
                Ok(None) => {}
                Err(reason) => {
                    skip(&reason);
                    return None;
                }
            }
        }
    }
    let request = PluginRequest {
        instrument: call == "vsti",
        name,
        preset: preset.as_deref(),
        params: &params,
    };
    match samples.insert(&request) {
        Ok(controls) => controls,
        Err(reason) => {
            skip(&reason);
            None
        }
    }
}

/// The vowel formant table, including the unicode aliases.
fn vowel_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::VowelControls>, String> {
    let Some(serde_json::Value::String(letter)) = object.get("vowel") else {
        return Ok(None);
    };
    let (freqs, gains, qs): ([f32; 5], [f32; 5], [f32; 5]) = match letter.as_str() {
        "a" => (
            [660., 1120., 2750., 3000., 3350.],
            [1., 0.5012, 0.0708, 0.0631, 0.0126],
            [80., 90., 120., 130., 140.],
        ),
        "e" => (
            [440., 1800., 2700., 3000., 3300.],
            [1., 0.1995, 0.1259, 0.1, 0.1],
            [70., 80., 100., 120., 120.],
        ),
        "i" => (
            [270., 1850., 2900., 3350., 3590.],
            [1., 0.0631, 0.0631, 0.0158, 0.0158],
            [40., 90., 100., 120., 120.],
        ),
        "o" => (
            [430., 820., 2700., 3000., 3300.],
            [1., 0.3162, 0.0501, 0.0794, 0.01995],
            [40., 80., 100., 120., 120.],
        ),
        "u" => (
            [370., 630., 2750., 3000., 3400.],
            [1., 0.1, 0.0708, 0.0316, 0.01995],
            [40., 60., 100., 120., 120.],
        ),
        "ae" | "æ" => (
            [650., 1515., 2400., 3000., 3350.],
            [1., 0.5, 0.1008, 0.0631, 0.0126],
            [80., 90., 120., 130., 140.],
        ),
        "aa" | "ɑ" | "å" => (
            [560., 900., 2570., 3000., 3300.],
            [1., 0.5, 0.0708, 0.0631, 0.0126],
            [80., 90., 120., 130., 140.],
        ),
        "oe" | "ø" | "ö" => (
            [500., 1430., 2300., 3000., 3300.],
            [1., 0.2, 0.0708, 0.0316, 0.01995],
            [40., 60., 100., 120., 120.],
        ),
        "ue" | "ü" => (
            [250., 1750., 2150., 3200., 3300.],
            [1., 0.1, 0.0708, 0.0316, 0.01995],
            [40., 60., 100., 120., 120.],
        ),
        "y" | "ı" => (
            [400., 1460., 2400., 3000., 3300.],
            [1., 0.2, 0.0708, 0.0316, 0.02995],
            [40., 60., 100., 120., 120.],
        ),
        "uh" => (
            [600., 1250., 2100., 3100., 3500.],
            [1., 0.3, 0.0608, 0.0316, 0.01995],
            [40., 70., 100., 120., 130.],
        ),
        "un" => (
            [500., 1240., 2280., 3000., 3500.],
            [1., 0.1, 0.1708, 0.0216, 0.02995],
            [40., 60., 100., 120., 120.],
        ),
        "en" => (
            [600., 1480., 2450., 3200., 3300.],
            [1., 0.15, 0.0708, 0.0316, 0.02995],
            [40., 60., 100., 120., 120.],
        ),
        "an" => (
            [700., 1050., 2500., 3000., 3300.],
            [1., 0.1, 0.0708, 0.0316, 0.02995],
            [40., 60., 100., 120., 120.],
        ),
        "on" => (
            [500., 1080., 2350., 3000., 3300.],
            [1., 0.1, 0.0708, 0.0316, 0.02995],
            [40., 60., 100., 120., 120.],
        ),
        // VowelNode throws for unknown letters and the voice never plays.
        other => return Err(format!("vowel: unknown vowel {other}")),
    };
    Ok(Some(rustel_audio::VowelControls { freqs, gains, qs }))
}

/// Pitch envelope: active when any of the penv family is set;
/// cents = penv·100, min = −cents·panchor, max = cents − cents·panchor,
/// ADSR defaults [0.2, 0.001, 1, 0.001], pcurve 0 linear / 1 exponential.
fn pitch_env_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::PitchEnvControls>, String> {
    let keys = ["pattack", "pdecay", "psustain", "prelease", "penv"];
    if !keys
        .iter()
        .any(|key| object.get(*key).is_some_and(|value| !value.is_null()))
    {
        return Ok(None);
    }
    let penv = optional_f64(object.get("penv"), "penv")?.unwrap_or(1.0);
    // ADSR defaults [0.2, 0.001, 1, 0.001]. The conditional sustain rule
    // matters: a decay with no sustain puts sustain at the 0.001 floor.
    // Through `panchor ?? psustain`, that anchors the sweep at the base
    // pitch and sends the attack up by the full penv.
    let (attack, decay, sustain, release) = adsr_values(
        [
            optional_f64(object.get("pattack"), "pattack")?,
            optional_f64(object.get("pdecay"), "pdecay")?,
            optional_f64(object.get("psustain"), "psustain")?,
            optional_f64(object.get("prelease"), "prelease")?,
        ],
        (0.2, 0.001, 1.0, 0.001),
    );
    let panchor = optional_f64(object.get("panchor"), "panchor")?.unwrap_or(sustain);
    let cents = penv * 100.0;
    let exponential = optional_f64(object.get("pcurve"), "pcurve")?.unwrap_or(0.0) == 1.0;
    Ok(Some(rustel_audio::PitchEnvControls {
        adsr: rustel_audio::FilterEnvelope {
            attack_secs: checked_f32(attack, "pattack")?,
            decay_secs: checked_f32(decay, "pdecay")?,
            sustain: checked_f64(sustain, "psustain")?,
            release_secs: checked_f32(release, "prelease")?,
            min_hz: checked_f64(0.0 - cents * panchor, "penv min")?,
            max_hz: checked_f64(cents - cents * panchor, "penv max")?,
        },
        exponential,
    }))
}

/// `.limit("-1:punchy")`: a brickwall limiter in line on this voice.
///
/// The threshold comes first, in dBFS. The character is optional and has a
/// name, as `distorttype` does. This is a creative effect, not output
/// protection: it limits one voice, and ten voices that are each under the
/// ceiling can still sum over it.
fn limit_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::LimiterSettings>, String> {
    let Some(threshold_db) = optional_f64(object.get("limit"), "limit")? else {
        return Ok(None);
    };
    let character = match object.get("limitchar") {
        None | Some(serde_json::Value::Null) => rustel_audio::LimiterCharacter::default(),
        Some(serde_json::Value::String(name)) => rustel_audio::LimiterCharacter::parse(name)
            .ok_or_else(|| {
                let names = rustel_audio::LimiterCharacter::ALL
                    .iter()
                    .map(|character| character.key())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("limitchar must be one of: {names}")
            })?,
        Some(serde_json::Value::Number(index)) => {
            let index = index
                .as_f64()
                .filter(|index| index.is_finite())
                .ok_or("limitchar must be a finite number or a name")?;
            let ladder = rustel_audio::LimiterCharacter::ALL;
            ladder[(index as i64).rem_euclid(ladder.len() as i64) as usize]
        }
        _ => return Err("limitchar must be a number or a character name".into()),
    };
    Ok(Some(rustel_audio::LimiterSettings {
        threshold_db: checked_f32(threshold_db, "limit")?,
        character,
    }))
}

fn distort_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<rustel_audio::DistortControls>, String> {
    // The `diode` control is the diode waveshaper under its own name: a
    // scalar is the amount, a `[:volume]` pair adds the postgain. The native
    // graph carries ONE distortion slot, so a hap that sets both folds: an
    // explicit `distort` keeps the amount and `diode`'s fills in only when
    // it is absent, `diode`'s volume fills in where `distortvol` is silent,
    // and `diode` picks the algorithm when `distorttype` does not name one.
    // The presets set only `diode`, which is the case that has to be exact.
    let diode = optional_value(object.get("diode"))
        .map(diode_controls)
        .transpose()?;
    let Some(amount) = optional_f64(object.get("distort"), "distort")?
        .or(diode.as_ref().map(|diode| f64::from(diode.amount)))
    else {
        return Ok(None);
    };
    let postgain = optional_f64(object.get("distortvol"), "distortvol")?
        .or_else(|| diode.as_ref().map(|diode| f64::from(diode.postgain)))
        .unwrap_or(1.0)
        .clamp(0.001, 1.0);
    let algorithm = match object.get("distorttype") {
        None | Some(serde_json::Value::Null) if diode.is_some() => {
            rustel_audio::DISTORTION_ALGORITHMS
                .iter()
                .position(|name| *name == "diode")
                .expect("the diode algorithm is in the table") as u8
        }
        None | Some(serde_json::Value::Null) => 0,
        Some(serde_json::Value::Number(index)) => {
            let index = index
                .as_f64()
                .filter(|index| index.is_finite())
                .ok_or("distorttype must be a finite number or a name")?;
            (index as i64).rem_euclid(rustel_audio::DISTORTION_ALGORITHMS.len() as i64) as u8
        }
        Some(serde_json::Value::String(name)) => rustel_audio::DISTORTION_ALGORITHMS
            .iter()
            .position(|candidate| candidate == name)
            .unwrap_or(0) as u8,
        _ => return Err("distorttype must be a number or an algorithm name".into()),
    };
    Ok(Some(rustel_audio::DistortControls {
        amount: checked_f32(amount, "distort")?,
        postgain: checked_f32(postgain, "distortvol")?,
        algorithm,
    }))
}

/// A control value that may legitimately be a non-number: `diode`'s volume
/// rides beside the amount, so a List must survive `optional_f64`'s refusal.
/// `null` reads as absent, the same call `optional_f64` makes.
fn optional_value(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    match value {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.clone()),
    }
}

/// The `diode` control's two numbers: a scalar amount, or an
/// `amount:volume` pair - `diode("2.5:.6")` and `diode([2.5,.6])` are the
/// same shaping at the same output level. A scalar alone leaves the
/// distortion's own output level untouched.
fn diode_controls(value: serde_json::Value) -> Result<rustel_audio::DistortControls, String> {
    let refuse = || "diode must be a finite number or an `amount:volume` pair of them".to_owned();
    let numbers = match &value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(refuse)
            })
            .collect::<Result<Vec<f64>, String>>()?,
        serde_json::Value::String(text) => text
            .split(':')
            .map(|piece| {
                piece
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .ok_or_else(refuse)
            })
            .collect::<Result<Vec<f64>, String>>()?,
        value => vec![
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(refuse)?,
        ],
    };
    let (amount, volume) = match numbers.as_slice() {
        [amount] => (*amount, 1.0),
        [amount, volume] => (*amount, *volume),
        _ => return Err("diode takes an amount and an optional `:volume`".into()),
    };
    Ok(rustel_audio::DistortControls {
        amount: checked_f32(amount, "diode")?,
        postgain: checked_f32(volume.clamp(0.001, 1.0), "diode volume")?,
        algorithm: rustel_audio::DISTORTION_ALGORITHMS
            .iter()
            .position(|name| *name == "diode")
            .expect("the diode algorithm is in the table") as u8,
    })
}

/// One `.FX(...)` stage - a complete per-voice effects chain per entry of
/// `FX`.
fn fx_stage_controls(
    object: &serde_json::Map<String, serde_json::Value>,
    target_time: f64,
    musical_time: f64,
    cps: f64,
    samples: &dyn SampleLookup,
) -> Result<rustel_audio::FxStage, String> {
    // A stage builds every effect in the chain. Two controls are ignored by
    // design: `orbit` and `duckorbit` do nothing inside a stage. Measured:
    // `.FX(orbit(2))` against no FX has correlation 1.000000 at an RMS
    // ratio of 0.8, which is this stage's default gain. The stage exists
    // and the control does nothing.
    //
    // `gain *= velocity`. The default gain is 0.8, not 1, so a stage that
    // names no gain still attenuates. Three such stages attenuate by 0.8^3,
    // which is audible.
    let gain = optional_f64(object.get("gain"), "gain")?.unwrap_or(0.8)
        * optional_f64(object.get("velocity"), "velocity")?.unwrap_or(1.0);
    Ok(rustel_audio::FxStage {
        gain: checked_f32(gain, "gain")?,
        filters: filter_controls(object)?,
        stretch: optional_f64(object.get("stretch"), "stretch")?
            .map(|v| checked_f32(v, "stretch"))
            .transpose()?,
        transient: transient_controls(object)?,
        vowel: vowel_controls(object)?,
        tremolo: tremolo_controls(object, musical_time, cps)?,
        compressor: compressor_controls(object)?,
        // StereoPanner axis, the same conversion the main chain makes.
        pan_x: optional_f64(object.get("pan"), "pan")?
            .map(|pan| checked_f32(2.0 * pan - 1.0, "pan"))
            .transpose()?,
        phaser: phaser_controls(object, target_time)?,
        delay: delay_controls(object, cps)?,
        room: reverb_controls(object, samples)?,
        dry: optional_f64(object.get("dry"), "dry")?
            .map(|d| checked_f32(d, "dry"))
            .transpose()?
            .unwrap_or(1.0),
        coarse: optional_f64(object.get("coarse"), "coarse")?
            .map(|c| checked_f32(c, "coarse"))
            .transpose()?,
        crush: optional_f64(object.get("crush"), "crush")?
            .map(|c| checked_f32(c, "crush"))
            .transpose()?,
        shape: match optional_f64(object.get("shape"), "shape")? {
            None => None,
            Some(shape) => Some(rustel_audio::ShapeControls {
                shape: checked_f32(shape, "shape")?,
                postgain: checked_f32(
                    optional_f64(object.get("shapevol"), "shapevol")?.unwrap_or(1.0),
                    "shapevol",
                )?,
            }),
        },
        distort: distort_controls(object)?,
    })
}

/// The `FX` array a pattern built with `.FX(...)`, resolved in order. The
/// hap's own params are NOT here: they are the main chain, applied last.
fn fx_stages(
    object: &serde_json::Map<String, serde_json::Value>,
    target_time: f64,
    musical_time: f64,
    cps: f64,
    samples: &dyn SampleLookup,
) -> Result<[Option<rustel_audio::FxStage>; rustel_audio::MAX_FX_STAGES], String> {
    let mut stages = [None; rustel_audio::MAX_FX_STAGES];
    let Some(serde_json::Value::Array(entries)) = object.get("FX") else {
        return Ok(stages);
    };
    let report = |message: String| {
        let record = serde_json::json!({ "fx_stage_skipped": { "message": message } });
        report_notice(message, record);
    };
    for (index, entry) in entries.iter().enumerate() {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if index >= rustel_audio::MAX_FX_STAGES {
            report(format!(
                "FX stage {index} is past the native per-voice capacity of {}",
                rustel_audio::MAX_FX_STAGES
            ));
            continue;
        }
        stages[index] = Some(fx_stage_controls(
            entry,
            target_time,
            musical_time,
            cps,
            samples,
        )?);
    }
    Ok(stages)
}

fn filter_controls(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<rustel_audio::FilterControls, String> {
    let lowpass = static_filter(object, "cutoff", "resonance")?;
    let highpass = static_filter(object, "hcutoff", "hresonance")?;
    let bandpass = static_filter(object, "bandf", "bandq")?;
    let model = match object.get("ftype") {
        None => Some(rustel_audio::FilterStages::One),
        Some(serde_json::Value::String(model)) if model == "12db" => {
            Some(rustel_audio::FilterStages::One)
        }
        Some(serde_json::Value::String(model)) if model == "24db" => {
            Some(rustel_audio::FilterStages::Two)
        }
        Some(serde_json::Value::String(model)) if model == "ladder" => {
            Some(rustel_audio::FilterStages::Ladder)
        }
        // A numeric ftype never selects the ladder: 0 and 1 both select one
        // biquad and 2 selects two. The string `ftype("ladder")` is the only
        // way to the ladder.
        //
        // Verified against Chromium: `ftype(0)`, `ftype(1)` and `ftype("12db")`
        // render bit-identically, `ftype(2)` matches `ftype("24db")`, and
        // `ftype("ladder")` matches neither. The docs' own
        // `.ftype("<0 1 2>")` example sweeps through 1, so 1 must not select
        // the ladder.
        Some(serde_json::Value::Number(model)) => {
            model
                .as_f64()
                .and_then(|model| match model.rem_euclid(3.0).floor() {
                    0.0 | 1.0 => Some(rustel_audio::FilterStages::One),
                    2.0 => Some(rustel_audio::FilterStages::Two),
                    _ => None,
                })
        }
        _ => None,
    };
    if (lowpass.is_some() || highpass.is_some() || bandpass.is_some()) && model.is_none() {
        return Err("unrecognized ftype filter model".to_owned());
    }
    Ok(rustel_audio::FilterControls {
        lowpass_envelope: filter_envelope(
            object,
            lowpass,
            ["lpattack", "lpdecay", "lpsustain", "lprelease"],
            "lpenv",
        )?,
        highpass_envelope: filter_envelope(
            object,
            highpass,
            ["hpattack", "hpdecay", "hpsustain", "hprelease"],
            "hpenv",
        )?,
        bandpass_envelope: filter_envelope(
            object,
            bandpass,
            ["bpattack", "bpdecay", "bpsustain", "bprelease"],
            "bpenv",
        )?,
        lowpass,
        highpass,
        bandpass,
        stages: model.unwrap_or_default(),
        drive: checked_f32(
            optional_f64(object.get("drive"), "drive")?.unwrap_or(0.69),
            "drive",
        )?,
    })
}

fn filter_envelope(
    object: &serde_json::Map<String, serde_json::Value>,
    filter: Option<rustel_audio::StaticBiquad>,
    adsr_names: [&str; 4],
    env_name: &str,
) -> Result<Option<rustel_audio::FilterEnvelope>, String> {
    let Some(filter) = filter else {
        return Ok(None);
    };
    let attack = optional_f64(object.get(adsr_names[0]), adsr_names[0])?;
    let decay = optional_f64(object.get(adsr_names[1]), adsr_names[1])?;
    let sustain = optional_f64(object.get(adsr_names[2]), adsr_names[2])?;
    let release = optional_f64(object.get(adsr_names[3]), adsr_names[3])?;
    let env = optional_f64(object.get(env_name), env_name)?;
    if attack.is_none()
        && decay.is_none()
        && sustain.is_none()
        && release.is_none()
        && env.is_none()
    {
        return Ok(None);
    }

    let (attack, decay, sustain, release) =
        if attack.is_none() && decay.is_none() && sustain.is_none() && release.is_none() {
            (0.005, 0.14, 0.0, 0.1)
        } else {
            let resolved_sustain = sustain.unwrap_or(
                if (attack.is_some() && decay.is_none()) || (attack.is_none() && decay.is_none()) {
                    1.0
                } else {
                    0.001
                },
            );
            (
                attack.unwrap_or(0.0).max(0.001),
                decay.unwrap_or(0.0).max(0.001),
                resolved_sustain.min(1.0),
                release.unwrap_or(0.0).max(0.01),
            )
        };
    let env = env.unwrap_or(1.0);
    let anchor = optional_f64(object.get("fanchor"), "fanchor")?.unwrap_or(0.0);
    let env_abs = env.abs();
    let offset = env_abs * anchor;
    let frequency = f64::from(filter.frequency_hz);
    let mut min = (2.0f64.powf(-offset) * frequency).clamp(0.0, 20_000.0);
    let mut max = (2.0f64.powf(env_abs - offset) * frequency).clamp(0.0, 20_000.0);
    if env < 0.0 {
        std::mem::swap(&mut min, &mut max);
    }
    Ok(Some(rustel_audio::FilterEnvelope {
        attack_secs: checked_f32(attack, adsr_names[0])?,
        decay_secs: checked_f32(decay, adsr_names[1])?,
        sustain: checked_f64(sustain, adsr_names[2])?,
        release_secs: checked_f32(release, adsr_names[3])?,
        min_hz: checked_f64(min, env_name)?,
        max_hz: checked_f64(max, env_name)?,
    }))
}

fn static_filter(
    object: &serde_json::Map<String, serde_json::Value>,
    frequency_name: &str,
    q_name: &str,
) -> Result<Option<rustel_audio::StaticBiquad>, String> {
    let Some(frequency) = optional_f64(object.get(frequency_name), frequency_name)? else {
        return Ok(None);
    };
    let q = optional_f64(object.get(q_name), q_name)?.unwrap_or(1.0);
    Ok(Some(rustel_audio::StaticBiquad {
        frequency_hz: checked_f32(frequency, frequency_name)?,
        q: checked_f32(q, q_name)?,
    }))
}

/// The f64 twin of [`checked_f32`], for the envelope bounds whose exact
/// cancellation the pitch envelope depends on.
fn checked_f64(value: f64, name: &str) -> Result<f64, String> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{name} is not finite"))
    }
}

/// `channels` selects the output destination for each source channel.
///
/// Output numbers start at 1. A source channel without an entry is dropped;
/// an unused output stays silent. Duplicate destinations sum, and order
/// matters: `"2:1"` swaps a stereo pair.
///
/// The stored form is `destination + 1`, leaving 0 to mean "not routed".
/// Destinations wrap on the output channel count when the voice is mixed.
/// This layer does not know that count, but rejects channel numbers below 1.
fn channel_route(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<[u8; 2]>, String> {
    let Some(value) = object.get("channels").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let mut destinations = [0u8; 2];
    let mut push = |index: usize, channel: f64| -> Result<(), String> {
        if !channel.is_finite() || channel < 1.0 {
            return Err(format!("channels must name outputs from 1, got {channel}"));
        }
        // Only the source channels that actually exist can be routed; a
        // longer list simply has nothing left to wire.
        if index < destinations.len() {
            destinations[index] = (channel as u64).min(u64::from(u8::MAX)) as u8;
        }
        Ok(())
    };
    match value {
        serde_json::Value::Number(_) => {
            push(
                0,
                optional_f64(Some(value), "channels")?.unwrap_or_default(),
            )?;
        }
        serde_json::Value::Array(list) => {
            for (index, entry) in list.iter().enumerate() {
                push(
                    index,
                    optional_f64(Some(entry), "channels")?.unwrap_or_default(),
                )?;
            }
        }
        _ => return Err("channels must be a number or a list of numbers".into()),
    }
    Ok(Some(destinations))
}

fn checked_f32(value: f64, name: &str) -> Result<f32, String> {
    if value.is_finite() && value.abs() <= f64::from(f32::MAX) {
        Ok(value as f32)
    } else {
        Err(format!("{name} is outside the finite f32 range"))
    }
}

fn optional_f64(value: Option<&serde_json::Value>, name: &str) -> Result<Option<f64>, String> {
    match value {
        None => Ok(None),
        // JS destructuring defaults only fire on `undefined`, but every
        // numeric use downstream coerces null to 0-or-default via `??` or
        // arithmetic; treating an explicit null as absent matches the
        // audible result (e.g. bare `.delay()`).
        Some(serde_json::Value::Null) => Ok(None),
        // JS numeric coercion: `2 * true - 1` pans right. `pan(brand)`
        // patterns booleans through controls.
        Some(serde_json::Value::Bool(b)) => Ok(Some(if *b { 1.0 } else { 0.0 })),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("{name} must be a finite number"))
            .map(Some),
    }
}

fn checked_frequency(frequency: f64) -> Result<f64, String> {
    if frequency.is_finite() && frequency > 0.0 {
        Ok(frequency)
    } else {
        Err(format!(
            "frequency must be finite and greater than zero, got {frequency}"
        ))
    }
}

/// Pinned MIDI→Hz, exported so hosts can pin the exact f64 grammar.
pub fn midi_to_hz(midi: f64) -> Result<f64, String> {
    if !midi.is_finite() {
        return Err(format!("MIDI note must be finite, got {midi}"));
    }
    checked_frequency(rustel_core::util::midi_to_freq(midi))
}

/// Use core's pinned note translation rather than creating a second
/// audio-only note grammar. An omitted octave defaults to 3.
pub fn note_to_hz(note: &str) -> Result<f64, String> {
    midi_to_hz(rustel_core::util::note_to_midi(note, 3)?)
}

// ---------------------------------------------------------------------------
// Native synth reference entries, consumed by the studio reference builder.
// ---------------------------------------------------------------------------

use rustel_core::reference::{ReferenceEntry, ReferenceParam};

const fn sound_param(
    name: &'static str,
    r#type: &'static str,
    description: &'static str,
) -> ReferenceParam {
    ReferenceParam {
        name,
        r#type,
        description,
    }
}

/// One entry per native synth sound, in the order of
/// [`NATIVE_SYNTH_SOUNDS`].
pub static SOUND_REFERENCE: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "sine",
        synonyms: &["sin"],
        summary: "a pure tone - the fundamental only, no harmonics",
        description: "The plainest oscillator voice: the fundamental alone, no harmonics. Everything the synth family shares - envelope, filters, FM, effects - is heard clearest on it. The same shape also runs as a signal between 0 and 1 under sine().",
        params: &[],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "triangle",
        synonyms: &["tri"],
        summary: "hollow and soft - odd harmonics, falling away fast",
        description: "Halfway between sine and square: odd harmonics only, falling off quicker than square's, which reads as soft and flute-like. Takes the whole synth path - envelope, filters, FM.",
        params: &[],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "square",
        synonyms: &["sqr"],
        summary: "hollow and woody - odd harmonics at full strength",
        description: "Odd harmonics at full strength: the classic hollow synth tone. For the same shape with a moving width, reach for pulse and pw instead.",
        params: &[],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "sawtooth",
        synonyms: &["saw"],
        summary: "bright and biting - every harmonic in the stack",
        description: "All the harmonics, gently falling: bright and biting, the workhorse synth tone - as much at home under a filter sweep as in an FM stack.",
        params: &[],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "supersaw",
        synonyms: &[],
        summary: "a stack of detuned saws - wide, chorused, trance-ready",
        description: "A unison stack of saw voices spread in pitch and in stereo: unison counts the voices (default 5, ceiling 32 - past it the event is refused), detune spreads their pitch (n supplies it too; default 0.18) and spread pans them apart (default 0.6).",
        params: &[
            sound_param(
                "unison",
                "number | Pattern",
                "how many saw voices; default 5, ceiling 32.",
            ),
            sound_param(
                "detune",
                "number | Pattern",
                "pitch spread between the voices; n supplies it too; default 0.18.",
            ),
            sound_param(
                "spread",
                "number | Pattern",
                "stereo spread, 0..1; default 0.6.",
            ),
        ],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "pulse",
        synonyms: &[],
        summary: "a variable-width pulse - thin to hollow",
        description: "A Tomisawa-style pulse: pw sets the width (default 0.5), and pwrate with pwsweep wobble it under an LFO - name either alone and the other falls back (rate 1, sweep 0.3).",
        params: &[
            sound_param("pw", "number | Pattern", "the pulse width; default 0.5."),
            sound_param(
                "pwrate",
                "number | Pattern",
                "the width LFO's rate in Hz; naming only pwsweep falls back to 1.",
            ),
            sound_param(
                "pwsweep",
                "number | Pattern",
                "the width LFO's depth; naming only pwrate falls back to 0.3; zero builds no LFO.",
            ),
        ],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "sbd",
        synonyms: &[],
        summary: "a synthesized bass drum - pitched thump with a noise click",
        description: "A triangle oscillator through a saturating shaper with an exponential pitch drop, plus a 25 ms brown-noise attack; the drum's own envelope is the whole sound, so it bypasses the usual ADSR. decay sets the body (default 0.5), pdecay and penv the pitch drop (0.5 seconds across 36 semitones), and a bare call lands on F1.",
        params: &[
            sound_param(
                "decay",
                "number | Pattern",
                "the drum's body length in seconds; default 0.5.",
            ),
            sound_param(
                "pdecay",
                "number | Pattern",
                "the pitch drop's time in seconds; default 0.5.",
            ),
            sound_param(
                "penv",
                "number | Pattern",
                "the pitch drop's depth in semitones; default 36.",
            ),
        ],
        examples: &[],
        tags: &["sound", "drum"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "white",
        synonyms: &[],
        summary: "white noise - every frequency at equal energy",
        description: "Every frequency at equal energy: the brightest of the noise colours. Shape it with the filters and the envelope like any voice.",
        params: &[],
        examples: &[],
        tags: &["sound", "noise"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "pink",
        synonyms: &[],
        summary: "pink noise - energy falling 3 dB an octave",
        description: "Energy falling 3 dB an octave: hiss like rain, softer than white. Shape it with the filters and the envelope like any voice.",
        params: &[],
        examples: &[],
        tags: &["sound", "noise"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "brown",
        synonyms: &[],
        summary: "brown noise - energy falling 6 dB an octave",
        description: "Energy falling 6 dB an octave: the deepest of the noise colours, a rumble more than a hiss. Shape it with the filters and the envelope like any voice.",
        params: &[],
        examples: &[],
        tags: &["sound", "noise"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "crackle",
        synonyms: &[],
        summary: "sparse impulses - a crackle you thin or thicken",
        description: "Each sample fires only when a random draw lands under density × 0.01, so density is the crackle's thickness - default 0.02, a sparse bed of ticks.",
        params: &[sound_param(
            "density",
            "number | Pattern",
            "impulse density; default 0.02.",
        )],
        examples: &[],
        tags: &["sound", "noise"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "bytebeat",
        synonyms: &[],
        summary: "one-line maths played as sound",
        description: "The demoscene trick: a counter walks a compiled expression and its low byte is the sample. n picks one of the built-in expressions; bbexpr (byteBeatExpression) writes your own as literal text, and an expression the compiler cannot take refuses the event rather than falling silent. byteBeatStartTime reseeds the counter.",
        params: &[
            sound_param("n", "number | Pattern", "which built-in expression plays."),
            sound_param(
                "bbexpr",
                "string",
                "your own bytebeat expression, as literal text.",
            ),
            sound_param(
                "byteBeatStartTime",
                "number | Pattern",
                "reseeds the expression's counter.",
            ),
        ],
        examples: &[],
        tags: &["sound"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "zzfx",
        synonyms: &[],
        summary: "the ZZFX chiptune generator - one voice, twenty knobs",
        description: "Builds its voice from the ZZFX generator's twenty parameters: zrand stirs randomness (default 0 here - the generator's own 0.05 stays off until asked), curve shapes the wave, slide and deltaSlide glide the pitch, pitchJump and pitchJumpTime leap, znoise mixes noise in, zmod modulates, zcrush crushes and zdelay delays. A zzfx([...]) array sets all twenty at once, in the generator's order. The zzfx control's reference lists each position and its default. The voice resolves its own pitch - freq, or the note control, or C2.",
        params: &[sound_param(
            "zzfx",
            "number[] | Pattern",
            "up to twenty numbers, in the generator's order - volume first, tremolo last; defaults fill the rest",
        )],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "z_sine",
        synonyms: &[],
        summary: "ZZFX with the sine shape",
        description: "One of the pinned ZZFX shapes: the generator with its waveform chosen for you, shaped by the same z* controls and the zzfx([...]) array as bare zzfx.",
        params: &[],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "z_sawtooth",
        synonyms: &[],
        summary: "ZZFX with the sawtooth shape",
        description: "One of the pinned ZZFX shapes: the generator with its waveform chosen for you, shaped by the same z* controls and the zzfx([...]) array as bare zzfx.",
        params: &[],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "z_triangle",
        synonyms: &[],
        summary: "ZZFX with the triangle shape",
        description: "One of the pinned ZZFX shapes: the generator with its waveform chosen for you, shaped by the same z* controls and the zzfx([...]) array as bare zzfx.",
        params: &[],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "z_square",
        synonyms: &[],
        summary: "ZZFX with the square shape",
        description: "One of the pinned ZZFX shapes: the square here is the triangle shape through curve 0 - the shape falls to the generator's default and curve is forced to 0, whatever the control says.",
        params: &[],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "z_tan",
        synonyms: &[],
        summary: "ZZFX with the tan shape",
        description: "One of the pinned ZZFX shapes - the harshest of the family: the generator through a tangent curve, shaped by the same z* controls and the zzfx([...]) array as bare zzfx.",
        params: &[],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "z_noise",
        synonyms: &[],
        summary: "ZZFX with the noise shape",
        description: "One of the pinned ZZFX shapes: the generator as noise, shaped by the same z* controls and the zzfx([...]) array as bare zzfx.",
        params: &[],
        examples: &[],
        tags: &["sound", "synth"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "in",
        synonyms: &[],
        summary: "live audio input - effects and rhythmic gating; n selects the channel",
        description: concat!(
            "Choose an input in the device picker's audio in section; the `none` row at the top of that list turns the input off again. `s(\"in\")` plays channel 0; `s(\"in:1\")` plays channel 1. `n` selects an input channel, not a pitch. With no input open, the source is silent.\n\n",
            "Effects: gain, pan, filters, distortion, delay and reverb process the incoming audio.\n\n",
            "Rhythm: `seg` / `segment` divides the pattern into listening windows. Adjacent windows can sound continuous; add `clip(.5)` to leave gaps. Envelopes shape each window.\n\n",
            "Requires recorded audio: `speed`, sample `begin` / `end`, and reverse playback with `speed(-1)` need a recorded sample. `note` does not retune the live input. `chop` can change event timing, but its sample slices do not select different parts of the stream; `rev` reverses event order, not the incoming waveform."
        ),
        params: &[],
        examples: &[
            "$: s(\"in\").lpf(2000).room(.5).delay(.3)",
            "$: s(\"in\").seg(8).clip(.5).attack(.005).release(.01).lpf(1200)",
            "$: stack(s(\"in\").pan(0), s(\"in:1\").pan(1))",
        ],
        tags: &["sound", "external_io"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

#[cfg(test)]
mod unplayable_tests {
    use super::*;

    /// The shapes `stack` produces from a control that was named but not
    /// called. Rendered by `FunctionRef`'s Debug impl, which is why the two
    /// live together.
    #[test]
    fn a_lane_that_is_a_function_says_so_instead_of_not_a_note() {
        for rendered in ["js fn #11 (cb 41)", "fn #7", "fn speed#3"] {
            let message = explain_unplayable(rendered, "not a note".to_owned());
            assert!(
                message.contains("never called"),
                "{rendered} should be explained as an uncalled control, got: {message}"
            );
        }
    }

    /// A genuinely wrong note name must keep its own error: the hint would be
    /// misleading, and the note text is what the artist needs to see.
    #[test]
    fn a_misspelled_note_keeps_the_parser_error() {
        let message = explain_unplayable("h4", "not a note: \"h4\"".to_owned());
        assert_eq!(message, "not a note: \"h4\"");
        // A sound whose name merely starts with "fn" is not a function.
        let message = explain_unplayable("fnky", "not a note: \"fnky\"".to_owned());
        assert_eq!(message, "not a note: \"fnky\"");
    }
}

/// The pitch a sample voice takes when the score names no note.
///
/// An ordinary sample bank uses midi 36; a `gm_*` soundfont uses c3, midi 48,
/// because the soundfont loader carries its own default an octave above the
/// sampler's.
fn sample_default_midi(_object: &serde_json::Map<String, serde_json::Value>, name: &str) -> f64 {
    if name.starts_with("gm_") { 48.0 } else { 36.0 }
}

#[cfg(test)]
mod channel_control_tests {
    fn route(value: serde_json::Value) -> Result<Option<[u8; 2]>, String> {
        let object = serde_json::json!({ "s": "bd", "channels": value });
        super::channel_route(object.as_object().expect("object"))
    }

    /// The stored form is `destination + 1`, so 0 reads as "this source
    /// channel goes nowhere".
    #[test]
    fn channels_parses_a_number_or_a_list_and_refuses_an_output_below_one() {
        assert_eq!(
            super::channel_route(serde_json::json!({ "s": "bd" }).as_object().unwrap()),
            Ok(None),
            "no control means plain stereo"
        );

        // A bare number routes the FIRST source channel and drops the second.
        assert_eq!(route(serde_json::json!(2)), Ok(Some([2, 0])));
        assert_eq!(route(serde_json::json!(1)), Ok(Some([1, 0])));

        // A list wires them in order, so "2:1" swaps the pair.
        assert_eq!(route(serde_json::json!([1, 2])), Ok(Some([1, 2])));
        assert_eq!(route(serde_json::json!([2, 1])), Ok(Some([2, 1])));
        assert_eq!(route(serde_json::json!([1, 1])), Ok(Some([1, 1])));

        // A list longer than the source has channels leaves the rest
        // unwired.
        assert_eq!(route(serde_json::json!([1, 2, 3])), Ok(Some([1, 2])));

        // Below 1 would map to -1, which WebAudio throws on.
        assert!(
            route(serde_json::json!(0)).is_err(),
            "channels(0) must be refused"
        );
        assert!(route(serde_json::json!(-1)).is_err());
        assert!(route(serde_json::json!([1, 0])).is_err());

        // Wrapping happens where the voice is mixed, not here: the parser
        // keeps whatever output was named.
        assert_eq!(route(serde_json::json!(3)), Ok(Some([3, 0])));
    }
}

#[cfg(test)]
mod compressor_range_tests {
    /// Each compressor setting clamps into its AudioParam range. A ratio of
    /// 0 would divide by zero in the gain computer and render NaN.
    #[test]
    fn compressor_settings_clamp_into_their_audioparam_ranges() {
        let of = |json: serde_json::Value| {
            super::compressor_controls(json.as_object().expect("object"))
                .expect("resolves")
                .expect("present")
        };

        // ratio [1, 20] - the one that produced the NaN.
        let zero = of(serde_json::json!({ "compressor": -20.0, "compressorRatio": 0.0 }));
        assert_eq!(zero.ratio, 1.0, "a ratio of 0 divides by itself downstream");
        assert_eq!(
            of(serde_json::json!({ "compressor": -20.0, "compressorRatio": 100.0 })).ratio,
            20.0
        );
        assert_eq!(
            of(serde_json::json!({ "compressor": -20.0, "compressorRatio": -5.0 })).ratio,
            1.0
        );

        // threshold [-100, 0], knee [0, 40], attack and release [0, 1].
        assert_eq!(
            of(serde_json::json!({ "compressor": 20.0 })).threshold_db,
            0.0
        );
        assert_eq!(
            of(serde_json::json!({ "compressor": -200.0 })).threshold_db,
            -100.0
        );
        assert_eq!(
            of(serde_json::json!({ "compressor": -20.0, "compressorKnee": 100.0 })).knee_db,
            40.0
        );
        assert_eq!(
            of(serde_json::json!({ "compressor": -20.0, "compressorKnee": -10.0 })).knee_db,
            0.0
        );
        assert_eq!(
            of(serde_json::json!({ "compressor": -20.0, "compressorAttack": 5.0 })).attack_secs,
            1.0
        );
        assert_eq!(
            of(serde_json::json!({ "compressor": -20.0, "compressorRelease": -1.0 })).release_secs,
            0.0
        );

        // In-range settings pass through untouched.
        let plain = of(serde_json::json!({
            "compressor": -20.0, "compressorRatio": 10.0, "compressorKnee": 10.0,
            "compressorAttack": 0.002, "compressorRelease": 0.02
        }));
        assert_eq!(plain.threshold_db, -20.0);
        assert_eq!(plain.ratio, 10.0);
        assert_eq!(plain.knee_db, 10.0);
    }
}

#[cfg(test)]
mod bank_prefix_tests {
    fn prefixed(object: serde_json::Value, name: &str) -> Option<String> {
        super::bank_prefixed(object.as_object().expect("object"), name).expect("resolves")
    }

    /// `s` becomes `{bank}_{s}` before any dispatch on the name, so a bank
    /// can decide which engine plays a sound. `s("basique").bank("wt_digital")`
    /// must play the same as `s("wt_digital_basique")`.
    #[test]
    fn the_bank_prefix_decides_whether_a_sound_is_a_wavetable() {
        assert_eq!(
            prefixed(serde_json::json!({ "bank": "wt_digital" }), "basique").as_deref(),
            Some("wt_digital_basique"),
            "the prefixed name is what the wt_ test must see"
        );
        assert!(
            prefixed(serde_json::json!({ "bank": "wt_digital" }), "basique")
                .expect("prefixed")
                .starts_with("wt_"),
            "a wavetable bank makes a plain name a wavetable"
        );

        // No bank leaves the name alone, so the caller keeps what it had.
        assert_eq!(prefixed(serde_json::json!({}), "basique"), None);
        assert_eq!(
            prefixed(serde_json::json!({ "bank": null }), "basique"),
            None
        );

        // An empty `s` has nothing to prefix.
        assert_eq!(prefixed(serde_json::json!({ "bank": "tr909" }), ""), None);

        // `.bank('9000')` numerifies under miniAllStrings and the prefix
        // rule stringifies it right back.
        assert_eq!(
            prefixed(serde_json::json!({ "bank": 9000 }), "bd").as_deref(),
            Some("9000_bd")
        );

        // And an ordinary drum bank still composes the way it always did.
        assert_eq!(
            prefixed(serde_json::json!({ "bank": "tr909" }), "bd").as_deref(),
            Some("tr909_bd")
        );
    }
}

#[cfg(test)]
mod zzfx_resolve_tests {
    fn resolved(object: serde_json::Value) -> rustel_audio::OnsetEvent {
        super::resolve_voice(&object, 1, 0.5, 0.0, 48_000, 0.5).expect("resolves")
    }

    /// ZzFX bakes its envelope into the generated samples, which play with
    /// no outer ADSR. The hap's ADSR keys are zzfx parameters only.
    #[test]
    fn a_zzfx_voice_gets_a_flat_outer_envelope_whatever_its_adsr_keys_say() {
        let event = resolved(serde_json::json!({
            "s": "z_sawtooth", "note": "c3",
            "attack": 0.05, "decay": 0.1, "sustain": 0.5, "release": 0.3,
        }));
        let envelope = event.controls.envelope;
        assert_eq!(envelope.attack_secs, 0.0);
        assert_eq!(envelope.decay_secs, 0.0);
        assert_eq!(envelope.sustain, 1.0);
        assert_eq!(envelope.release_secs, 0.0);
        let Some(rustel_audio::SynthSource::ZzFx { params }) = event.synth else {
            panic!("expected a zzfx source, got {:?}", event.synth);
        };
        // ...while the keys land where they belong: in the zzfx params.
        assert_eq!(params.attack, 0.05);
        assert_eq!(params.decay, 0.1);
        assert_eq!(params.sustain_volume, 0.5);
        assert_eq!(params.release, 0.3);
        // sustain TIME is what the hap leaves after attack+decay.
        assert!((params.sustain - (0.5 - 0.05 - 0.1)).abs() < 1e-12);
    }

    /// zzfx resolves its own pitch: `freq ?? midiToFreq(note ?? 36)`, so the
    /// default is MIDI 36 (C2). `z_square` uses shape -1 through curve 0,
    /// which makes it square.
    #[test]
    fn zzfx_pitch_and_shape_follow_the_strudel_quirks() {
        let Some(rustel_audio::SynthSource::ZzFx { params }) =
            resolved(serde_json::json!({ "s": "z_sine" })).synth
        else {
            panic!("no source")
        };
        assert!(
            (params.frequency - 65.406_391_325_149_66).abs() < 1e-9,
            "no note means midi 36 (C2), got {} Hz",
            params.frequency
        );

        let Some(rustel_audio::SynthSource::ZzFx { params }) =
            resolved(serde_json::json!({ "s": "z_square", "note": "c3", "curve": 3.0 })).synth
        else {
            panic!("no source")
        };
        assert_eq!(
            params.shape, -1.0,
            "indexOf misses and -1 || 0 keeps the -1"
        );
        assert_eq!(
            params.shape_curve, 0.0,
            "square forces curve 0 over the control"
        );
    }
}

#[cfg(test)]
mod envelope_resolve_tests {
    use super::*;
    use serde_json::json;

    fn adsr(envelope: rustel_audio::Envelope) -> [f32; 4] {
        [
            envelope.attack_secs,
            envelope.decay_secs,
            envelope.sustain,
            envelope.release_secs,
        ]
    }

    fn resolved_adsr(value: &serde_json::Value) -> [f32; 4] {
        let event = resolve_voice(value, 1, 0.5, 0.0, 48_000, 0.5).expect("resolves");
        adsr(event.controls.envelope)
    }

    #[test]
    fn bus_defaults_keep_full_sustain() {
        assert_eq!(
            resolved_adsr(&json!({ "s": "bus", "n": 1 })),
            [0.001, 0.05, 1.0, 0.01]
        );
    }

    #[test]
    fn explicit_bus_adsr_keeps_the_shared_control_rules() {
        for (mut controls, expected) in [
            (
                json!({ "attack": 0.02, "decay": 0.3, "sustain": 0.4, "release": 0.5 }),
                [0.02, 0.3, 0.4, 0.5],
            ),
            (json!({ "attack": 0.2 }), [0.2, 0.001, 1.0, 0.01]),
            (json!({ "decay": 0.2 }), [0.001, 0.2, 0.001, 0.01]),
            (json!({ "sustain": 0.7 }), [0.001, 0.001, 0.7, 0.01]),
            (json!({ "release": 0.2 }), [0.001, 0.001, 1.0, 0.2]),
            (
                json!({ "attack": 0, "decay": 0, "sustain": 0, "release": 0 }),
                [0.001, 0.001, 0.0, 0.01],
            ),
        ] {
            controls["s"] = json!("bus");
            assert_eq!(resolved_adsr(&controls), expected, "controls: {controls}");
        }
    }

    #[test]
    fn a_banked_sample_named_bus_keeps_sample_envelope_defaults() {
        struct BankedBus;

        impl SampleLookup for BankedBus {
            fn resolve(&self, sound: &str, _index: f64, _midi: f64) -> SampleResolution {
                assert_eq!(sound, "fixture_bus");
                SampleResolution::Found {
                    id: rustel_audio::SampleId(7),
                    transpose: 0.0,
                    duration_secs: 2.0,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    soundfont: false,
                }
            }
        }

        let event = resolve_voice_with_samples(
            &json!({ "s": "bus", "bank": "fixture" }),
            1,
            0.5,
            0.0,
            48_000,
            0.5,
            &BankedBus,
        )
        .expect("banked sample resolves");
        assert!(event.sample.is_some());
        assert_eq!(adsr(event.controls.envelope), [0.001, 0.001, 1.0, 0.01]);
    }
}

#[cfg(test)]
mod lfo_time_base_tests {
    use super::*;

    /// The clock reads 10.4 s on cycle 3/4. At 0.5 cps the musical time is
    /// 1.5 s.
    #[test]
    fn tremolo_filter_lfo_and_lfo_modulator_start_on_the_musical_time_and_the_rest_on_the_clock() {
        let value = serde_json::json!({
            "s": "pulse", "note": 36, "pwrate": 2,
            "cutoff": 800, "lprate": 3, "tremolo": 5, "phaserrate": 2,
            "lfo": {
                "free": { "control": "cutoff", "rate": 7 },
                "held": { "control": "cutoff", "rate": 7, "retrig": 1 }
            },
            "FX": [{ "tremolo": 5, "phaserrate": 2 }]
        });
        let event = resolve_voice_with_samples_detailed(
            &value,
            1,
            0.25,
            10.4,
            0.75,
            48_000,
            0.5,
            &BundledOnly,
        )
        .expect("resolve");
        let controls = &event.controls;
        let stage = controls.fx_stages[0].expect("stage");
        let phase0 = |pick: &dyn Fn(&rustel_audio::LfoMod) -> bool| {
            let lfo = controls.lfos.iter().flatten().find(|lfo| pick(lfo));
            lfo.expect("lfo").phase0
        };

        // frac(1.5 * 7) and frac(1.5 * 3) are 0.5. A `retrig` starts at 0.
        assert_eq!(phase0(&|lfo| lfo.id == Some(0)), 0.5);
        assert_eq!(phase0(&|lfo| lfo.id == Some(1)), 0.0);
        assert_eq!(phase0(&|lfo| lfo.filter.is_some()), 0.5);
        assert_eq!(controls.tremolo.expect("tremolo").time_secs, 1.5);
        assert_eq!(stage.tremolo.expect("stage tremolo").time_secs, 1.5);

        assert_eq!(controls.phaser.expect("phaser").time_secs, 10.4);
        assert_eq!(stage.phaser.expect("stage phaser").time_secs, 10.4);
        let Some(rustel_audio::SynthSource::Pulse {
            width_lfo: Some(width_lfo),
            ..
        }) = event.synth
        else {
            panic!("no pulse-width LFO")
        };
        assert_eq!(width_lfo.time_secs, 10.4);
        assert_eq!(controls.worklet_begin_secs, 10.4);

        // An entry point with no cycle takes the musical time from the clock.
        let clocked = resolve_voice(&value, 1, 0.25, 10.4, 48_000, 0.5).expect("resolve");
        assert_eq!(clocked.controls.tremolo.expect("tremolo").time_secs, 10.4);
    }
}
