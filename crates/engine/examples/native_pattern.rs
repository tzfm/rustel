//! An offline host using only native patterns and DSP, without JavaScript or
//! a device backend. Run with `cargo run -p rustel-engine --example native_pattern`.

use std::sync::Arc;

use rustel_engine::{
    audio::{ScalarBackend, render_pcm},
    core::controls::ControlSpec,
    mini,
    scheduler::{Clock, Scheduler, TickStatus, Transport, VirtualClock},
    voice,
};

fn main() -> Result<(), String> {
    let sample_rate = 48_000;
    let cps = 1.0;
    let duration_secs = 1.0;
    let transport = Arc::new(Transport::default());
    let clock = VirtualClock::new(0.0);
    let mut scheduler = Scheduler::new(transport, cps, duration_secs);
    let pattern = mini::mini("c4 e4 g4 c5").map_err(|error| error.to_string())?;
    let pattern = ControlSpec::new(["note"]).pattern(&pattern);
    scheduler.set_pattern(pattern, clock.now());
    if scheduler.tick(&clock) != TickStatus::Filled {
        return Err("the scheduler could not fill its first window".into());
    }

    let mut onsets = Vec::new();
    for event in scheduler.drain_through(&clock, duration_secs) {
        onsets.push(
            voice::resolve_hap_value(
                &event.value,
                event.onset_id,
                event.duration.to_f64() / cps,
                event.target_time,
                sample_rate,
                cps,
                &voice::BundledOnly,
            )
            .map_err(|error| error.to_string())?
            .with_generation(event.generation),
        );
    }

    let frames = (duration_secs * f64::from(sample_rate)) as usize;
    let pcm = render_pcm(&mut ScalarBackend::new(), sample_rate, frames, &onsets)?;
    let peak = pcm
        .iter()
        .fold(0.0_f32, |peak, value| peak.max(value.abs()));
    println!(
        "Rendered {} notes into {} stereo frames at {sample_rate} Hz (peak {peak:.3})",
        onsets.len(),
        pcm.len() / 2
    );
    Ok(())
}
