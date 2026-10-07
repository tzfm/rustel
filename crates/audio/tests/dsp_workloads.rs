//! Manual release benchmark for whole-voice DSP paths.
//!
//! The cases use only the public Rust audio boundary. They deliberately avoid
//! score syntax so the benchmark describes reusable engine work rather than a
//! particular composition. Run with:
//!
//! ```text
//! cargo test --release -p rustel-audio --test dsp_workloads -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::time::Instant;

use rustel_audio::{
    AudioBackend, DecodedSample, Envelope, FilterControls, FmControls, FmOperator, FmRoute, FmWave,
    LfoMod, MAX_FM_OPERATORS, MAX_FM_ROUTES, ModTarget, OnsetEvent, OscillatorControls, SampleId,
    ScalarBackend, StaticBiquad, SynthSource, VowelControls, Waveform, WavetableControls,
};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 128;
const VALIDATION_BLOCKS: usize = 8;
// Longer than the largest accepted warm-up + measurement window, so even a
// deliberately long run measures active voices rather than their silent tail.
const NOTE_SECONDS: f32 = 4_096.0;
const WAVETABLE_ID: SampleId = SampleId(37);
const WAVETABLE_FRAME_LEN: usize = 2_048;
const WAVETABLE_FRAMES: usize = 4;

#[derive(Clone, Copy)]
enum VoiceKind {
    DryMono,
    FilteredMono,
    FmMono,
    FmScaling {
        operators: usize,
        dense_matrix: bool,
        modulated: bool,
    },
    SupersawSingleLane,
    SupersawStereo,
    SupersawMaxUnison,
    SupersawModulated,
    WavetableSingleLane,
    WavetableStereo,
    WavetableMaxUnison,
    WavetableSpreadModulated,
    FilteredStereo,
    VowelMono,
}

#[derive(Clone, Copy)]
struct Workload {
    id: &'static str,
    voices: usize,
    kind: VoiceKind,
}

const WORKLOADS: [Workload; 17] = [
    Workload {
        id: "dry-mono-128",
        voices: 128,
        kind: VoiceKind::DryMono,
    },
    Workload {
        id: "filtered-mono-64",
        voices: 64,
        kind: VoiceKind::FilteredMono,
    },
    Workload {
        id: "fm-mono-32",
        voices: 32,
        kind: VoiceKind::FmMono,
    },
    Workload {
        id: "fm-four-operators-16",
        voices: 16,
        kind: VoiceKind::FmScaling {
            operators: 4,
            dense_matrix: false,
            modulated: false,
        },
    },
    Workload {
        id: "fm-eight-operators-8",
        voices: 8,
        kind: VoiceKind::FmScaling {
            operators: 8,
            dense_matrix: false,
            modulated: false,
        },
    },
    Workload {
        id: "fm-dense-matrix-8",
        voices: 8,
        kind: VoiceKind::FmScaling {
            operators: 8,
            dense_matrix: true,
            modulated: false,
        },
    },
    Workload {
        id: "fm-modulated-matrix-8",
        voices: 8,
        kind: VoiceKind::FmScaling {
            operators: 8,
            dense_matrix: true,
            modulated: true,
        },
    },
    Workload {
        id: "supersaw-single-lane-64",
        voices: 64,
        kind: VoiceKind::SupersawSingleLane,
    },
    Workload {
        id: "supersaw-stereo-32",
        voices: 32,
        kind: VoiceKind::SupersawStereo,
    },
    Workload {
        id: "supersaw-max-unison-16",
        voices: 16,
        kind: VoiceKind::SupersawMaxUnison,
    },
    Workload {
        id: "supersaw-modulated-32",
        voices: 32,
        kind: VoiceKind::SupersawModulated,
    },
    Workload {
        id: "wavetable-single-lane-64",
        voices: 64,
        kind: VoiceKind::WavetableSingleLane,
    },
    Workload {
        id: "wavetable-stereo-16",
        voices: 16,
        kind: VoiceKind::WavetableStereo,
    },
    Workload {
        id: "wavetable-max-unison-8",
        voices: 8,
        kind: VoiceKind::WavetableMaxUnison,
    },
    Workload {
        id: "wavetable-spread-modulated-16",
        voices: 16,
        kind: VoiceKind::WavetableSpreadModulated,
    },
    Workload {
        id: "filtered-stereo-32",
        voices: 32,
        kind: VoiceKind::FilteredStereo,
    },
    Workload {
        id: "vowel-mono-32",
        voices: 32,
        kind: VoiceKind::VowelMono,
    },
];

