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

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::hermetic;

/// The first ^Q arms and says what the second will do; only the second
/// press actually wants the studio gone.
#[test]
fn ctrl_q_arms_then_quits() {
    let mut studio = hermetic();

    studio.chord("ctrl+q");
    assert!(
        studio.status().contains("again to quit"),
        "the first press names the second: {}",
        studio.status()
    );
    assert!(!studio.wants_quit(), "arming is not quitting");

    studio.chord("ctrl+q");
    assert!(
        studio.wants_quit(),
        "the second press is the one that goes: {}",
        studio.status()
    );
}

/// Any other key calls the arm off - and the next ^Q starts the ask over
/// rather than being received as the confirmation.
#[test]
fn any_other_key_calls_the_arm_off() {
    let mut studio = hermetic();

    studio.chord("ctrl+q");
    assert!(!studio.wants_quit());

    studio.press(KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(studio.status(), "quit cancelled");
    assert!(!studio.wants_quit(), "the arm did not survive the key");

    // A fresh ^Q asks again rather than quitting on what it thinks is the
    // second press of the cancelled gesture.
    studio.chord("ctrl+q");
    assert!(
        studio.status().contains("again to quit"),
        "the ask starts over: {}",
        studio.status()
    );
    assert!(!studio.wants_quit());

    studio.chord("ctrl+q");
    assert!(studio.wants_quit());
}

/// An Alt-modified key cancels too: a finger rolling off a chord is exactly
/// the stray press the arming exists to catch.
#[test]
fn an_alt_key_calls_the_arm_off() {
    let mut studio = hermetic();

    studio.chord("ctrl+q");
    studio.press(KeyCode::Char('z'), KeyModifiers::ALT);
    assert_eq!(studio.status(), "quit cancelled");
    assert!(!studio.wants_quit());
}

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

/// The chord is global: it arms and quits over an open sheet, so no panel
/// or prompt can make the studio impossible to quit from the keyboard.
#[test]
fn ctrl_q_works_over_an_open_panel() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    assert!(
        !studio.status().contains("again to quit"),
        "setup: the settings sheet is what the last press opened"
    );

    studio.chord("ctrl+q");
    assert!(
        studio.status().contains("again to quit"),
        "the chord armed over the sheet: {}",
        studio.status()
    );
    studio.chord("ctrl+q");
    assert!(studio.wants_quit(), "and went over it too");
}
