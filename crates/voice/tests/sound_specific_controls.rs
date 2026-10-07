use rustel_audio::{AudioBackend, DecodedSample, OnsetEvent, SampleId, ScalarBackend, SynthSource};
use rustel_voice::{SampleLookup, SampleResolution, resolve_voice_with_samples};
use serde_json::json;

struct Wavetable;

impl SampleLookup for Wavetable {
    fn resolve(&self, sound: &str, _n: f64, _midi: f64) -> SampleResolution {
        if sound != "wt_digital_basique" {
            return SampleResolution::Unknown;
        }
        SampleResolution::Found {
            id: SampleId(9),
            transpose: 0.0,
            duration_secs: 1.0,
            loop_secs: None,
            envelope_peak: 1.0,
            soundfont: false,
        }
    }
}

fn event(sound: &str, unison: f64, detune: f64, spread: f64) -> OnsetEvent {
    let value = if sound == "supersaw" {
        json!({"s": sound, "note": 60, "unison": unison, "detune": detune, "spread": spread})
    } else {
        json!({"s": "basique", "bank": "wt_digital", "note": 60,
            "unison": unison, "detune": detune, "spread": spread})
    };
    resolve_voice_with_samples(&value, 1, 0.2, 0.0, 48_000, 1.0, &Wavetable).unwrap()
}

fn render(event: OnsetEvent) -> Vec<f32> {
    let mut backend = ScalarBackend::prepared(48_000, 1).unwrap();
    let pcm = (0..2048)
        .map(|i| ((i as f32 / 2048.0) * std::f32::consts::TAU).sin())
        .collect();
    backend
        .install_sample(
            SampleId(9),
            Box::new(DecodedSample::from_parts(48_000, 1, pcm).unwrap()),
        )
        .unwrap();
    backend.note(event);
    let mut output = vec![0.0; 2048 * 2];
    backend.process_block(&mut output, 2048);
    assert!(output.iter().any(|sample| sample.abs() > 0.001));
    output
}

fn difference(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| (left - right).abs())
        .sum()
}

fn stereo_difference(output: &[f32]) -> f32 {
    output
        .as_chunks::<2>()
        .0
        .iter()
        .map(|lr| (lr[0] - lr[1]).abs())
        .sum()
}

#[test]
fn sound_specific_controls_reach_the_native_voice_and_render_path() {
    for sound in ["supersaw", "wavetable"] {
        let single = event(sound, 1.0, 0.0, 0.0);
        let multi = event(sound, 5.0, 0.0, 0.0);
        match (single.synth, multi.synth) {
            (
                Some(SynthSource::Supersaw { voices: 1.0, .. }),
                Some(SynthSource::Supersaw { voices: 5.0, .. }),
            ) => {}
            (None, None)
                if single.wavetable.unwrap().voices == 1.0
                    && multi.wavetable.unwrap().voices == 5.0 => {}
            other => panic!("{sound} did not keep the requested unison count: {other:?}"),
        }

        let mono = render(single);
        let detuned_single = render(event(sound, 1.0, 1.0, 0.0));
        let spread_single = render(event(sound, 1.0, 0.0, 1.0));
        assert_eq!(mono, detuned_single, "{sound}: one voice cannot detune");
        assert_eq!(mono, spread_single, "{sound}: one voice cannot spread");

        let centred = render(multi);
        let detuned = render(event(sound, 5.0, 1.0, 0.0));
        let wide = render(event(sound, 5.0, 0.0, 1.0));
        assert!(
            difference(&centred, &detuned) > 0.1,
            "{sound}: detune was inaudible"
        );
        assert!(
            stereo_difference(&wide) > stereo_difference(&centred) + 0.1,
            "{sound}: spread did not widen the stereo output"
        );
    }
}
