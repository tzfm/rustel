//! Check session listing, deletion and replay from the set panel. Fixtures use
//! the recorder's tape format with base64 sources and a header.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_runtime::session_log::encode_base64;
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_recording, row_containing};

/// A tape's file name, for tests that read one back.
fn tape_name(timestamp: &str) -> String {
    format!(
        "session-{timestamp}{}",
        rustel_runtime::product::SESSION_FILE_SUFFIX
    )
}

/// ^B opens the set panel, the sessions fold lists the set's tapes newest
/// first, and Esc hands the keys back to the score - the panel, docked,
/// stays.
#[test]
fn ctrl_b_opens_the_set_panel_and_the_fold_lists_tapes() {
    let mut studio = hermetic();
    studio.write_tape("1970-01-01T00-00-00", [(0.0, "$: s(\"bd\")")]);
    studio.write_tape(
        "2001-09-09T01-46-40",
        [(0.0, "$: s(\"bd\")"), (4.0, "$: s(\"sd\")")],
    );

    studio.chord("ctrl+b");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    assert!(studio.status().starts_with("set - "), "{}", studio.status());

    // The fold, shut, says how many tapes wait under it.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    row_containing(&studio.rows(), "▸ sessions (2)");

    // Opened, the tapes are the fold's own rows, newest first. The fold
    // is narrow: a tape's name clips before its size note does.
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    row_containing(&studio.rows(), "▾ sessions (2)");
    let rows = studio.rows();
    let newer = row_containing(&rows, "Sep 09 01:46:40");
    let older = row_containing(&rows, "Jan 01 00:00:00");
    assert!(newer < older, "newest first: {newer} < {older}");

    // Docked, the panel stays and the keys go back to the score.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert_eq!(
        studio.status(),
        "back to the score - the set panel stays; ^B hides it"
    );
}

/// Enter on a tape opens it as the replay view on its first block: the
/// editor holds the first save, and the panel stayed behind it. Opening
/// another tape retargets the one replay view.
#[test]
fn enter_opens_a_tape_as_the_replay_view() {
    let mut studio = hermetic();
    studio.write_tape(
        "2026-09-05T12-09-48",
        [
            (0.0, "$: s(\"bd\")"),
            (4.0, "$: s(\"sd\")"),
            (9.0, "$: s(\"hh\")"),
        ],
    );

    studio.select_newest_tape();
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(studio.focus(), None, "the score took the keys back");
    assert_eq!(
        studio.status(),
        rustel_studio::keybinds::shortcut_label(
            "replay - 3 saves · click a block or Alt+←/→ · ^Enter plays from it · ^W back to the set"
        )
    );
    assert!(
        studio
            .scene_names()
            .contains(&"2026-09-05T12-09-48".to_owned())
    );
    assert_eq!(
        studio.score().trim_end(),
        "$: s(\"bd\")",
        "the view opened on block 1"
    );

    // Opening another tape replaces what the replay view shows - one
    // replay, and the strip reads the new tape's count. The panel reads
    // the folder when it opens, so hide it and bring it back to see the
    // tape written behind it; the fold starts shut again.
    studio.write_tape("2026-09-05T13-00-00", [(0.0, "$: n(\"bd\")")]);
    studio.chord("ctrl+b");
    studio.open_newest_tape();
    let names = studio.scene_names();
    assert_eq!(
        names
            .iter()
            .filter(|name| *name == &"2026-09-05T13-00-00".to_owned())
            .count(),
        1,
        "the second tape took the first's place: {names:?}"
    );
    assert!(
        !names.contains(&"2026-09-05T12-09-48".to_owned()),
        "the old tape is gone from the strip: {names:?}"
    );
}

/// Alt+→ walks the blocks and puts each one's code in the editor; the walk
/// itself is not an edit. ^W closes the view and says where to find it again.
#[test]
fn alt_arrows_walk_the_blocks() {
    let mut studio = hermetic();
    let tapes = studio.tapes_directory();
    studio.write_tape(
        "2026-09-05T12-09-48",
        [
            (0.0, "$: s(\"bd\")"),
            (4.0, "$: s(\"sd\")"),
            (9.0, "$: s(\"hh\")"),
        ],
    );

    studio.select_newest_tape();
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    studio.press(KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(studio.score().trim_end(), "$: s(\"sd\")");
    studio.press(KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(studio.score().trim_end(), "$: s(\"hh\")");

    // Back out again.
    studio.press(KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(studio.score().trim_end(), "$: s(\"sd\")");

    // The walk is not an edit: ^W closes, and the tape is as it was.
    studio.press(KeyCode::Char('w'), KeyModifiers::CONTROL);
    studio.settle();
    assert_eq!(
        studio.status(),
        "closed the replay - back to the set; the set panel (^B) opens it again"
    );
    assert!(
        !studio
            .scene_names()
            .contains(&"2026-09-05T12-09-48".to_owned()),
        "the replay is gone from the strip"
    );
    let tape = std::fs::read_to_string(tapes.join(tape_name("2026-09-05T12-09-48")))
        .expect("the tape reads back");
    assert!(
        tape.contains(&encode_base64(b"$: s(\"hh\")")),
        "closing never rewrote the tape"
    );
}

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

/// Delete on a tape is a two-press delete: the first names it, Enter
/// removes it and refreshes the fold's count.
#[test]
fn the_delete_flow_arms_then_removes() {
    let mut studio = hermetic();
    let path = studio.write_tape("2026-09-05T12-09-48", [(0.0, "$: s(\"bd\")")]);

    studio.select_newest_tape();
    studio.press(KeyCode::Delete, KeyModifiers::NONE);

    assert!(path.exists(), "the first press only arms");
    assert_eq!(
        studio.status(),
        "delete 2026-09-05T12-09-48 from the disk? Enter deletes it · Esc keeps it"
    );

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(!path.exists(), "Enter removes it");
    // The status names the tape, not the runner's temp path.
    let name = path
        .file_name()
        .expect("the deleted tape has a file name")
        .to_string_lossy();
    assert_eq!(studio.status(), format!("deleted {name}"));

    // The fold refreshed itself: shut or open, it counts no tapes.
    row_containing(&studio.rows(), "sessions (0)");
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

/// While the panel has the keyboard, a letter that is not one of its keys
/// is refused with a message and does not reach the score. Only Esc returns
/// the keys to the score. The reference entry page is the one surface that
/// types through.
#[test]
fn unknown_letters_are_refused_and_said() {
    let mut studio = hermetic();
    studio.write_tape("2026-09-05T12-09-48", [(0.0, "$: s(\"bd\")")]);

    studio.chord("ctrl+b");
    studio.press(KeyCode::Char('x'), KeyModifiers::NONE);

    assert_eq!(
        studio.focus(),
        Some(PanelKind::Set),
        "the panel kept the keyboard: a refused key does not land in the score"
    );
    assert!(
        studio.status().contains("the set has the keyboard"),
        "the refusal is said, not swallowed: {:?}",
        studio.status()
    );

    // The score never saw the 'x': evaluating now leaves it as it was.
    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        !studio.score().starts_with('x'),
        "the refused letter stayed out of the score: {:?}",
        studio.score()
    );

    // Esc is what hands the keys back, and only that.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "Esc gives the keys back to the score");
}
