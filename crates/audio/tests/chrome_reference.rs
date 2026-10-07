use std::path::{Path, PathBuf};

/// Peak and RMS of an interleaved stereo buffer for comparison with the
/// browser reference.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    peak: f32,
    rms: f32,
}

impl Metrics {
    fn from_interleaved_stereo(pcm: &[f32]) -> Self {
        let mut peak = 0.0f32;
        let mut sum_squares = 0.0f64;
        for &sample in pcm {
            peak = peak.max(sample.abs());
            sum_squares += f64::from(sample) * f64::from(sample);
        }
        Self {
            peak,
            rms: (sum_squares / pcm.len().max(1) as f64).sqrt() as f32,
        }
    }
}

use rustel_audio::{
    BUNDLED_BD_SAMPLE_ID, Envelope, FilterControls, FilterEnvelope, FilterStages, OnsetEvent,
    OscillatorControls, SampleControls, SampleHold, ScalarBackend, StaticBiquad, Waveform,
    render_pcm,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Baseline {
    scope: String,
    cases: Vec<BaselineCase>,
}

#[derive(Deserialize)]
struct BaselineCase {
    id: String,
    fixture: String,
    reference: Capture,
    negative_control: NegativeControl,
}

#[derive(Deserialize)]
struct Capture {
    peak: f64,
    rms: f64,
    first_audible_frame: u64,
    last_audible_frame: u64,
    channel_metrics: Vec<ChannelMetrics>,
}

#[derive(Deserialize)]
struct ChannelMetrics {
    peak: f64,
    rms: f64,
}

#[derive(Deserialize)]
struct NegativeControl {
    comparison_status: String,
    relative_rms_delta: f64,
}

#[derive(Deserialize)]
struct Fixture {
    id: String,
    events: Vec<FixtureEvent>,
}

#[derive(Deserialize)]
struct FixtureEvent {
    time_seconds: f64,
    duration_seconds: f32,
    value: FixtureValue,
}

#[derive(Deserialize)]
struct FixtureValue {
    s: String,
    note: Option<String>,
    freq: Option<f32>,
    gain: f32,
    velocity: Option<f32>,
    postgain: Option<f32>,
    attack: Option<f32>,
    decay: Option<f32>,
    sustain: Option<f32>,
    release: Option<f32>,
    pan: Option<f32>,
    cutoff: Option<f32>,
    resonance: Option<f32>,
    hcutoff: Option<f32>,
    hresonance: Option<f32>,
    bandf: Option<f32>,
    bandq: Option<f32>,
    ftype: Option<serde_json::Value>,
    fanchor: Option<f32>,
    lpenv: Option<f32>,
    lpattack: Option<f32>,
    lpdecay: Option<f32>,
    lpsustain: Option<f32>,
    lprelease: Option<f32>,
    hpenv: Option<f32>,
    hpattack: Option<f32>,
    hpdecay: Option<f32>,
    hpsustain: Option<f32>,
    hprelease: Option<f32>,
    bpenv: Option<f32>,
    bpattack: Option<f32>,
    bpdecay: Option<f32>,
    bpsustain: Option<f32>,
    bprelease: Option<f32>,
    speed: Option<f32>,
    begin: Option<f32>,
    end: Option<f32>,
}

fn repo_file(relative: impl AsRef<Path>) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn relative_delta(left: f64, right: f64) -> f64 {
    (left - right).abs() / left.abs().max(f64::MIN_POSITIVE)
}

fn note_frequency(note: &str) -> f32 {
    match note {
        "c4" => 261.625_58,
        "c3" => 130.812_79,
        "e4" => 329.627_56,
        "a4" => 440.0,
        _ => panic!("fixture uses an unpinned note spelling {note:?}"),
    }
}

fn sample_playback_rate(value: &FixtureValue) -> f32 {
    let midi = if let Some(freq) = value.freq {
        69.0 + 12.0 * (freq / 440.0).log2()
    } else {
        match value.note.as_deref() {
            Some("c3") => 48.0,
            Some("c4") => 60.0,
            Some(note) => panic!("fixture uses an unpinned sample note {note:?}"),
            None => 36.0,
        }
    };
    value.speed.unwrap_or(1.0).abs() * 2.0f32.powf((midi - 36.0) / 12.0)
}

fn waveform(name: &str) -> Waveform {
    match name {
        "sine" => Waveform::Sine,
        "triangle" => Waveform::Triangle,
        "square" => Waveform::Square,
        "sawtooth" => Waveform::Sawtooth,
        _ => panic!("fixture uses an unsupported oscillator {name:?}"),
    }
}

fn envelope(value: &FixtureValue) -> Envelope {
    let (attack, decay, sustain, release) = if value.attack.is_none()
        && value.decay.is_none()
        && value.sustain.is_none()
        && value.release.is_none()
    {
        if value.s == "bd" {
            (0.001, 0.001, 1.0, 0.01)
        } else {
            (0.001, 0.05, 0.6, 0.01)
        }
    } else {
        let sustain = value.sustain.unwrap_or(
            if (value.attack.is_some() && value.decay.is_none())
                || (value.attack.is_none() && value.decay.is_none())
            {
                1.0
            } else {
                0.001
            },
        );
        (
            value.attack.unwrap_or(0.0).max(0.001),
            value.decay.unwrap_or(0.0).max(0.001),
            sustain.min(1.0),
            value.release.unwrap_or(0.0).max(0.01),
        )
    };
    Envelope {
        attack_secs: attack,
        decay_secs: decay,
        sustain,
        release_secs: release,
    }
}

fn native_event(event: &FixtureEvent, sample_rate: u32) -> OnsetEvent {
    let is_sample = event.value.s == "bd";
    let frequency = if is_sample {
        0.0
    } else {
        event
            .value
            .freq
            .unwrap_or_else(|| note_frequency(event.value.note.as_deref().expect("note or freq")))
    };
    let onset = OnsetEvent::new(
        (event.time_seconds * f64::from(sample_rate)).round() as u64,
        frequency,
        event.value.gain,
        event.duration_seconds,
    )
    .with_controls(OscillatorControls {
        limit: None,
        live_controls: [0; 2],
        preview_epoch: 0,
        choke_only: false,
        piano: false,
        noise: 0.0,
        modulator_release_secs: 0.01,
        worklet_begin_secs: 0.0,
        lfo_end_secs: f32::INFINITY,
        filter_lfo_end_secs: f32::INFINITY,
        bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
        bus: None,
        busgain: 1.0,
        channels: None,
        waveform: if is_sample {
            Waveform::Sine
        } else {
            waveform(&event.value.s)
        },
        envelope: envelope(&event.value),
        velocity: event.value.velocity.unwrap_or(1.0),
        postgain: event.value.postgain.unwrap_or(1.0),
        pan: event.value.pan,
        filters: FilterControls {
            lowpass_envelope: filter_envelope(
                event.value.cutoff,
                event.value.fanchor,
                [
                    event.value.lpenv,
                    event.value.lpattack,
                    event.value.lpdecay,
                    event.value.lpsustain,
                    event.value.lprelease,
                ],
            ),
            highpass_envelope: filter_envelope(
                event.value.hcutoff,
                event.value.fanchor,
                [
                    event.value.hpenv,
                    event.value.hpattack,
                    event.value.hpdecay,
                    event.value.hpsustain,
                    event.value.hprelease,
                ],
            ),
            bandpass_envelope: filter_envelope(
                event.value.bandf,
                event.value.fanchor,
                [
                    event.value.bpenv,
                    event.value.bpattack,
                    event.value.bpdecay,
                    event.value.bpsustain,
                    event.value.bprelease,
                ],
            ),
            lowpass: event.value.cutoff.map(|frequency_hz| StaticBiquad {
                frequency_hz,
                q: event.value.resonance.unwrap_or(1.0),
            }),
            highpass: event.value.hcutoff.map(|frequency_hz| StaticBiquad {
                frequency_hz,
                q: event.value.hresonance.unwrap_or(1.0),
            }),
            bandpass: event.value.bandf.map(|frequency_hz| StaticBiquad {
                frequency_hz,
                q: event.value.bandq.unwrap_or(1.0),
            }),
            stages: match event.value.ftype.as_ref() {
                Some(serde_json::Value::String(value)) if value == "24db" => FilterStages::Two,
                Some(serde_json::Value::Number(value))
                    if value
                        .as_f64()
                        .is_some_and(|value| value.rem_euclid(3.0).floor() == 2.0) =>
                {
                    FilterStages::Two
                }
                _ => FilterStages::One,
            },
            ..FilterControls::default()
        },
        distort: None,
        delay: None,
        duck: None,
        reverb: None,
        dry: None,
        stretch: None,
        fm: None,
        orbit: 1,
        lfos: [None; rustel_audio::MAX_VOICE_MODS],
        envs: [None; rustel_audio::MAX_VOICE_MODS],
        phaser: None,
        tremolo: None,
        vibrato: None,
        pitch_env: None,
        djf: None,
        compressor: None,
        partials: None,
        transient: None,
        fx_stages: [None; rustel_audio::MAX_FX_STAGES],
        vowel: None,
        coarse: None,
        crush: None,
        shape: None,
    });
    if is_sample {
        onset.with_sample(SampleControls {
            sample: BUNDLED_BD_SAMPLE_ID,
            playback_rate: sample_playback_rate(&event.value),
            begin: event.value.begin.unwrap_or(0.0),
            end: event.value.end.unwrap_or(1.0),
            hold: if event.value.release.is_some() {
                SampleHold::Hap
            } else {
                SampleHold::Slice
            },
            muted: false,
            loop_secs: None,
            envelope_peak: 1.0,
            reversed: false,
            nudge_secs: 0.0,
            cut: None,
        })
    } else {
        onset
    }
}

/// One filter's frequency envelope, mirroring `rustel-runtime`'s
/// `filter_envelope`: active if any of env/a/d/s/r is set; the sweep runs
/// between `2^-offset * f` and `2^(|env| - offset) * f`, swapped for negative
/// env. `params` is `[env, attack, decay, sustain, release]`.
fn filter_envelope(
    frequency: Option<f32>,
    fanchor: Option<f32>,
    params: [Option<f32>; 5],
) -> Option<FilterEnvelope> {
    let frequency = frequency?;
    let [env_amount, e_attack, e_decay, e_sustain, e_release] = params;
    if env_amount.is_none()
        && e_attack.is_none()
        && e_decay.is_none()
        && e_sustain.is_none()
        && e_release.is_none()
    {
        return None;
    }
    let (attack, decay, sustain, release) =
        if e_attack.is_none() && e_decay.is_none() && e_sustain.is_none() && e_release.is_none() {
            (0.005, 0.14, 0.0, 0.1)
        } else {
            let sustain = e_sustain.unwrap_or(
                if (e_attack.is_some() && e_decay.is_none())
                    || (e_attack.is_none() && e_decay.is_none())
                {
                    1.0
                } else {
                    0.001
                },
            );
            (
                e_attack.unwrap_or(0.0).max(0.001),
                e_decay.unwrap_or(0.0).max(0.001),
                sustain.min(1.0),
                e_release.unwrap_or(0.0).max(0.01),
            )
        };
    let env = env_amount.unwrap_or(1.0);
    let offset = env.abs() * fanchor.unwrap_or(0.0);
    let mut min = (2.0f32.powf(-offset) * frequency).clamp(0.0, 20_000.0);
    let mut max = (2.0f32.powf(env.abs() - offset) * frequency).clamp(0.0, 20_000.0);
    if env < 0.0 {
        std::mem::swap(&mut min, &mut max);
    }
    Some(FilterEnvelope {
        attack_secs: attack,
        decay_secs: decay,
        sustain: f64::from(sustain),
        release_secs: release,
        min_hz: f64::from(min),
        max_hz: f64::from(max),
    })
}

fn channel_metrics(pcm: &[f32], channel: usize) -> ChannelMetrics {
    let samples = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| f64::from(frame[channel]));
    let mut peak = 0.0f64;
    let mut sum_squares = 0.0f64;
    let mut count = 0usize;
    for sample in samples {
        peak = peak.max(sample.abs());
        sum_squares += sample * sample;
        count += 1;
    }
    ChannelMetrics {
        peak,
        rms: (sum_squares / count as f64).sqrt(),
    }
}

