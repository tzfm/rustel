//! The chords and scales tabs: the music-theory browsers, and the colour
//! vocabulary that completes inside a string.
//!
//! What breaks if these fail: a musician cannot find the chord or scale
//! they can hear but not spell, or a chosen name lands in the score
//! mangled. Every written name is asserted exactly as a score writes it.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{hermetic, row_containing};

/// The chords tab: opening a quality lists its twelve roots, and the
/// written name is spelled the way a score writes it - `C-7` for C minor
/// seventh, from the dictionary's own symbol.
#[test]
fn the_chords_tab_opens_a_quality_onto_its_roots() {
    let mut studio = hermetic();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("chords"));
    studio.settle();

    // Minor is the second quality in the browser's own order. Enter on
    // the quality row opens it onto its twelve roots.
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();

    let rows = studio.rows();
    row_containing(&rows, "C-   ");
    row_containing(&rows, "Db-   ");
}

/// A chosen chord is inserted quoted and whole where the caret stands -
/// inside the string, replacing the half-typed word.
#[test]
fn a_chord_inserts_quoted_into_the_string() {
    let mut studio = hermetic();

    studio.set_score("$: chord(\"\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("chords"));

    // Row zero is the first quality - a shelf, not a chord: Enter opens
    // major onto its roots in place, Down walks onto the `C^` row, and
    // Enter there is what the browser was opened for: the chord goes in.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: chord(\"C^\")",
        "the written chord landed in the string"
    );
}

/// The scales tab: opening a scale lists its twelve tonics, and a chosen
/// tonic inserts as the score writes it - `G:minor`.
#[test]
fn a_scale_tonic_inserts_with_its_tonic() {
    let mut studio = hermetic();

    studio.set_score("$: scale(\"\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("scales"));

    // Minor is the second name in the browser's own order: Down to it,
    // Enter opens it onto its tonics, Down to the second tonic (G is the
    // eighth root, so walk by hand and stop where the search said).
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();

    let rows = studio.rows();
    // The drawn tonics use the notation glyphs: `D♭`, not `Db`.
    row_containing(&rows, "D♭:minor");

    // The roots run C Db D Eb E F Gb G Ab A Bb B; the seventh tonic row
    // is G. Six Downs from the scale row, then Enter.
    for _ in 0..8 {
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: scale(\"G:minor\")",
        "the chosen tonic landed, tonic spelled first"
    );
}

/// Searching the chords tab by a written chord - `Ab-7` - finds the
/// quality whose symbol spells it.
#[test]
fn the_chord_search_answers_a_written_chord() {
    let mut studio = hermetic();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();

    studio.type_text("Ab-7");
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "search: Ab-7");
}

/// The scales search answers a written scale too - `C:minor` finds minor.
#[test]
fn the_scale_search_answers_a_written_scale() {
    let mut studio = hermetic();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    for _ in 0..3 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(studio.reference_tab(), Some("scales"));
    studio.settle();

    studio.type_text("C:minor");
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "search: C:minor");
    row_containing(&rows, "minor");
}

/// Previews: → on a chord row plays it as a chord and says so; → on a
/// scale row plays its run. (The status is the contract; the audio is the
/// engine's, already covered elsewhere.)
#[test]
fn arrows_preview_chords_and_scales() {
    let mut studio = hermetic();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();

    // The first quality row is major: → opens it onto its roots, and
    // → again on the quality row previews nothing (the row is a shelf,
    // not a chord). Down onto the C^ row and → plays the written chord.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview C^");

    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("scales"));

    // The same on the scales tab: → opens the shelf, Down walks into it,
    // → plays the written scale `C:major`.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview C:major");
}

/// A colour name is not a function: inside `.color("…")` the completion
/// offers the colour vocabulary, drawn each in its own colour.
#[test]
fn a_colour_string_completes_from_the_vocabulary() {
    let mut studio = hermetic();

    studio.set_score("$: s(\"bd\").color(\"\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("reference"));
    studio.settle();

    // The vocabulary is a word list in the reference tab's clothes: the
    // search line is live, and choosing a name puts it in the string.
    studio.type_text("cyan");
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "search: cyan");

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio.score().contains("\"cyan\""),
        "the colour name landed in the string: {}",
        studio.score()
    );
}
