//! Check form paste, out-of-range values, equal bounds and incomplete slider calls.
//! Assert form warnings, score ownership, undo and successful evaluation.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{hermetic, row_containing};

/// Put the caret on `needle` in the score, then press ^J and choose the
/// form's one row.
fn open_form_on(studio: &mut rustel_studio_e2e::Hermetic, needle: &str) {
    let at = studio.source().find(needle).expect("the number");
    studio.set_caret(at);
    studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
}

/// A bracketed paste goes into the form's value field, not the score under
/// it, and Enter replaces the whole number the form opened on - the caret
/// on its first digit or on the digit after its point - in one undoable
/// write.
#[test]
fn bracketed_paste_edits_the_fader_form_without_changing_the_score_behind_it() {
    for (before, caret, pasted, after) in [
        (
            "$: s(\"bd\").lpf(2300)",
            "2300",
            "50",
            "$: s(\"bd\").lpf(slider(50, 0, 5000, 1))",
        ),
        (
            "$: s(\"bd\").gain(.8)",
            "8",
            "0.5",
            "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.01))",
        ),
    ] {
        let mut studio = hermetic();
        studio.set_score(before);
        open_form_on(&mut studio, caret);
        studio.paste(pasted);
        assert_eq!(studio.source(), before, "the form owns the paste");
        row_containing(&studio.rows(), &format!("▸ value {pasted}"));

        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            !studio.is_evaluating(),
            "applying the form does not evaluate"
        );
        studio.settle();
        assert_eq!(studio.source(), after, "the whole number is replaced");
        studio.chord("ctrl+s");
        studio.settle();
        assert!(studio.is_playing(), "{}", studio.status());
        assert_eq!(studio.errors(), None);

        studio.chord("ctrl+z");
        assert_eq!(studio.source(), before, "one undo restores the number");
    }
}

/// A value typed above its ceiling is pulled back to the ceiling. The form
/// shows the pulled number and a warning and writes nothing, and no
/// evaluation starts. The next Enter writes what the form shows.
#[test]
fn a_value_outside_the_range_is_pulled_in_and_said() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\").lpf(800)");
    open_form_on(&mut studio, "800");

    // The value field opens selected: typing replaces the 800. The range
    // guessed for 800 is 0-2000.
    for digit in "3000".chars() {
        studio.press(KeyCode::Char(digit), KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();

    let rows = studio.rows();
    row_containing(&rows, "▸ value 2000");
    row_containing(&rows, "⚠ value pulled into range · Enter writes");
    assert_eq!(
        studio.source(),
        "$: s(\"bd\").lpf(800)",
        "nothing is written until the pull has been read"
    );
    assert!(!studio.is_evaluating(), "and nothing is evaluating");

    // The second Enter writes the pulled value without evaluating it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !studio.is_evaluating(),
        "applying the form does not evaluate"
    );
    studio.settle();
    assert_eq!(
        studio.source(),
        "$: s(\"bd\").lpf(slider(2000, 0, 2000, 1))"
    );
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("⌘ smart action") || row.contains("value pulled")),
        "written and away: the form is gone:\n{}",
        studio.screen()
    );
    assert!(
        !studio.is_playing(),
        "applying the form does not start playback"
    );
    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        studio.is_playing(),
        "the engine took it: {}",
        studio.status()
    );
    assert_eq!(studio.errors(), None, "the fader's score plays clean");
}

/// A min at or above its max has no travel; the form refuses to write a
/// fader that starts off its rail and keeps the message on the form.
#[test]
fn a_min_that_swallows_its_max_is_refused() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\").lpf(800)");
    open_form_on(&mut studio, "800");

    // Field 2 is min: type a floor above the ceiling.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    for digit in "9999".chars() {
        studio.press(KeyCode::Char(digit), KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();

    row_containing(&studio.rows(), "⚠ min has to be below max");
    assert_eq!(
        studio.source(),
        "$: s(\"bd\").lpf(800)",
        "nothing was written"
    );
}

/// An empty `slider(` gets the form and not a refusal. The form completes
/// it in place, with its bracket still open or already closed around the
/// caret. The result is one whole call that parses, and the engine takes
/// it on an explicit update.
#[test]
fn a_half_typed_slider_is_finished_by_the_form() {
    for before in [
        // Closed and empty: the `)` is the call's own.
        "$: s(\"bd\").gain(slider())",
        // `gain()` first, then `slider(` typed inside it: the `)` is gain's.
        "$: s(\"bd\").gain(slider()",
    ] {
        let mut studio = hermetic();
        studio.set_score(before);
        let inside = studio.source().find("slider(").expect("the started call") + "slider(".len();
        studio.set_caret(inside);

        studio.press(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert!(
            studio.status().starts_with("smart action: Enter chooses"),
            "the started call is offered, not refused ({before}): {}",
            studio.status()
        );
        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            !studio.is_evaluating(),
            "finishing a slider does not evaluate"
        );
        studio.settle();

        assert_eq!(
            studio.source(),
            "$: s(\"bd\").gain(slider(1, 0, 1, 0.01))",
            "the form finished the call in place, from {before:?}"
        );
        assert!(
            !studio.is_playing(),
            "finishing a slider does not start playback"
        );
        studio.chord("ctrl+s");
        studio.settle();
        assert!(
            studio.is_playing(),
            "the engine took it, from {before:?}: {}",
            studio.status()
        );
        assert_eq!(
            studio.errors(),
            None,
            "the finished score plays clean, from {before:?}"
        );
    }
}
