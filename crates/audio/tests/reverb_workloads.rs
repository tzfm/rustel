//! Manual release benchmark for the complete convolution-reverb path.
//!
//! The cases enter through the public Rust audio boundary and include the
//! preplanned FFTs, frequency-domain accumulation, inverse transforms, and
//! stereo wet mix. Run with:
//!
//! ```text
//! cargo test --release -p rustel-audio --test reverb_workloads -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::time::Instant;

use rustel_audio::reverb::{OrbitReverb, REVERB_BLOCK, ReverbParams};

const SAMPLE_RATE: u32 = 48_000;
const VALIDATION_BLOCKS: usize = 64;

#[derive(Clone, Copy)]
struct Workload {
    id: &'static str,
    room_seconds: f32,
}

const WORKLOADS: [Workload; 3] = [
    Workload {
        id: "room-short",
        room_seconds: 0.25,
    },
    Workload {
        id: "room-default",
        room_seconds: 2.0,
    },
    Workload {
        id: "room-long",
        room_seconds: 6.0,
    },
];

fn reverb(workload: Workload) -> OrbitReverb {
    OrbitReverb::generate(
        SAMPLE_RATE,
        ReverbParams {
            size_secs: workload.room_seconds,
            fade_secs: 0.1,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0,
            ir: None,
        },
    )
}

fn input_blocks() -> ([f32; REVERB_BLOCK], [f32; REVERB_BLOCK]) {
    let mut left = [0.0; REVERB_BLOCK];
    let mut right = [0.0; REVERB_BLOCK];
    for frame in 0..REVERB_BLOCK {
        let phase = frame as f32 / REVERB_BLOCK as f32;
        left[frame] = (phase * std::f32::consts::TAU * 3.0).sin() * 0.2;
        right[frame] = (phase * std::f32::consts::TAU * 5.0).cos() * 0.15;
    }
    (left, right)
}

fn process(
    reverb: &mut OrbitReverb,
    input_left: &[f32; REVERB_BLOCK],
    input_right: &[f32; REVERB_BLOCK],
    output_left: &mut [f32; REVERB_BLOCK],
    output_right: &mut [f32; REVERB_BLOCK],
) {
    output_left.fill(0.0);
    output_right.fill(0.0);
    reverb.process_block(input_left, input_right, output_left, output_right);
}

fn validation_hash(
    reverb: &mut OrbitReverb,
    input_left: &[f32; REVERB_BLOCK],
    input_right: &[f32; REVERB_BLOCK],
    output_left: &mut [f32; REVERB_BLOCK],
    output_right: &mut [f32; REVERB_BLOCK],
) -> (u64, f32) {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut peak = 0.0f32;
    for _ in 0..VALIDATION_BLOCKS {
        process(reverb, input_left, input_right, output_left, output_right);
        for sample in output_left.iter().chain(output_right.iter()).copied() {
            assert!(
                sample.is_finite(),
                "reverb benchmark produced a non-finite sample"
            );
            peak = peak.max(sample.abs());
            for byte in sample.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    assert!(peak > 1e-6, "reverb benchmark produced silence");
    (hash, peak)
}

fn setting(name: &str, default: usize, minimum: usize, maximum: usize) -> usize {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let value = raw
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{name} must be a positive integer"));
    assert!(
        (minimum..=maximum).contains(&value),
        "{name} must be in {minimum}..={maximum}"
    );
    value
}

fn require_release_build() {
    #[cfg(debug_assertions)]
    panic!("reverb measurements must use cargo test --release");
}

#[test]
#[ignore = "manual release benchmark"]
fn convolution_reverb_workloads() {
    require_release_build();
    // Finish libtest's `test ...` status line before emitting JSONL records.
    println!();
    let repetitions = setting("RUSTEL_REVERB_BENCH_REPETITIONS", 5, 1, 100);
    let warmup_blocks = setting("RUSTEL_REVERB_BENCH_WARMUP_BLOCKS", 128, 1, 100_000);
    // Shorter windows can miss the coarse tail's periodic FFT hop and report
    // only the fixed head cost instead of the complete reverb path.
    let measured_blocks = setting("RUSTEL_REVERB_BENCH_BLOCKS", 512, 128, 1_000_000);
    let selected = std::env::var("RUSTEL_REVERB_BENCH_CASE").ok();
    let (input_left, input_right) = input_blocks();
    let mut matched = false;

    for workload in WORKLOADS {
        if selected.as_deref().is_some_and(|id| id != workload.id) {
            continue;
        }
        matched = true;
        let mut expected_hash = None;
        for repetition in 1..=repetitions {
            let mut reverb = reverb(workload);
            let planned_bytes = reverb.approx_bytes();
            assert!(
                planned_bytes > 0,
                "{} planned no convolution data",
                workload.id
            );
            let mut output_left = [0.0; REVERB_BLOCK];
            let mut output_right = [0.0; REVERB_BLOCK];
            let (hash, peak) = validation_hash(
                &mut reverb,
                &input_left,
                &input_right,
                &mut output_left,
                &mut output_right,
            );
            assert_eq!(
                *expected_hash.get_or_insert(hash),
                hash,
                "{} validation output changed between repetitions",
                workload.id
            );

            for _ in 0..warmup_blocks {
                process(
                    &mut reverb,
                    &input_left,
                    &input_right,
                    &mut output_left,
                    &mut output_right,
                );
                black_box((&output_left, &output_right));
            }
            let started = Instant::now();
            for _ in 0..measured_blocks {
                process(
                    &mut reverb,
                    &input_left,
                    &input_right,
                    &mut output_left,
                    &mut output_right,
                );
                black_box((&output_left, &output_right));
            }
            let elapsed_nanos = started.elapsed().as_nanos();
            assert!(
                output_left
                    .iter()
                    .chain(&output_right)
                    .all(|sample| sample.is_finite())
            );
            assert!(
                output_left
                    .iter()
                    .chain(&output_right)
                    .any(|sample| sample.abs() > 1e-6)
            );

            let stereo_frames = measured_blocks * REVERB_BLOCK;
            let nanos_per_stereo_frame = elapsed_nanos as f64 / stereo_frames as f64;
            println!(
                "{{\"schema_version\":1,\"benchmark\":\"convolution-reverb\",\"workload\":\"{}\",\"repetition\":{},\"room_seconds\":{:.3},\"planned_bytes\":{},\"block_frames\":{},\"warmup_blocks\":{},\"measured_blocks\":{},\"stereo_frames\":{},\"elapsed_nanos\":{},\"nanos_per_stereo_frame\":{:.6},\"validation_hash\":\"{:016x}\",\"validation_peak\":{:.9}}}",
                workload.id,
                repetition,
                workload.room_seconds,
                planned_bytes,
                REVERB_BLOCK,
                warmup_blocks,
                measured_blocks,
                stereo_frames,
                elapsed_nanos,
                nanos_per_stereo_frame,
                hash,
                peak,
            );
        }
    }

    assert!(
        matched,
        "RUSTEL_REVERB_BENCH_CASE did not name a benchmark workload"
    );
}
