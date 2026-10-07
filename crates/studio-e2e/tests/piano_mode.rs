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

/// F12 opens the piano - the screen keeps its shape and focus, only the
/// letters change meaning - and Esc hands the whole keyboard back.
#[test]
fn f12_opens_the_piano_and_esc_hands_the_keyboard_back() {
    let mut studio = hermetic();
    let before = studio.score();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_some(),
        "the footer reads the piano while the mode is open:\n{}",
        studio.rows().join("\n")
    );
    assert_eq!(
        studio.focus(),
        None,
        "opening the piano parks focus on no panel - the screen stays as it was"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_none(),
        "Esc took the indicator down"
    );
    assert_eq!(studio.score(), before, "and the score never moved");
}

/// F12 leaves the last chord readable; opening and closing again clears it.
#[test]
fn f12_again_closes_the_piano_and_double_toggle_clears_the_scratchpad() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(piano_row(&mut studio).is_some(), "setup: the piano is open");
    for character in ['a', 'd', 'g'] {
        studio.press(KeyCode::Char(character), KeyModifiers::NONE);
    }

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_none(),
        "the second F12 closed it:\n{}",
        studio.rows().join("\n")
    );
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("Notes  C4 + E4 + G4")),
        "leaving retains the last chord"
    );

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(piano_row(&mut studio).is_some(), "the piano reopened");
    assert!(
        !studio.rows().iter().any(|row| row.contains("E4 + G4")),
        "reopening immediately clears the old chord"
    );
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(piano_row(&mut studio).is_none());
    assert!(
        !studio.rows().iter().any(|row| row.contains("Notes  ")),
        "leaving without playing keeps the scratchpad empty"
    );
}

/// The indicator names what the fingers hold, in semitone order, joined
/// with `+` - and after a terminal without key-up events lets the timed
/// notes expire, the last chord stays readable.
#[test]
fn the_indicator_names_the_chord_under_the_fingers() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    for character in ['a', 'd', 'g'] {
        studio.press(KeyCode::Char(character), KeyModifiers::NONE);
    }
    studio.settle();

    let row = piano_row(&mut studio).expect("the piano indicator is up");
    assert!(
        row.contains("C4 + E4 + G4"),
        "the chord reads in semitone order with its note names: {row}"
    );

    // The legacy terminal sends no key-up: the notes are timed, and the
    // indicator keeps the most recent chord readable after they end.
    std::thread::sleep(std::time::Duration::from_millis(800));
    studio.pump();
    let row = piano_row(&mut studio).expect("the indicator is still up");
    assert!(
        row.contains("C4 + E4 + G4"),
        "the lifted chord stays on the footer: {row}"
    );
}

/// `x` and `z` show the new octave until a played note replaces it.
#[test]
fn the_octave_keys_move_the_keyboard() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_some_and(|row| !row.contains("C4") && !row.contains("Octave")),
        "an untouched keyboard has no note or octave label: {:?}",
        piano_row(&mut studio)
    );

    studio.press(KeyCode::Char('x'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_some_and(|row| row.contains("Octave 5") && !row.contains("C5")),
        "changing the octave shows its number: {:?}",
        piano_row(&mut studio)
    );

    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_some_and(|row| row.contains("E5") && !row.contains("Octave")),
        "the next played note replaces the octave label: {:?}",
        piano_row(&mut studio)
    );

    // The octave label stays even after the previous note expires.
    studio.press(KeyCode::Char('z'), KeyModifiers::NONE);
    std::thread::sleep(std::time::Duration::from_millis(800));
    studio.pump();
    assert!(
        piano_row(&mut studio).is_some_and(|row| row.contains("Octave 4") && !row.contains("E5")),
        "the new octave stays until another note is played: {:?}",
        piano_row(&mut studio)
    );

    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        piano_row(&mut studio).is_some_and(|row| row.contains("E4") && !row.contains("Octave")),
        "z brought the base back down - the same key now reads E4: {:?}",
        piano_row(&mut studio)
    );
}

/// While the piano holds the letters, nothing they type reaches the
/// score - and a paste is swallowed whole rather than editing text.
#[test]
fn note_keys_and_pastes_never_reach_the_score() {
    let mut studio = hermetic();
    let before = studio.score();

    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.type_text("asdfghjkl");
    studio.paste("don't edit");
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    assert_eq!(
        studio.score(),
        before,
        "the jam session left no text behind"
    );
    assert!(
        !studio.is_dirty(),
        "and the scene is not dirty: nothing was edited"
    );
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
