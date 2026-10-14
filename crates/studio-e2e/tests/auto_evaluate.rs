//! Check the auto evaluate rows and a paused edit applying itself through
//! the real worker: the file it writes, the refusal it keeps quiet, and the
//! defaults it keeps. The line an applied edit waits for is the launch
//! row's, and has its own suite.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic};

/// Walk the sheet's rows by label until the given one is selected - the
/// marker rides the selected row's own `▸`, so a status line quoting the
/// row's name cannot end the walk early.
fn select_row(studio: &mut Hermetic, marker: &str) {
    for _ in 0..40 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        studio.pump();
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// The drawn row that carries this label.
fn row_with(studio: &mut Hermetic, label: &str) -> String {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains(label))
        .unwrap_or_else(|| panic!("no row reads {label:?}:\n{}", studio.rows().join("\n")))
}

/// A playing studio with auto evaluate on and the typing pause at its
/// shortest, 250 ms: short enough that a settle outlives the pause, so
/// what the test reads after it is what landed.
fn playing_auto_studio() -> Hermetic {
    let mut studio = hermetic();
    studio.settle();
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "the set plays");

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ auto evaluate");
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert!(
        row_with(&mut studio, "auto evaluate").contains("● on"),
        "the row reads on: {}",
        row_with(&mut studio, "auto evaluate")
    );
    select_row(&mut studio, "▸ typing pause");
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert!(
        row_with(&mut studio, "typing pause").contains("250 ms"),
        "the row reads the short pause: {}",
        row_with(&mut studio, "typing pause")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio
}

/// The starter score's path - the file an applied edit lands in.
fn score_file(studio: &Hermetic) -> String {
    std::fs::read_to_string(studio.set_directory().join("first.strudel")).expect("the score file")
}

/// The two rows say what they do and what they are set to: off waits for
/// update, and the pause rests half a second before an edit applies.
#[test]
fn the_live_coding_rows_say_what_they_do() {
    let mut studio = hermetic();
    studio.chord("ctrl+o");

    select_row(&mut studio, "▸ auto evaluate");
    let row = row_with(&mut studio, "auto evaluate");
    assert!(
        row.contains("○ off"),
        "the default leaves the updates to the press: {row}"
    );
    let rows = studio.rows().join("\n");
    assert!(
        rows.contains("a paused edit applies itself while the set plays"),
        "the selected row explains itself:\n{rows}"
    );

    select_row(&mut studio, "▸ typing pause");
    let row = row_with(&mut studio, "typing pause");
    assert!(
        row.contains("500 ms"),
        "the pause rests half a second by default: {row}"
    );
    let rows = studio.rows().join("\n");
    assert!(
        rows.contains("how long typing rests before the edit applies"),
        "the selected row explains itself:\n{rows}"
    );
}

/// A paused edit applies itself while the set plays: the score is never
/// pressed, and the file on disk carries the edit anyway - the file is what
/// you hear, whoever asked for the update.
#[test]
fn a_paused_edit_applies_itself_while_the_set_plays() {
    let mut studio = playing_auto_studio();

    studio.set_score("$: s(\"bd*2\")");
    studio.settle();

    assert!(studio.is_playing(), "the set plays through the apply");
    assert_eq!(
        score_file(&studio),
        "$: s(\"bd*2\")",
        "the applied edit was written to the score's file"
    );
    assert!(
        !studio.status().contains("refused"),
        "a clean edit is applied, not refused: {}",
        studio.status()
    );
}

/// Text the checker refuses is not applied: the last good score keeps
/// playing and its file keeps its text, and the footer says why - the same
/// refusal an update meets, without the press.
#[test]
fn refused_text_keeps_the_last_good_score_playing() {
    let mut studio = playing_auto_studio();

    studio.set_score("$: s(\"bd\"");
    studio.settle();

    assert!(studio.is_playing(), "the last good score keeps playing");
    assert_eq!(
        score_file(&studio),
        "$: s(\"bd\")\n",
        "a refused edit writes nothing"
    );
    assert!(
        studio.status().contains("refused"),
        "the refusal is said: {}",
        studio.status()
    );
}
