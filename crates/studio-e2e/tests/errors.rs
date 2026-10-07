//! Check checker and engine errors, newest-message display, and clearing an
//! owner's error after a successful evaluation.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{hermetic, row_containing};

/// A score the checker refuses never reaches the engine, and the status
/// says why and what still plays.
#[test]
fn a_refused_score_never_reaches_the_engine() {
    let mut studio = hermetic();

    // First a clean score, so there is something playing to protect.
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(studio.errors(), None);

    // Now break it: a string left open does not parse.
    studio.set_score("$: s(\"bd\"");
    studio.chord("ctrl+s");

    assert_eq!(
        studio.errors(),
        None,
        "a score's refusal is the status line's news, not the error slot's"
    );
    assert_eq!(
        studio.status(),
        "refused - line 1: Expected `)` but found `EOF` · the last good score keeps playing"
    );
}

/// A score that checks out but fails in the engine lands in the error
/// line with its actionable cause - and repairing the score takes it away.
#[test]
fn an_engine_failure_shows_and_a_repair_clears_it() {
    let mut studio = hermetic();

    studio.set_score("null.x");
    studio.chord("ctrl+s");
    studio.settle();
    let error = studio.errors().expect("the engine's failure is on show");
    assert_eq!(
        error, "cannot read property 'x' of null",
        "the footer keeps the actionable cause and drops engine routing"
    );

    // The repair: a clean score installs, and the error line empties.
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(studio.errors(), None, "a clean install clears the slot");
}

/// A refused setup keeps its own slot. A newer score message pushes it
/// off the footer - the most recent message is the one on show - but the
/// setup is still broken after the score is fixed, so its message
/// resurfaces rather than being forgotten.
#[test]
fn a_setup_error_outlives_a_newer_score_message() {
    let mut studio = hermetic();

    // A setup that checks out but fails in the engine: applying it sets
    // the setup slot's error.
    select_prebake_row(&mut studio);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("null.x");
    studio.chord("ctrl+s");
    studio.settle();
    let setup_error = studio.errors().expect("the setup's failure is on show");
    assert!(
        setup_error.contains("prebake (local)"),
        "the message names the tab: {setup_error}"
    );

    // A newer score failure takes over the footer - it is the most recent
    // message anywhere. The score is one strip step back from the tab.
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    studio.set_score("null.x");
    studio.chord("ctrl+s");
    studio.settle();
    let newer = studio.errors().expect("the score's failure is on show");
    assert_eq!(newer, "cannot read property 'x' of null");

    // The score is fixed - and the setup's refusal comes back: the setup
    // is still broken, and its slot remembers.
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    let setup_error = studio
        .errors()
        .expect("the setup's refusal resurfaces once the score is fixed");
    assert!(setup_error.contains("prebake (local)"), "{setup_error}");
}

/// The error line stays in the footer above the device and MIDI chips:
/// part of the frame, not a floating popup.
#[test]
fn the_error_line_is_part_of_the_frame() {
    let mut studio = hermetic();

    studio.set_score("null.x");
    studio.chord("ctrl+s");
    studio.settle();
    let error = studio.errors().expect("an error is showing");
    let rows = studio.rows();
    let chips = row_containing(&rows, "♪");
    let error_row = row_containing(&rows, &error);
    assert!(
        chips == rows.len() - 1,
        "the chips are the footer's last row:\n{}",
        rows.join("\n")
    );
    assert!(
        error_row < chips,
        "the message stays in the footer above the chips:\n{}",
        rows.join("\n")
    );
    assert_eq!(
        error, "cannot read property 'x' of null",
        "the engine's actionable words show without routing noise"
    );
}

fn select_prebake_row(studio: &mut rustel_studio_e2e::Hermetic) {
    // The set's setup lives on the settings sheet now: the local prebake
    // row, last of the rows, Enter opens it as a tab.
    studio.chord("ctrl+o");
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ local prebake"))
        {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    panic!(
        "no row reads 'local prebake':\n{}",
        studio.rows().join("\n")
    );
}
