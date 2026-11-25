//! Check piano mode, chord display, octaves and shortcuts through the TUI.
//! Engine and live-engine tests cover voices; these check keyboard ownership.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic};

/// The footer row the piano indicator owns, while it owns it.
fn piano_row(studio: &mut Hermetic) -> Option<String> {
    studio.rows().into_iter().find(|row| row.contains("PIANO"))
}

/// A stop is a graceful tail the engine finishes on its own clock -
/// `settle()` does not wait for it, so this does, bounded the way
/// `transport.rs` waits one.
fn wait_for_stop(studio: &mut Hermetic) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while studio.is_playing() || studio.is_stopping() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stop never landed"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        studio.pump();
    }
}

/// The transport outranks the piano the same way it outranks every
/// panel: Ctrl+S and Ctrl+G still play and stop the score while the
/// letters belong to the instrument.
#[test]
fn the_transport_still_answers_under_the_piano() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "the score plays from under the piano");
    assert!(
        piano_row(&mut studio).is_some(),
        "and the piano stayed open through it"
    );

    studio.chord("ctrl+g");
    wait_for_stop(&mut studio);
    assert!(!studio.is_playing(), "and stops from under it too");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
}

/// Ctrl+Q under the piano closes the instrument first and then asks the
/// usual twice - a quit that left a note sounding would outlive the
/// studio itself.
#[test]
fn quit_from_the_piano_closes_it_and_asks_twice() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(piano_row(&mut studio).is_some(), "setup: the piano is open");

    studio.chord("ctrl+q");
    studio.settle();

    assert!(
        piano_row(&mut studio).is_none(),
        "the quit closed the piano:\n{}",
        studio.rows().join("\n")
    );
    assert!(
        studio.status().contains("again to quit"),
        "and asked, rather than going: {}",
        studio.status()
    );
    assert!(
        !studio.wants_quit(),
        "one press under the piano is the same single ask as anywhere else"
    );
}