fn benchmark_envelope() -> Envelope {
    Envelope {
        attack_secs: 0.001,
        decay_secs: 0.01,
        sustain: 0.8,
        release_secs: 0.01,
    }
}

fn fm_controls() -> FmControls {
    let mut operators = [None; MAX_FM_OPERATORS];
    operators[0] = Some(FmOperator {
        harmonicity: 2.0,
        waveform: FmWave::Sine,
        env: None,
        env_exponential: true,
    });
    let mut routes = [None; MAX_FM_ROUTES];
    routes[0] = Some(FmRoute {
        source: 1,
        target: 0,
        amount: 1.5,
        mod_slot: Some(0),
    });
    FmControls { operators, routes }
}

fn fm_scaling_controls(operator_count: usize, dense_matrix: bool) -> FmControls {
    assert!((1..=MAX_FM_OPERATORS).contains(&operator_count));
    let mut operators = [None; MAX_FM_OPERATORS];
    let harmonicities = [1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 5.0];
    let waveforms = [
        FmWave::Sine,
        FmWave::Triangle,
        FmWave::Square,
        FmWave::Sawtooth,
    ];
    for slot in 0..operator_count {
        operators[slot] = Some(FmOperator {
            harmonicity: harmonicities[slot],
            waveform: waveforms[slot % waveforms.len()],
            env: (slot % 2 == 1).then_some(Envelope {
                attack_secs: 0.002,
                decay_secs: 0.03,
                sustain: 0.65,
                release_secs: 0.02,
            }),
            env_exponential: slot % 3 != 0,
        });
    }

    let mut routes = [None; MAX_FM_ROUTES];
    for (slot, route) in routes.iter_mut().enumerate().take(operator_count) {
        *route = Some(FmRoute {
            source: (slot + 1) as u8,
            target: slot as u8,
            amount: 0.35 + slot as f32 * 0.1,
            mod_slot: Some(slot as u8),
        });
    }
    if dense_matrix {
        for (slot, route) in routes[operator_count..]
            .iter_mut()
            .enumerate()
            .take(operator_count)
        {
            *route = Some(FmRoute {
                source: (slot + 1) as u8,
                target: ((slot + 2) % (operator_count + 1)) as u8,
                amount: 0.15 + slot as f32 * 0.025,
                mod_slot: None,
            });
        }
    }
    FmControls { operators, routes }
}

fn benchmark_lfo(target: ModTarget, frequency_hz: f32, depth: f32, shape: u8) -> LfoMod {
    LfoMod {
        fxi: None,
        target,
        frequency_hz,
        phase0: 0.25,
        depth,
        dcoffset: 0.0,
        skew: 0.5,
        curve: 1.0,
        shape,
        min: 0.0,
        max: depth,
        param_base: 1.0,
        filter: None,
        id: None,
    }
}

fn wavetable_controls(voices: f32) -> WavetableControls {
    WavetableControls {
        table: WAVETABLE_ID,
        frame_len: WAVETABLE_FRAME_LEN as u32,
        voices,
        lfo_shape: 0,
        phaserand: 1.0,
        freqspread: 0.35,
        panspread: 0.8,
        position: 0.43,
        pos_env_amount: 0.0,
        pos_attack: 0.0,
        pos_decay: 0.5,
        pos_sustain: 0.0,
        pos_release: 0.1,
        lfo_depth: 0.0,
        lfo_rate: 1.0,
        lfo_skew: 0.5,
        lfo_dc: 0.0,
        warp: 0.0,
        warp_mode: 0,
        warp_env_amount: 0.0,
        warp_attack: 0.0,
        warp_decay: 0.5,
        warp_sustain: 0.0,
        warp_release: 0.1,
        warp_lfo_depth: 0.0,
        warp_lfo_rate: 1.0,
        warp_lfo_skew: 0.5,
        warp_lfo_dc: 0.0,
        warp_lfo_shape: 0,
    }
}

