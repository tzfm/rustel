use rustel_audio::live_control::{LiveControlUpdate, SLIDER_DECLICK_SECS, SLIDER_SMOOTHING_SECS};
use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_audio::{
    AudioBackend, Envelope, FilterControls, FilterEnvelope, OnsetEvent, OscillatorControls,
    ScalarBackend, StaticBiquad, Waveform,
};
use std::sync::Mutex;

#[global_allocator]
static ALLOCATOR: TripwireAlloc = TripwireAlloc;
static SERIAL: Mutex<()> = Mutex::new(());
const RATE: u32 = 48_000;

fn tone(gain: f32, bindings: [u64; 3], cutoff: Option<f32>) -> ScalarBackend {
    tone_with(gain, bindings, lowpass(cutoff, None))
}

fn lowpass(cutoff: Option<f32>, envelope: Option<FilterEnvelope>) -> FilterControls {
    FilterControls {
        lowpass: cutoff.map(|frequency_hz| StaticBiquad {
            frequency_hz,
            q: 0.0,
        }),
        lowpass_envelope: envelope,
        ..FilterControls::default()
    }
}

fn tone_with(gain: f32, bindings: [u64; 3], filters: FilterControls) -> ScalarBackend {
    let mut backend = ScalarBackend::prepared(RATE, 8).unwrap();
    assert!(
        backend.try_note_prepared(OnsetEvent::new(0, 1000.0, gain, 5.0).with_controls(
            OscillatorControls {
                limit: None,
                live_controls: bindings,
                waveform: Waveform::Sine,
                envelope: Envelope {
                    attack_secs: 0.0,
                    decay_secs: 0.0,
                    sustain: 1.0,
                    release_secs: 0.01
                },
                filters,
                ..OscillatorControls::default()
            }
        ))
    );
    backend
}

fn render(backend: &mut ScalarBackend, frames: usize) -> Vec<f32> {
    let mut output = vec![0.0; frames * 2];
    backend.process_block(&mut output, frames);
    output
}

/// Frames an exact update takes to reach its target.
fn declick_frames() -> usize {
    (RATE as f32 * SLIDER_DECLICK_SECS).round() as usize
}

#[test]
fn sustained_zero_gain_fades_in_sample_by_sample_and_exact_edits_replace_the_glide() {
    let _serial = SERIAL.lock().unwrap();
    let mut fading = tone(0.0, [7, 0, 0], None);
    let mut reference = tone(1.0, [7, 0, 0], None);
    assert!(render(&mut fading, 128).iter().all(|value| *value == 0.0));
    render(&mut reference, 128);
    fading.set_live_control(LiveControlUpdate {
        binding: 7,
        value: 1.0,
        smooth: true,
    });
    let frames = (RATE as f32 * SLIDER_SMOOTHING_SECS).round() as usize;
    let actual = render(&mut fading, frames);
    let full = render(&mut reference, frames);
    for (frame, (actual, full)) in actual
        .as_chunks::<2>()
        .0
        .iter()
        .zip(full.as_chunks::<2>().0)
        .enumerate()
    {
        let fraction = (frame + 1) as f32 / frames as f32;
        assert!(
            (actual[0] - full[0] * fraction).abs() < 0.00001,
            "frame {frame}: {} != {}",
            actual[0],
            full[0] * fraction
        );
    }
    // One note is still sounding: no query, new onset, or phase restart.
    fading.set_live_control(LiveControlUpdate {
        binding: 7,
        value: 0.0,
        smooth: true,
    });
    render(&mut fading, 64);
    render(&mut reference, 64);
    fading.set_live_control(LiveControlUpdate {
        binding: 7,
        value: 0.125,
        smooth: false,
    });
    render(&mut fading, declick_frames());
    render(&mut reference, declick_frames());
    for (actual, full) in render(&mut fading, 256)
        .into_iter()
        .zip(render(&mut reference, 256))
    {
        assert!((actual - full * 0.125).abs() < 0.000001);
    }
}

/// A drag with smoothing off sends exact updates. Each one must cross a
/// short ramp: a step in gain is a click, and a run of steps is distortion.
#[test]
fn an_exact_gain_update_does_not_step_a_sounding_voice() {
    let _serial = SERIAL.lock().unwrap();
    let mut voice = tone(1.0, [7, 0, 0], None);
    let steady = render(&mut voice, 1024);
    let slope = |samples: &[f32]| {
        samples
            .chunks(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max)
    };
    let steady_slope = slope(&steady);
    voice.set_live_control(LiveControlUpdate {
        binding: 7,
        value: 0.25,
        smooth: false,
    });
    let moving = render(&mut voice, declick_frames());
    // Join the last steady frame to the first moving frame as well.
    let joined = [&steady[steady.len() - 2..], &moving[..]].concat();
    assert!(
        slope(&joined) <= steady_slope * 1.05,
        "the update stepped the signal: {} > {steady_slope}",
        slope(&joined)
    );
    // After the ramp the voice is at the exact target.
    let mut reference = tone(1.0, [0, 0, 0], None);
    render(&mut reference, 1024 + declick_frames());
    for (actual, full) in render(&mut voice, 256)
        .into_iter()
        .zip(render(&mut reference, 256))
    {
        assert!((actual - full * 0.25).abs() < 0.000001);
    }
}

