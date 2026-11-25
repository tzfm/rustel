//! Check session listing, deletion and replay from the set panel. Fixtures use
//! the recorder's tape format with base64 sources and a header.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_runtime::session_log::encode_base64;
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_recording, row_containing};

/// Editing a block marks it with a filled dot; ^S starts the run from the
/// block and writes the view's edits back to the tape.
#[test]
fn an_edit_marks_the_block_and_ws_writes_back() {
    let mut studio = hermetic();
    let path = studio.write_tape(
        "2026-09-05T12-09-48",
        [(0.0, "$: s(\"bd\")"), (4.0, "$: s(\"sine\")")],
    );

    studio.select_newest_tape();
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    // Onto block 2, then a space at the block's head: an edit, and
    // marked as one. The block's text went in with the walk, so the
    // caret is at its start, and the space falls through the timeline
    // straight into the score.
    studio.press(KeyCode::Right, KeyModifiers::ALT);
    studio.type_text(" ");
    row_containing(&studio.rows(), "●");

    // ^S on a replay starts the run from the block and writes back.
    studio.press(KeyCode::Char('s'), KeyModifiers::CONTROL);
    studio.settle();

    let tape = std::fs::read_to_string(&path).expect("the tape reads back");
    assert!(
        tape.contains(&encode_base64(b" $: s(\"sine\")")),
        "the edit reached the disk: {tape}"
    );
}

/// The tape being written is listed in the fold with a `● rec` mark and
/// refuses deletion. The fixture opens with recording on, and the first
/// evaluate opens the tape. The refusal appears in the panel's error line.
/// `record_take.rs` covers Ctrl+Shift+R's WAV take, which is never listed
/// here.
#[test]
fn the_tape_being_written_is_marked_and_refuses_deletion() {
    let mut studio = hermetic_recording();

    // The first evaluate opens the tape and writes the save into it.
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(
        studio
            .tapes_directory()
            .read_dir()
            .expect("tape folder")
            .filter_map(Result::ok)
            .filter(|entry| entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "rustel-session"))
            .count(),
        1,
        "the first evaluate opened exactly one tape"
    );

    studio.select_newest_tape();
    let rows = studio.rows();
    row_containing(&rows, "● rec");

    studio.press(KeyCode::Delete, KeyModifiers::NONE);
    row_containing(&studio.rows(), "recording · press n for a new tape");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
}

/// The tape the first evaluate opened.
fn open_tape(studio: &rustel_studio_e2e::Hermetic) -> std::path::PathBuf {
    studio
        .tapes_directory()
        .read_dir()
        .expect("tape folder")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "rustel-session"))
        .expect("the first evaluate opened a tape")
}

/// Wait until the tape's last slider record plays `source`. A source the
/// tape already holds is written as a `ref` to the save that first held it.
fn wait_for_slider_record(studio: &mut rustel_studio_e2e::Hermetic, source: &str) {
    let tape = open_tape(studio);
    let expected = encode_base64(source.as_bytes());
    let last_slider_plays = |text: &str| {
        let saves: Vec<serde_json::Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|record| record["kind"] == "save")
            .collect();
        let Some(last) = saves.iter().rev().find(|record| record["via"] == "slider") else {
            return false;
        };
        let held = match last["ref"].as_u64() {
            Some(index) => usize::try_from(index)
                .ok()
                .and_then(|index| saves.get(index)),
            None => Some(last),
        };
        held.is_some_and(|record| record["source"] == expected.as_str())
    };
    let what = format!("a slider record of {source:?} last on the tape");
    studio.pump_until(Duration::from_secs(5), &what, |_| {
        std::fs::read_to_string(&tape).is_ok_and(|text| last_slider_plays(&text))
    });
}

/// A slider moved with Alt+Up while the set plays is written to the tape
/// once it settles, as the evaluated score with the new value, so a replay
/// plays the move. A move back to the evaluated value is written too.
#[test]
fn a_settled_slider_move_is_recorded_on_the_tape() {
    let mut studio = hermetic_recording();
    let score = "$: s(\"bd\").gain(slider(0.8, 0, 1, 0.1))";
    studio.set_score(score);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "the set plays");
    studio.set_caret(score.find("0.8").expect("slider value"));
    studio.press(KeyCode::Up, KeyModifiers::ALT);
    assert!(studio.score().contains("slider(0.9,"), "{}", studio.score());
    wait_for_slider_record(&mut studio, &score.replace("slider(0.8,", "slider(0.9,"));
    studio.press(KeyCode::Down, KeyModifiers::ALT);
    assert!(studio.score().contains("slider(0.8,"), "{}", studio.score());
    wait_for_slider_record(&mut studio, score);
}

/// A move on a knobs rail that plays live is written to the tape the same
/// way, once it settles.
#[test]
fn a_settled_knobs_rail_move_is_recorded_on_the_tape() {
    let mut studio = hermetic_recording();
    let score = "$: s(\"bd\").lpf(4000)";
    studio.set_score(score);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "the set plays");
    studio.set_caret(score.find("4000").expect("cutoff"));
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Knobs));
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    let moved = studio.score();
    assert_ne!(moved, score, "the rail writes the score");
    wait_for_slider_record(&mut studio, &moved);
}