fn wavetable_sample() -> DecodedSample {
    let mut pcm = Vec::with_capacity(WAVETABLE_FRAME_LEN * WAVETABLE_FRAMES);
    for frame in 0..WAVETABLE_FRAMES {
        for index in 0..WAVETABLE_FRAME_LEN {
            let phase = index as f32 / WAVETABLE_FRAME_LEN as f32;
            let sample = match frame {
                0 => phase * 2.0 - 1.0,
                1 => 1.0 - 4.0 * (phase - 0.5).abs(),
                2 => (phase * std::f32::consts::TAU).sin(),
                _ => {
                    if phase < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
            };
            pcm.push(sample);
        }
    }
    DecodedSample::from_parts(SAMPLE_RATE, 1, pcm).expect("synthetic wavetable")
}

fn is_wavetable(kind: VoiceKind) -> bool {
    matches!(
        kind,
        VoiceKind::WavetableSingleLane
            | VoiceKind::WavetableStereo
            | VoiceKind::WavetableMaxUnison
            | VoiceKind::WavetableSpreadModulated
    )
}

fn event(kind: VoiceKind, voice: usize) -> OnsetEvent {
    let frequency = 110.0 * 2.0f32.powf((voice % 36) as f32 / 12.0);
    let mut controls = OscillatorControls {
        limit: None,
        waveform: Waveform::Sawtooth,
        envelope: benchmark_envelope(),
        orbit: (voice % 4) as u8,
        ..OscillatorControls::default()
    };
    let mut onset = OnsetEvent::new(0, frequency, 0.15, NOTE_SECONDS);

    match kind {
        VoiceKind::DryMono => {}
        VoiceKind::FilteredMono => {
            controls.filters = FilterControls {
                lowpass: Some(StaticBiquad {
                    frequency_hz: 2_400.0,
                    q: 1.0,
                }),
                ..FilterControls::default()
            };
        }
        VoiceKind::FmMono => controls.fm = Some(fm_controls()),
        VoiceKind::FmScaling {
            operators,
            dense_matrix,
            modulated,
        } => {
            controls.fm = Some(fm_scaling_controls(operators, dense_matrix));
            if modulated {
                controls.lfos[0] = Some(benchmark_lfo(ModTarget::FmIndex(0), 3.0, 0.2, 1));
                controls.lfos[1] = Some(benchmark_lfo(ModTarget::FmFreq(1), 2.0, 0.15, 1));
            }
        }
        VoiceKind::SupersawSingleLane => {
            onset.synth = Some(SynthSource::Supersaw {
                voices: 1.0,
                freqspread: 0.0,
                panspread: 0.0,
            });
        }
        VoiceKind::SupersawStereo => {
            onset.synth = Some(SynthSource::Supersaw {
                voices: 8.0,
                freqspread: 0.35,
                panspread: 0.8,
            });
        }
        VoiceKind::SupersawMaxUnison => {
            onset.synth = Some(SynthSource::Supersaw {
                voices: 32.0,
                freqspread: 1.0,
                panspread: 0.8,
            });
        }
        VoiceKind::SupersawModulated => {
            onset.synth = Some(SynthSource::Supersaw {
                voices: 8.0,
                freqspread: 0.35,
                panspread: 0.8,
            });
            controls.lfos[0] = Some(benchmark_lfo(ModTarget::SourceFreqspread, 0.0, 0.2, 4));
        }
        VoiceKind::WavetableSingleLane => {
            onset.wavetable = Some(wavetable_controls(1.0));
        }
        VoiceKind::WavetableStereo => {
            onset.wavetable = Some(wavetable_controls(8.0));
        }
        VoiceKind::WavetableMaxUnison => {
            onset.wavetable = Some(wavetable_controls(32.0));
        }
        VoiceKind::WavetableSpreadModulated => {
            onset.wavetable = Some(wavetable_controls(8.0));
            controls.lfos[0] = Some(benchmark_lfo(ModTarget::SourceFreqspread, 3.0, 0.2, 1));
            controls.lfos[1] = Some(benchmark_lfo(ModTarget::SourcePanspread, 2.0, 0.15, 1));
        }
        VoiceKind::FilteredStereo => {
            controls.filters = FilterControls {
                lowpass: Some(StaticBiquad {
                    frequency_hz: 2_400.0,
                    q: 1.0,
                }),
                ..FilterControls::default()
            };
            onset.synth = Some(SynthSource::Supersaw {
                voices: 4.0,
                freqspread: 0.25,
                panspread: 0.7,
            });
        }
        VoiceKind::VowelMono => {
            controls.vowel = Some(VowelControls {
                freqs: [800.0, 1_150.0, 2_900.0, 3_900.0, 4_950.0],
                gains: [1.0, 0.5, 0.25, 0.125, 0.063],
                qs: [8.0, 10.0, 12.0, 12.0, 14.0],
            });
        }
    }

    onset.with_controls(controls)
}

fn backend(workload: Workload) -> ScalarBackend {
    let mut backend = ScalarBackend::prepared(SAMPLE_RATE, workload.voices)
        .unwrap_or_else(|error| panic!("{} preparation failed: {error}", workload.id));
    if is_wavetable(workload.kind) {
        backend
            .install_sample(WAVETABLE_ID, Box::new(wavetable_sample()))
            .expect("wavetable installation must fit the fixed sample bank");
    }
    for voice in 0..workload.voices {
        assert!(
            backend.try_note_prepared(event(workload.kind, voice)),
            "{} refused voice {voice}",
            workload.id
        );
    }
    backend
}

fn validation_hash(backend: &mut ScalarBackend, block: &mut [f32]) -> (u64, f32) {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut peak = 0.0f32;
    for _ in 0..VALIDATION_BLOCKS {
        backend.process_block(block, BLOCK_FRAMES);
        for sample in block.iter().copied() {
            assert!(
                sample.is_finite(),
                "DSP benchmark produced a non-finite sample"
            );
            peak = peak.max(sample.abs());
            for byte in sample.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    assert!(peak > 1e-6, "DSP benchmark produced silence");
    (hash, peak)
}

fn setting(name: &str, default: usize, maximum: usize) -> usize {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let value = raw
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{name} must be a positive integer"));
    assert!(
        (1..=maximum).contains(&value),
        "{name} must be in 1..={maximum}"
    );
    value
}

fn require_release_build() {
    #[cfg(debug_assertions)]
    panic!("DSP measurements must use cargo test --release");
}

#[test]
#[ignore = "manual release benchmark"]
fn whole_voice_dsp_workloads() {
    require_release_build();
    let repetitions = setting("RUSTEL_DSP_BENCH_REPETITIONS", 5, 100);
    let warmup_blocks = setting("RUSTEL_DSP_BENCH_WARMUP_BLOCKS", 64, 100_000);
    let measured_blocks = setting("RUSTEL_DSP_BENCH_BLOCKS", 512, 1_000_000);
    let selected = std::env::var("RUSTEL_DSP_BENCH_CASE").ok();
    let mut matched = false;

    for workload in WORKLOADS {
        if selected.as_deref().is_some_and(|id| id != workload.id) {
            continue;
        }
        matched = true;
        let mut expected_hash = None;
        for repetition in 1..=repetitions {
            let mut backend = backend(workload);
            let mut block = [0.0f32; BLOCK_FRAMES * 2];
            let (hash, peak) = validation_hash(&mut backend, &mut block);
            assert_eq!(
                *expected_hash.get_or_insert(hash),
                hash,
                "{} validation output changed between repetitions",
                workload.id
            );

            for _ in 0..warmup_blocks {
                backend.process_block(&mut block, BLOCK_FRAMES);
                black_box(&block);
            }
            let started = Instant::now();
            for _ in 0..measured_blocks {
                backend.process_block(&mut block, BLOCK_FRAMES);
                black_box(&block);
            }
            let elapsed_nanos = started.elapsed().as_nanos();
            assert!(block.iter().all(|sample| sample.is_finite()));
            assert!(block.iter().any(|sample| sample.abs() > 1e-6));

            let voice_frames = measured_blocks * BLOCK_FRAMES * workload.voices;
            let nanos_per_voice_frame = elapsed_nanos as f64 / voice_frames as f64;
            println!(
                "{{\"schema_version\":1,\"benchmark\":\"whole-voice-dsp\",\"workload\":\"{}\",\"repetition\":{},\"voices\":{},\"block_frames\":{},\"warmup_blocks\":{},\"measured_blocks\":{},\"voice_frames\":{},\"elapsed_nanos\":{},\"nanos_per_voice_frame\":{:.6},\"validation_hash\":\"{:016x}\",\"validation_peak\":{:.9}}}",
                workload.id,
                repetition,
                workload.voices,
                BLOCK_FRAMES,
                warmup_blocks,
                measured_blocks,
                voice_frames,
                elapsed_nanos,
                nanos_per_voice_frame,
                hash,
                peak,
            );
        }
    }

    assert!(
        matched,
        "RUSTEL_DSP_BENCH_CASE did not name a benchmark workload"
    );
}