#[test]
fn native_scalar_is_measured_against_every_pinned_chrome_fixture() {
    let baseline: Baseline = serde_json::from_slice(
        &std::fs::read(repo_file(
            "crates/audio/tests/fixtures/chrome-reference.json",
        ))
        .expect("read baseline"),
    )
    .expect("parse baseline");
    assert!(
        baseline
            .scope
            .contains("not representative of the complete audio surface"),
        "the compact fixture must not claim complete audio coverage"
    );
    let case_ids: Vec<&str> = baseline.cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(
        case_ids,
        [
            "sine-explicit-adsr",
            "triangle-default-adsr",
            "square-short-envelope",
            "sine-gain-velocity-postgain-pan",
            "overlapping-sine-triangle",
            "sawtooth-static-lowpass",
            "square-static-highpass",
            "sawtooth-static-bandpass",
            "sawtooth-static-lowpass-24db",
            "square-lowpass-envelope",
            "bundled-bd-speed-slice-pan",
            "bundled-bd-note-release",
            "sine-decay-only-pluck",
            "triangle-attack-only",
            "square-highpass-envelope",
            "sawtooth-bandpass-envelope",
            "sawtooth-lowpass-negative-env-fanchor",
            "sawtooth-lowpass-24db-envelope",
            "sine-pan-extremes",
        ],
        "the Chrome reference matrix must not shrink or reorder silently"
    );

    let sample_rate = 48_000;
    let frames = 96_000;
    for case in &baseline.cases {
        assert_eq!(case.negative_control.comparison_status, "FAIL");
        assert!(case.negative_control.relative_rms_delta > 0.05);
        let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(&case.fixture);
        let fixture: Fixture = serde_json::from_slice(
            &std::fs::read(&fixture_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", fixture_path.display())),
        )
        .expect("parse fixture");
        assert_eq!(fixture.id, case.id);
        let events: Vec<OnsetEvent> = fixture
            .events
            .iter()
            .map(|event| native_event(event, sample_rate))
            .collect();
        let pcm = render_pcm(&mut ScalarBackend::new(), sample_rate, frames, &events)
            .expect("render the scalar side");
        let native = Metrics::from_interleaved_stereo(&pcm);
        let native_channels = [channel_metrics(&pcm, 0), channel_metrics(&pcm, 1)];
        let audible_frames: Vec<u64> = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.iter().any(|sample| sample.abs() >= 1e-7))
            .map(|(frame, _)| frame as u64)
            .collect();
        let native_first = *audible_frames.first().expect("scalar fixture is audible");
        let native_last = *audible_frames.last().expect("scalar fixture is audible");
        eprintln!(
            "{}: Chrome peak={:.9} rms={:.9} audible={}..={}; native peak={:.9} rms={:.9} audible={native_first}..={native_last}; deltas peak={:.6} rms={:.6}; channels L={:.9}/{:.9} R={:.9}/{:.9}",
            case.id,
            case.reference.peak,
            case.reference.rms,
            case.reference.first_audible_frame,
            case.reference.last_audible_frame,
            native.peak,
            native.rms,
            relative_delta(case.reference.peak, f64::from(native.peak)),
            relative_delta(case.reference.rms, f64::from(native.rms)),
            native_channels[0].peak,
            native_channels[0].rms,
            native_channels[1].peak,
            native_channels[1].rms,
        );
        assert_eq!(case.reference.channel_metrics.len(), 2);
        assert!(
            relative_delta(case.reference.peak, f64::from(native.peak)) <= 0.0001,
            "{} aggregate peak diverged from the Chrome reference",
            case.id
        );
        assert!(
            relative_delta(case.reference.rms, f64::from(native.rms)) <= 0.0001,
            "{} aggregate RMS diverged from the Chrome reference",
            case.id
        );
        assert_eq!(
            native_first, case.reference.first_audible_frame,
            "{} first audible frame diverged from the Chrome reference",
            case.id
        );
        assert_eq!(
            native_last, case.reference.last_audible_frame,
            "{} last audible frame diverged from the Chrome reference",
            case.id
        );
        for (channel, (reference, native)) in case
            .reference
            .channel_metrics
            .iter()
            .zip(&native_channels)
            .enumerate()
        {
            let peak_matches = if reference.peak < 1e-12 {
                native.peak < 1e-12
            } else {
                relative_delta(reference.peak, native.peak) <= 0.0001
            };
            let rms_matches = if reference.rms < 1e-12 {
                native.rms < 1e-12
            } else {
                relative_delta(reference.rms, native.rms) <= 0.0001
            };
            assert!(
                peak_matches && rms_matches,
                "{} channel {channel} diverged: Chrome peak/rms={:.9}/{:.9}, native={:.9}/{:.9}",
                case.id,
                reference.peak,
                reference.rms,
                native.peak,
                native.rms
            );
        }
    }
}
