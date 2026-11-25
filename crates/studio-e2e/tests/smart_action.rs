//! Check Ctrl-J menus and forms for each supported caret target, including
//! status on refusal. keymap covers the documented shortcut.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{hermetic, row_containing};

/// The caret on a bare number: ^J offers to make it a fader, the form
/// arrives filled from the number, and Enter writes the call without playing it.
/// Esc after a write says the fader was added - after none, that nothing
/// happened.
#[test]
fn a_number_becomes_a_fader_and_the_form_writes_the_call() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\").lpf(800)");
    let at = studio.source().find("800").expect("the number");
    studio.set_caret(at + 2);

    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert!(
        studio.status().starts_with("quick action: Enter chooses"),
        "^J opened over the number: {}",
        studio.status()
    );
    // The menu's one row is the offer, in the words the docs use.
    row_containing(&studio.rows(), "make this number a fader");

    // Enter chooses: the menu becomes the form, filled from the number -
    // value 800, and a range guessed around it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio
            .status()
            .starts_with("Tab or the arrows between the fields"),
        "Enter opened the form: {}",
        studio.status()
    );
    row_containing(&studio.rows(), "value");
    row_containing(&studio.rows(), "800");

    // Enter again writes the call into the score where the number stood.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio.source().contains("slider(800"),
        "the number became the fader's value:\n{}",
        studio.source()
    );
    assert!(
        studio.status().starts_with("fader added"),
        "the write is said: {}",
        studio.status()
    );
    assert!(!studio.is_evaluating(), "adding a fader does not evaluate");
    studio.settle();
    assert!(
        !studio.is_playing(),
        "adding a fader does not start playback"
    );

    // And the gesture is closable without a trace the second time: ^J on
    // the fader it just made, Esc on the form, and the score is as the
    // write left it.
    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert!(
        studio.status().starts_with("quick action: Enter chooses"),
        "^J on the fader opens its menu: {}",
        studio.status()
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "quick action closed",
        "Esc with nothing written says so plainly"
    );
    assert!(
        studio.source().contains("slider(800"),
        "a closed menu wrote nothing:\n{}",
        studio.source()
    );

    // Editing that fader also only changes the score.
    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    for digit in "900".chars() {
        studio.press(KeyCode::Char(digit), KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(studio.source().contains("slider(900"));
    assert!(!studio.is_evaluating(), "editing a fader does not evaluate");
    studio.settle();
    assert!(
        !studio.is_playing(),
        "editing a fader does not start playback"
    );
}
