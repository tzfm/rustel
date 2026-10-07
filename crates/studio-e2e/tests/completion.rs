//! Contextual reference and completion know what an argument can hold.
//!
//! What breaks if these fail: a chosen name replaces the wrong span of
//! the score, a call's list never appears, or a refusal is silent. The
//! status line and the score text are the contracts here.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_with_local_bank, row_containing};

/// With the caret inside a word, the chosen name replaces the whole token:
/// `testk|x` becomes `testkick`, not `testktestkick`. The answer is the
/// fixture's local `testkick` bank, so the test waits for no manifest fetch.
#[test]
fn a_half_typed_word_is_replaced_whole() {
    let mut studio = hermetic_with_local_bank();

    let text = "$: s(\"testkx\")";
    studio.set_score(text);
    studio.set_caret(text.find('x').expect("the extra letter"));
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.wait_for_catalogue();

    // The query is what is typed before the caret; `testkick` answers it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: s(\"testkick\")",
        "the whole token went, not just the half before the caret"
    );
}

/// A call whose argument has not been opened yet - `chord()` - still
/// offers its list, and the chosen name lands quoted.
#[test]
fn a_call_with_no_string_still_offers_its_list() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: chord()");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("chords"));

    // Open major onto its roots, walk onto `C^`, take it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: chord(\"C^\")",
        "the name arrived quoted, where the argument would start"
    );
}

/// Ctrl+D inside a completable string opens that call's own list: the
/// sounds, ranked closest first.
#[test]
fn ctrl_d_inside_a_string_opens_the_sounds() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: s(\"bd\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    // The caret now sits after the `d`: the query is the whole word.
    studio.chord("ctrl+d");

    assert_eq!(
        studio.status(),
        "the sounds - closest first; Enter replaces the word"
    );
    assert_eq!(studio.reference_tab(), Some("samples"), "the list opened");
    studio.wait_for_catalogue();
    studio.settle();
    row_containing(&studio.rows(), "search: bd");
}

/// An unfinished compound argument needs the call's parameters, without
/// replacing any of the text the musician is still writing.
#[test]
fn ctrl_d_inside_an_unfinished_argument_opens_its_parameters() {
    let mut studio = hermetic();
    let text = "$: s(\"sine\").distort(\"0.1:0.3:";
    studio.set_score(text);
    studio.set_caret(text.len());
    studio.chord("ctrl+d");

    assert_eq!(studio.focus(), Some(PanelKind::Reference));
    assert_eq!(studio.reference_tab(), Some("reference"));
    assert_eq!(studio.status(), "reference - distort");
    row_containing(&studio.rows(), "distort(distortion, volume, type)");
    row_containing(&studio.rows(), "type (number | string | Pattern)");
    assert_eq!(studio.score(), text);

    studio.press(KeyCode::PageDown, KeyModifiers::NONE);
    studio.press(KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(
        studio.score(),
        text,
        "reading parameters leaves the score intact"
    );
    studio.chord("ctrl+d");
    assert_eq!(studio.reference_tab(), None);
    assert_eq!(studio.focus(), None);
    studio.type_text("fold\")");
    assert_eq!(studio.score(), format!("{text}fold\")"));
}

#[test]
fn ctrl_d_before_a_quote_opens_the_function_arguments() {
    for name in ["s", "chord", "scale"] {
        let mut studio = hermetic_with_local_bank();
        let text = format!("$: {name}(");
        studio.set_score(&text);
        studio.set_caret(text.len());
        studio.chord("ctrl+d");

        assert_eq!(studio.reference_tab(), Some("reference"), "{name}");
        assert_eq!(studio.status(), format!("reference - {name}"));
        assert_eq!(studio.score(), text, "opening documentation is read-only");
    }
}

#[test]
fn ctrl_d_after_the_scale_argument_opens_inspires_arguments() {
    let mut studio = hermetic_with_local_bank();
    let text = "$: s(\"piano\").inspire(\"C:major:pentatonic\", ";
    studio.set_score(text);
    studio.set_caret(text.len());
    studio.chord("ctrl+d");

    assert_eq!(studio.reference_tab(), Some("reference"));
    assert_eq!(studio.status(), "reference - inspire");
    assert_eq!(studio.score(), text);
}

#[test]
fn ctrl_d_inside_inspires_scale_argument_opens_scales() {
    let mut studio = hermetic_with_local_bank();
    let text = "$: s(\"piano\").inspire(\"C:ma\")";
    studio.set_score(text);
    studio.set_caret(text.len() - 2);
    studio.chord("ctrl+d");

    assert_eq!(studio.reference_tab(), Some("scales"));
    assert_eq!(studio.score(), text);
}

#[test]
fn backspace_from_an_empty_contextual_search_returns_to_function_arguments() {
    for (name, tab) in [("s", "samples"), ("chord", "chords"), ("scale", "scales")] {
        let mut studio = hermetic_with_local_bank();
        let text = format!("$: {name}(\"\")");
        studio.set_score(&text);
        studio.set_caret(text.len() - 2);
        studio.chord("ctrl+d");
        assert_eq!(studio.reference_tab(), Some(tab), "{name}");

        studio.press(KeyCode::Backspace, KeyModifiers::NONE);

        assert_eq!(studio.reference_tab(), Some("reference"), "{name}");
        assert_eq!(studio.status(), format!("reference - {name}"));
        assert_eq!(studio.score(), text, "Backspace only changes the panel");
    }
}

/// A string whose call takes no list - a number, a pattern - refuses the
/// completion, and says why.
#[test]
fn a_string_nothing_can_complete_says_so() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: gain(\"\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");

    assert_eq!(studio.status(), "nothing to complete inside this string");
    assert_eq!(studio.reference_tab(), None);
}

/// A string naming something the score registers is the score's own:
/// there is nothing to complete.
#[test]
fn a_registered_name_is_the_scores_own() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: register(\"myfunction\", () => {})");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    for _ in 0..14 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+space");

    assert_eq!(
        studio.status(),
        "this string names something of your own - nothing to complete"
    );
    assert_eq!(studio.reference_tab(), None);
}

/// The panel open on one string and the caret moved into another: the
/// chord re-targets rather than closing - one press answers the new
/// string.
#[test]
fn the_completion_retargets_to_the_string_the_caret_is_in() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: s(\"bd\")\n$: s(\"hh\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    // Into the second string's token, after the second `h`: the query is
    // the whole word.
    for _ in 0..2 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.wait_for_catalogue();
    studio.settle();

    // The query is the new string's word.
    row_containing(&studio.rows(), "search: hh");

    // The panel owns the arrows, so the caret moves the way it does on a
    // keyboard-and-mouse stage: a click into the editor, mid-token. Back
    // in the first string, the chord re-targets the panel rather than
    // closing it.
    // The first score line sits three rows down under the menu bar and
    // the scene tabs; column 12 is mid-token in `s("bd")`.
    studio.click(12, 3);
    studio.chord("ctrl+space");
    studio.settle();
    row_containing(&studio.rows(), "search: bd");
}