#[test]
fn filter_sliders_change_the_existing_voice_and_leave_unbound_voices_alone() {
    let _serial = SERIAL.lock().unwrap();
    // The cutoff opens far above the 1 kHz tone. The resonance peaks on it,
    // also under an envelope that holds the cutoff there.
    let held = FilterEnvelope {
        attack_secs: 0.0,
        decay_secs: 0.0,
        sustain: 1.0,
        release_secs: 0.01,
        min_hz: 1000.0,
        max_hz: 1000.0,
    };
    for (bindings, filters, value) in [
        ([0, 9, 0], lowpass(Some(100.0), None), 4000.0),
        ([0, 0, 9], lowpass(Some(1000.0), None), 30.0),
        ([0, 0, 9], lowpass(Some(1000.0), Some(held)), 30.0),
    ] {
        let mut bound = tone_with(0.5, bindings, filters);
        let mut unbound = tone_with(0.5, [0; 3], filters);
        render(&mut bound, 4096);
        render(&mut unbound, 4096);
        let update = LiveControlUpdate {
            binding: 9,
            value,
            smooth: true,
        };
        bound.set_live_control(update);
        unbound.set_live_control(update);
        render(&mut bound, 2048);
        render(&mut unbound, 2048);
        let power = |samples: Vec<f32>| samples.iter().map(|sample| sample * sample).sum::<f32>();
        let opened = power(render(&mut bound, 2048));
        let unchanged = power(render(&mut unbound, 2048));
        assert!(
            opened > unchanged * 100.0,
            "{bindings:?}: bound {opened}, unbound {unchanged}"
        );
    }
}

#[test]
fn live_updates_and_filter_ramps_allocate_nothing_in_the_callback() {
    let _serial = SERIAL.lock().unwrap();
    let mut backend = tone(0.0, [7, 9, 10], Some(200.0));
    let mut block = [0.0; 256];
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        backend.process_block(&mut block, 128);
        backend.set_live_control(LiveControlUpdate {
            binding: 7,
            value: 0.75,
            smooth: true,
        });
        backend.set_live_control(LiveControlUpdate {
            binding: 9,
            value: 4000.0,
            smooth: true,
        });
        backend.set_live_control(LiveControlUpdate {
            binding: 10,
            value: 12.0,
            smooth: true,
        });
        for _ in 0..20 {
            backend.process_block(&mut block, 128);
        }
    });
    assert_eq!(Violations::capture().since(before), Violations::default());
    assert!(block.iter().any(|sample| sample.abs() > 0.01));
}

#[test]
fn prefetched_onsets_take_the_latest_target_without_touching_another_evaluation() {
    let _serial = SERIAL.lock().unwrap();
    // These notes are queued but not activated yet, as in the live horizon.
    let mut pending = tone(0.0, [7, 0, 0], None);
    let mut other_evaluation = tone(0.0, [8, 0, 0], None);
    let mut reference = tone(0.75, [7, 0, 0], None);
    let update = LiveControlUpdate {
        binding: 7,
        value: 0.75,
        smooth: false,
    };
    pending.set_live_control(update);
    other_evaluation.set_live_control(update);
    assert_eq!(render(&mut pending, 512), render(&mut reference, 512));
    assert!(
        render(&mut other_evaluation, 512)
            .iter()
            .all(|value| *value == 0.0)
    );
}

#[test]
fn live_gain_stays_after_nonlinear_fx_when_a_silent_voice_fades_in() {
    let _serial = SERIAL.lock().unwrap();
    let voice = |gain, binding| {
        let mut backend = ScalarBackend::prepared(RATE, 2).unwrap();
        let mut controls = OscillatorControls {
            limit: None,
            live_controls: [binding, 0, 0],
            envelope: Envelope {
                attack_secs: 0.0,
                decay_secs: 0.0,
                sustain: 1.0,
                release_secs: 0.01,
            },
            velocity: 0.7,
            ..OscillatorControls::default()
        };
        controls.fx_stages[0] = Some(rustel_audio::FxStage {
            stretch: None,
            transient: None,
            gain: 0.8,
            filters: FilterControls::default(),
            vowel: None,
            coarse: None,
            crush: Some(2.0),
            shape: None,
            distort: None,
            tremolo: None,
            compressor: None,
            pan_x: None,
            phaser: None,
            delay: None,
            dry: 1.0,
            room: None,
        });
        assert!(
            backend
                .try_note_prepared(OnsetEvent::new(0, 1000.0, gain, 5.0).with_controls(controls))
        );
        backend
    };
    let mut bound = voice(0.0, 7);
    let mut reference = voice(1.0, 0);
    render(&mut bound, 512);
    render(&mut reference, 512);
    bound.set_live_control(LiveControlUpdate {
        binding: 7,
        value: 0.25,
        smooth: false,
    });
    render(&mut bound, declick_frames());
    render(&mut reference, declick_frames());
    for (actual, full) in render(&mut bound, 512)
        .into_iter()
        .zip(render(&mut reference, 512))
    {
        assert!((actual - full * 0.25).abs() < 0.000001);
    }
}
