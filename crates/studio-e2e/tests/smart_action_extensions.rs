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
