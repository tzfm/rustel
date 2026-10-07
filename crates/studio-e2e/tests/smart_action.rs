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
        studio.status().starts_with("smart action: Enter chooses"),
        "^J opened over the number: {}",
        studio.status()
    );
    // The menu's one row is the offer, in the words the docs use.
    row_containing(&studio.rows(), "make this number a fader");

    // Enter chooses: the menu becomes the form, filled from the number -
    // value 800, and a range guessed around it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio.status().starts_with("Tab between the fields"),
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
        studio.status().starts_with("smart action: Enter chooses"),
        "^J on the fader opens its menu: {}",
        studio.status()
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "smart action closed",
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

/// A number inside a pattern is not offerable - a fader there would be a
/// control nothing drives - and neither is no number at all. Both refusals
/// are status lines; no popup opens over the score either way.
#[test]
fn a_number_in_a_pattern_and_no_number_are_refused_with_a_reason() {
    let mut studio = hermetic();

    // Inside the quotes: the number is the music's, not a control's.
    studio.set_score("$: s(\"bd\").gain(0.8)");
    let in_pattern = studio.source().find("bd").expect("the pattern") + 1;
    studio.set_caret(in_pattern);
    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert!(
        studio.status().contains("inside a pattern"),
        "a pattern's number is refused with the why: {}",
        studio.status()
    );

    // Away from every number: the lesson is where a fader comes from.
    let end = studio.source().len();
    studio.set_caret(end);
    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert!(
        studio.status().contains("put the caret on a number first"),
        "no number is refused with the way back: {}",
        studio.status()
    );
}

/// The caret inside a `slider(…)` call: the menu offers to edit the fader
/// or take it off, and taking it off leaves the plain number the fader was
/// holding - the score reads as it did before the fader, and the removal
/// is said.
#[test]
fn the_fader_itself_can_be_taken_off_again() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\").lpf(slider(800, 100, 4000))");
    let at = studio.source().find("slider").expect("the call");
    studio.set_caret(at + 3);

    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert!(
        studio.status().starts_with("smart action: Enter chooses"),
        "^J on the fader opens: {}",
        studio.status()
    );
    // Two rows now: edit, or off. Walking to the second chooses off.
    row_containing(&studio.rows(), "edit this fader");
    row_containing(&studio.rows(), "take the fader off");
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        studio.source().contains("lpf(800)") && !studio.source().contains("slider("),
        "the call is gone, the number it held is back:\n{}",
        studio.source()
    );
    assert!(
        studio.status().starts_with("fader off"),
        "the removal is said with what is left: {}",
        studio.status()
    );
    assert!(
        !studio.is_evaluating(),
        "removing a fader does not evaluate"
    );
    studio.settle();
    assert!(
        !studio.is_playing(),
        "removing a fader does not start playback"
    );
}

/// Edit ▸ Smart action is greyed where the caret has nothing for it. The
/// row still draws, in the muted colour, and never takes the highlight or
/// Enter. With the caret on a number the same row is live.
#[test]
fn the_edit_menu_greys_the_smart_action_where_it_has_nothing_to_offer() {
    let mut studio = hermetic();

    // Away from every number, the row is greyed: it draws in the theme's
    // muted colour rather than the foreground the live rows wear.
    let end = studio.source().len();
    studio.set_caret(end);
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "Smart action");
    let row = studio
        .rows()
        .iter()
        .position(|row| row.contains("Smart action"))
        .expect("the row is on the dropped menu");
    let column = studio
        .rows()
        .into_iter()
        .nth(row)
        .and_then(|text| text.find("Smart action"))
        .expect("the label's column") as u16;
    let grey = studio
        .cell_fg(column, row as u16)
        .expect("the label is on screen");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // On a number, the same row is live: foreground ink, and Enter opens
    // the offer instead of stepping past.
    studio.set_score("$: s(\"bd\").lpf(800)");
    let at = studio.source().find("800").expect("the number");
    studio.set_caret(at + 2);
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    let lit = studio
        .rows()
        .iter()
        .position(|row| row.contains("Smart action"))
        .and_then(|row| {
            studio
                .rows()
                .into_iter()
                .nth(row)
                .and_then(|text| text.find("Smart action"))
                .map(|column| (row as u16, column as u16))
        })
        .expect("the row is on the dropped menu");
    let ink = studio
        .cell_fg(lit.1, lit.0)
        .expect("the label is on screen");
    // The row's own mnemonic fires it where the caret has a number: the
    // greyed row's mnemonic is skipped (`a` is nobody's while the row is
    // off), the live one's opens the offer.
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(
        studio.status().starts_with("smart action: Enter chooses"),
        "the live row answers its mnemonic: {}",
        studio.status()
    );

    assert_ne!(
        grey, ink,
        "a greyed row is drawn in the muted colour, not the live ink: {grey:?} vs {ink:?}"
    );
}
