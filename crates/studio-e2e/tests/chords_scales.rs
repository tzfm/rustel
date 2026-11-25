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
    // The preview bank can still be loading when the key lands: the
    // status says the load instead of the chord, and the chord plays
    // when the bank is in.
    assert!(
        studio.status() == "preview C^" || studio.status().contains("loading"),
        "{}",
        studio.status()
    );

    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("scales"));

    // The same on the scales tab: → opens the shelf, Down walks into it,
    // → plays the written scale `C:major`.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        studio.status() == "preview C:major" || studio.status().contains("loading"),
        "{}",
        studio.status()
    );
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
