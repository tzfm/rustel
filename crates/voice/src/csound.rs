use serde_json::{Map, Value};

/// The complete controls and timing of one Csound score event.
pub struct CsoundRequest<'a> {
    pub value: &'a Map<String, Value>,
    pub frequency: f64,
    pub duration_secs: f64,
    pub target_time: f64,
}

pub(super) fn frequency(value: &Map<String, Value>) -> Result<f64, String> {
    if let Some(frequency) = super::optional_f64(value.get("freq"), "freq")?
        && frequency != 0.0
    {
        return super::checked_frequency(frequency);
    }
    let pitch = ["note", "n", "value"]
        .into_iter()
        .filter_map(|name| value.get(name))
        .find(|value| !value.is_null());
    match pitch {
        Some(Value::Number(note)) => super::midi_to_hz(
            note.as_f64()
                .ok_or_else(|| "Csound needs a finite MIDI note".to_string())?,
        ),
        Some(Value::String(note)) => super::note_to_hz(note),
        _ => Err("Csound needs a note or frequency".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SampleLookup, SampleResolution, resolve_voice_with_samples};
    use rustel_audio::{InsertControls, InsertKey};
    use serde_json::json;

    struct Csound;

    impl SampleLookup for Csound {
        fn resolve(&self, _: &str, _: f64, _: f64) -> SampleResolution {
            panic!("a Csound event must not resolve a native sound")
        }

        fn csound(&self, request: &CsoundRequest<'_>) -> Result<InsertControls, String> {
            assert_eq!(request.frequency, 440.0);
            assert_eq!(request.duration_secs, 0.25);
            assert_eq!(request.target_time, 0.125);
            assert!([json!(0.3), json!("0.3"), json!([0.3])].contains(&request.value["gain"]));
            assert!([json!(0.2), json!("0.2"), json!([0.2])].contains(&request.value["velocity"]));
            assert_eq!(request.value["custom"], "held");
            Ok(InsertControls::new(InsertKey {
                plugin: u32::MAX,
                preset: 3,
            })
            .with_score(17))
        }
    }

    #[test]
    fn csound_owns_the_source_and_gain_while_native_routing_remains() {
        for mode in ["csound", "csoundm"] {
            let mut value = json!({
                "note": 69, "s": "unavailable", "gain": 0.3, "velocity": 0.2,
                "custom": "held", "pan": 0.1, "postgain": 0.7,
                "cutoff": 800, "orbit": 2, "delay": 0.4, "octave": 3
            });
            value[mode] = json!("voice");
            let event =
                resolve_voice_with_samples(&value, 1, 0.25, 0.125, 48_000, 0.5, &Csound).unwrap();
            assert_eq!(event.gain, 1.0);
            assert_eq!(event.controls.velocity, 1.0);
            assert_eq!(event.controls.pan, Some(0.1));
            assert_eq!(event.controls.postgain, 0.7);
            assert_eq!(event.controls.orbit, 2);
            assert!(event.controls.delay.is_some());
            assert!(event.controls.filters.lowpass.is_some());
            assert!(event.sample.is_none() && event.synth.is_none() && event.wavetable.is_none());
            let instrument = event.controls.instrument.unwrap();
            assert_eq!(instrument.score(), Some(17));
            assert_eq!(instrument.note(), None);
            for (gain, velocity) in [(json!("0.3"), json!("0.2")), (json!([0.3]), json!([0.2]))] {
                value["gain"] = gain;
                value["velocity"] = velocity;
                let event =
                    resolve_voice_with_samples(&value, 1, 0.25, 0.125, 48_000, 0.5, &Csound)
                        .unwrap();
                assert_eq!(event.gain, 1.0);
                assert_eq!(event.controls.velocity, 1.0);
            }
        }
    }

    #[test]
    fn pitch_uses_nullish_fallbacks_and_requires_a_pitch() {
        for value in [
            json!({"freq": 440, "note": 0}),
            json!({"freq": 0, "note": 69}),
            json!({"note": null, "n": 69}),
            json!({"n": null, "value": "a4"}),
        ] {
            assert_eq!(frequency(value.as_object().unwrap()).unwrap(), 440.0);
        }
        assert_eq!(
            frequency(json!({"note": 0, "n": 69}).as_object().unwrap()).unwrap(),
            crate::midi_to_hz(0.0).unwrap()
        );
        assert!(frequency(json!({}).as_object().unwrap()).is_err());
        let unavailable = resolve_voice_with_samples(
            &json!({"note": 69, "csound": "triangle"}),
            1,
            0.25,
            0.0,
            48_000,
            0.5,
            &crate::BundledOnly,
        )
        .unwrap_err();
        assert_eq!(unavailable, "this host has no Csound support");
    }
}
