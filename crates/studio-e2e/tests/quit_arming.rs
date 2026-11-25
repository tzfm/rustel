//! Check Ctrl-Q arming, confirmation, cancellation by another key and access
//! over panels. menu_bar covers immediate menu quit; piano_mode covers closing
//! the piano before quitting.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use rustel_studio_e2e::hermetic;

/// The arm is not a timer: a pause after the first ^Q keeps it armed.
///
/// The studio reads the wall clock and the harness cannot advance it, so
/// the test waits three real seconds with the loop turning. That is longer
/// than the one-to-three-second window of a typical confirm timer.
#[test]
fn the_arm_does_not_expire() {
    const HESITATION: std::time::Duration = std::time::Duration::from_secs(3);
    const TURN: std::time::Duration = std::time::Duration::from_millis(50);
    let mut studio = hermetic();

    studio.chord("ctrl+q");
    assert!(
        studio.status().contains("again to quit"),
        "armed: {}",
        studio.status()
    );
    let started = std::time::Instant::now();
    while started.elapsed() < HESITATION {
        studio.pump();
        std::thread::sleep(TURN);
    }
    studio.settle();
    assert!(!studio.wants_quit(), "waiting is not quitting");

    // The arm is only observable by what the next press does: a lapsed arm
    // would take this ^Q as a fresh first press and ask again.
    studio.chord("ctrl+q");
    assert!(
        studio.wants_quit(),
        "the waited-on second press still goes: {}",
        studio.status()
    );
}
