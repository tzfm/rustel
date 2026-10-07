//! A finite, prefilled callback window using a Session worker and host-owned
//! DSP. No device is opened. A desktop host keeps the worker alive and schedules
//! ahead of its device's sample clock while the callback consumes the ring.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64},
};

use rustel_engine::{
    QUERY_WORKER_STACK_BYTES, Session, SessionConfig,
    audio::{LiveFlipAtomics, LiveScalarBackend, Ring},
};

fn main() -> Result<(), String> {
    let sample_rate = 48_000;
    let window_secs = 0.1;
    let ring = Arc::new(Ring::new(64));
    let producer_ring = Arc::clone(&ring);
    let producer = std::thread::Builder::new()
        .name("rustel-session".into())
        .stack_size(QUERY_WORKER_STACK_BYTES)
        .spawn(move || -> Result<u64, String> {
            // Session and JavaScript remain on this worker. Only owned audio
            // events cross to the callback; the worker is the ring's sole producer.
            let mut session = Session::with_config(
                SessionConfig::default()
                    .with_sample_rate(sample_rate)
                    .with_horizon(window_secs),
            )
            .map_err(|error| error.to_string())?;
            session
                .evaluate(r#"note("c4 e4 g4 c5").s("sine").gain(0.2)"#)
                .map_err(|error| error.to_string())?;
            for event in session
                .schedule_audio_through(0.0, window_secs, sample_rate)
                .map_err(|error| error.to_string())?
            {
                if !producer_ring.push(event) {
                    return Err("audio event ring is full".into());
                }
            }
            Ok(session.generation())
        })
        .map_err(|error| error.to_string())?;
    let generation = AtomicU64::new(producer.join().map_err(|_| "session worker panicked")??);

    // Allocate and prepare before handing the backend to the native callback.
    let mut backend = LiveScalarBackend::new(sample_rate, 16)?;
    let takeover_frame = AtomicU64::new(0);
    let takeover_cut = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let flip = LiveFlipAtomics {
        generation: &generation,
        takeover_frame: &takeover_frame,
        takeover_cut: &takeover_cut,
        line_arm: &line_arm,
    };
    let mut output = [0.0; 128 * 2];
    let total_frames = (window_secs * f64::from(sample_rate)) as u64;
    let mut frame = 0;
    let mut peak = 0.0_f32;
    while frame < total_frames {
        let frames = (total_frames - frame).min(128) as usize;
        backend.process_block_with(
            &mut output[..frames * 2],
            frames,
            frame,
            &ring,
            flip,
            &stopped,
        );
        peak = output[..frames * 2]
            .iter()
            .fold(peak, |peak, sample| peak.max(sample.abs()));
        frame += frames as u64;
    }
    assert!(peak > 0.001, "the callback should render audible audio");
    println!("Host callback rendered {total_frames} stereo frames (peak {peak:.3})");
    Ok(())
}
