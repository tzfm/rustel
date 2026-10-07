//! The ring's producer and consumer roles pass to new threads only through
//! an explicit release, as a host with a restarted scheduling worker or a
//! reopened device callback needs.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rustel_audio::{AudioEvent, OscillatorControls, Ring};

fn event(onset_id: u64) -> AudioEvent {
    AudioEvent {
        onset_id,
        generation: 1,
        target_frame: onset_id * 10,
        onset_lead: 0.0,
        freq_hz: 440.0,
        gain: 0.5,
        duration_secs: 0.1,
        ui_visuals: 0,
        controls: OscillatorControls::default(),
        sample: None,
        wavetable: None,
        synth: None,
        cut: None,
    }
}

/// Run `work` on a thread of its own and wait for it to finish.
fn on_new_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(work).join().expect("ring thread")
}

#[test]
fn released_roles_pass_to_a_new_worker_and_a_new_callback() {
    let ring = Arc::new(Ring::new(8));
    let worker = Arc::clone(&ring);
    assert!(on_new_thread(move || worker.push(event(1))));
    let callback = Arc::clone(&ring);
    assert_eq!(
        on_new_thread(move || callback.pop().map(|event| event.onset_id)),
        Some(1)
    );

    // SAFETY: both threads were joined, so neither uses the ring again.
    unsafe {
        ring.release_producer();
        ring.release_consumer();
    }
    let worker = Arc::clone(&ring);
    assert!(on_new_thread(move || worker.push(event(2))));
    let callback = Arc::clone(&ring);
    assert_eq!(
        on_new_thread(move || callback.pop().map(|event| event.onset_id)),
        Some(2)
    );
    assert_eq!(ring.role_conflicts.load(Ordering::Relaxed), 0);
}
