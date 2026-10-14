//! Native worklet rendering across voice timing, FX ordering and pool reuse.

use rustel_audio::{
    Envelope, FilterControls, FxStage, KabelControls, OnsetEvent, OscillatorControls,
    ScalarBackend, render_pcm, rustel_kabel::Program,
};

const RATE: u32 = 48_000;

fn stereo(signal: &str) -> Program {
    Program::from_json(&format!(
        r#"{{"nodes":[{signal},{{"type":"n","value":0}},{{"type":"n","value":1}},{{"type":"output","inputs":[0,1]}},{{"type":"output","inputs":[0,2]}}]}}"#
    ))
    .unwrap()
}

fn worklet(program: Program, start: u64, gate_end: u64, end: u64) -> KabelControls {
    KabelControls {
        program: std::sync::Arc::new(program),
        start_secs: start as f64 / f64::from(RATE),
        gate_end_secs: gate_end as f64 / f64::from(RATE),
        end_secs: end as f64 / f64::from(RATE),
    }
}

fn event(program: Program, start: u64, end: u64, gain: f32) -> OnsetEvent {
    OnsetEvent::new(start, 440.0, gain, 0.001).with_controls(OscillatorControls {
        kabel: Some(worklet(program, start, start + 48, end)),
        envelope: Envelope {
            attack_secs: 0.0,
            decay_secs: 0.0,
            sustain: 1.0,
            release_secs: 0.0,
        },
        ..OscillatorControls::default()
    })
}

fn stage(kabel: KabelControls, gain: f32) -> FxStage {
    FxStage {
        kabel: Some(kabel),
        stretch: None,
        transient: None,
        gain,
        filters: FilterControls::default(),
        vowel: None,
        coarse: None,
        crush: None,
        shape: None,
        distort: None,
        tremolo: None,
        compressor: None,
        pan_x: None,
        phaser: None,
        delay: None,
        dry: 1.0,
        room: None,
    }
}

#[test]
fn generated_stereo_uses_native_gain_and_outlives_source_envelope() {
    let graph = stereo(r#"{"type":"n","value":1}"#);
    for gain in [0.0, 0.25, 0.75] {
        let mut backend = ScalarBackend::new();
        let pcm = render_pcm(&mut backend, RATE, 512, &[event(graph, 0, 256, gain)]).unwrap();
        assert!(
            pcm[..512]
                .iter()
                .all(|sample| (*sample - gain).abs() < 1e-6)
        );
        assert!(pcm[512..].iter().all(|sample| *sample == 0.0));
        assert_eq!(backend.missing_kabel_events(), 0);
    }
}

#[test]
fn start_and_gate_follow_absolute_render_quanta() {
    let graph = stereo(r#"{"type":"gate"}"#);
    let mut onset = event(graph, 1, 512, 1.0);
    onset.controls.kabel = Some(worklet(graph, 1, 128, 512));
    let pcm = render_pcm(&mut ScalarBackend::new(), RATE, 640, &[onset]).unwrap();
    assert!(pcm[..256].iter().all(|sample| *sample == 0.0));
    assert!(
        pcm[256..512]
            .iter()
            .all(|sample| (*sample - 1.0).abs() < 1e-6)
    );
    assert!(pcm[512..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn fx_worklet_precedes_stage_gain_and_main_worklet_receives_the_result() {
    let constant = stereo(r#"{"type":"n","value":1}"#);
    let passthrough = stereo(r#"{"type":"audioin"}"#);
    let mut onset = event(passthrough, 0, 256, 0.5);
    onset.controls.fx_stages[0] = Some(stage(worklet(constant, 0, 48, 256), 0.25));
    let pcm = render_pcm(&mut ScalarBackend::new(), RATE, 256, &[onset]).unwrap();
    assert!(pcm.iter().all(|sample| (*sample - 0.125).abs() < 1e-6));
}

#[test]
fn long_sequential_scores_reuse_processor_storage() {
    let constant = stereo(r#"{"type":"n","value":0.25}"#);
    let events: Vec<_> = (0..200)
        .map(|index| event(constant, index * 1024, index * 1024 + 256, 1.0))
        .collect();
    let mut backend = ScalarBackend::new();
    let pcm = render_pcm(&mut backend, RATE, 200 * 1024, &events).unwrap();
    let mut one = ScalarBackend::new();
    render_pcm(&mut one, RATE, 1024, &events[..1]).unwrap();
    assert_eq!(backend.kabel_resident_bytes(), one.kabel_resident_bytes());
    assert_eq!(backend.missing_kabel_events(), 0);
    assert!(pcm.iter().any(|sample| *sample > 0.0));
}

#[test]
fn overlapping_graphs_have_independent_state_and_mix() {
    let clock = stereo(r#"{"type":"time"}"#);
    let events = [event(clock, 0, 512, 1.0), event(clock, 128, 512, 1.0)];
    let mut backend = ScalarBackend::new();
    let pcm = render_pcm(&mut backend, RATE, 640, &events).unwrap();
    for (frame, samples) in pcm.as_chunks::<2>().0.iter().enumerate() {
        let expected = if frame >= 512 {
            0.0
        } else {
            (frame + frame.saturating_sub(128)) as f32 / RATE as f32
        };
        assert!(
            samples
                .iter()
                .all(|sample| (*sample - expected).abs() < 1e-6),
            "frame {frame}: {samples:?} != {expected}"
        );
    }
    assert_eq!(backend.missing_kabel_events(), 0);
}

#[test]
fn offline_render_reports_processor_capacity_instead_of_silent_notes() {
    let constant = stereo(r#"{"type":"n","value":1}"#);
    let events = vec![event(constant, 0, 256, 1.0); rustel_audio::MAX_KABEL_GRAPHS + 1];
    let error = render_pcm(&mut ScalarBackend::new(), RATE, 512, &events).unwrap_err();
    assert!(error.contains("128-graph"), "{error}");
}
