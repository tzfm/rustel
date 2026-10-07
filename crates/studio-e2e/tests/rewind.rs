//! Check the rewind flag, scene chip, menus, shortcuts and saved manifest.
//! Engine tests check where playback starts; these tests check the TUI controls
//! that select and display that behavior.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{hermetic, reopen_over, row_containing};

/// The row of the scene strip that carries the scenes' chips - not the
/// header, whose `▸ first` names the lit scene too. The chip reads
/// `{index} {name}` with the marks after (and a `▶` in front while it
/// plays), so `1 first` is the starter scene's chip on any row that has it.
fn chip_row(studio: &mut rustel_studio_e2e::Hermetic) -> String {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains("1 first"))
        .expect("the strip row with the scenes' chips is on screen")
}

/// ^⇧U flags the scene: the status says which way the next play goes, and
/// the chip wears `⟲` to say so. Another ^⇧U gives the flag back - and the
/// chip goes back to the ordinary join-the-cycle kind, with no `⟲` left
/// for a reader to misread.
#[test]
fn ctrl_shift_u_flags_the_scene_and_the_chip_wears_the_mark() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+u");
    assert_eq!(
        studio.status(),
        "\"first\" plays from its own cycle 0 ↺",
        "the flag is announced in the words the docs use"
    );
    let strip = chip_row(&mut studio);
    assert!(
        strip.contains("⟲"),
        "the lit chip wears the rewind mark: {strip:?}"
    );

    studio.chord("ctrl+shift+u");
    assert_eq!(
        studio.status(),
        "\"first\" joins the cycle already running",
        "the flag off is the ordinary join-the-cycle kind"
    );
    let strip = chip_row(&mut studio);
    assert!(
        !strip.contains("⟲"),
        "no mark on a scene that joins the cycle: {strip:?}"
    );
}

/// Scene ▸ Rewind on play is the same door as ^⇧U: the toggle row shows the
/// scene's current answer, Enter flips it, and the tick agrees with the
/// chip. The status line is the bar's own summary of the row - what the
/// docs say the door does.
#[test]
fn the_scene_menu_toggles_rewind_on_play() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "Rewind on play");

    // Off (the flag is not set): the row is unticked. Enter flips it and
    // closes the bar, a switch running its change and getting out of the
    // way of the strip it changed.
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(!studio.menu_open(), "a toggle closes the menu");
    assert!(
        chip_row(&mut studio).contains("⟲"),
        "the flip shows on the chip once the bar is gone"
    );

    // Again through the same door: the tick reads the flag it left.
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "✓Rewind on play");
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(!studio.menu_open());
    let strip = chip_row(&mut studio);
    assert!(
        !strip.contains("⟲"),
        "the second flip gave the flag back: {strip:?}"
    );
}

/// A replay tape has its own clock - there is nothing to rewind - so
/// ^⇧U on a replay tab refuses with a reason and the tape's chip grows no
/// `⟲`. The refusal is the point: a silent no-op would leave a musician
/// believing their tape now plays from the top.
#[test]
fn a_replay_tab_refuses_the_rewind_flag_with_a_reason() {
    let mut studio = hermetic();
    studio.write_tape("2026-09-11T10-00-00", [(0.0, "$: s(\"bd\")")]);
    studio.open_newest_tape();
    assert!(
        studio
            .current_scene()
            .is_some_and(|name| name.starts_with("2026-09-11")),
        "setup: the tape is open as the replay view"
    );

    studio.chord("ctrl+shift+u");
    assert_eq!(
        studio.status(),
        "a replay is on its own clock - there is nothing to rewind",
        "the refusal says why"
    );
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("2026-09-11") && row.contains("⟲")),
        "the tape's chip wears no rewind mark"
    );
}

/// A prebake tab is setup, not a scene that plays, so ^⇧U refuses with the
/// tab's own name rather than leaving a flag nothing reads.
#[test]
fn a_prebake_tab_refuses_the_rewind_flag_with_a_reason() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ local prebake"))
        {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        studio.scene_names().last(),
        Some(&"prebake (local)".to_owned()),
        "setup: the prebake tab is open"
    );

    studio.chord("ctrl+shift+u");
    assert_eq!(
        studio.status(),
        "prebake (local) is setup, not a scene that plays",
        "the refusal names the tab"
    );
}

/// The flag is the scene's, written beside the score in the set's manifest:
/// flag it, quit, reopen the set, and the chip wears `⟲` again before
/// anything is played - the answer survived the session.
#[test]
fn the_rewind_flag_survives_a_reopen() {
    let mut first = hermetic();
    let directory = first.set_directory();

    first.chord("ctrl+shift+u");
    assert_eq!(
        first.status(),
        "\"first\" plays from its own cycle 0 ↺",
        "setup: the flag is on"
    );

    let manifest = std::fs::read_to_string(directory.join("rustel-set.json"))
        .expect("the set's manifest is on disk");
    let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("manifest parses");
    assert_eq!(
        parsed["scenes"][0]["rewind"], true,
        "the flag is written beside the score, as the scene's own: {parsed}"
    );

    let mut reopened = reopen_over(first);
    assert_eq!(
        reopened.current_scene().as_deref(),
        Some("first"),
        "setup: the set reopened on its scene"
    );
    let strip = chip_row(&mut reopened);
    assert!(
        strip.contains("⟲"),
        "the reopened set's chip wears the mark: {strip:?}"
    );

    // And the menu row reads it: the answer is the scene's, not the
    // session's.
    reopened.press(KeyCode::F(1), KeyModifiers::NONE);
    reopened.press(KeyCode::Char('s'), KeyModifiers::NONE);
    row_containing(&reopened.rows(), "✓Rewind on play");
}

/// ^⇧S is the once-off, for a scene that wears no `⟲`: the score plays and
/// the status is the transport's own - nothing was flagged, so nothing on
/// the strip changed. Transport ▸ Rewind update is the same command by
/// menu.
#[test]
fn ctrl_shift_s_rewinds_once_without_flagging_the_scene() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+s");
    studio.settle();
    assert!(
        studio.is_playing(),
        "the once-off plays the score like any update"
    );
    let strip = chip_row(&mut studio);
    assert!(
        !strip.contains("⟲"),
        "the once-off flags nothing: {strip:?}"
    );
    assert!(
        !studio.rows().iter().any(|row| row.contains("⟲")),
        "no chip wears the mark anywhere on the strip"
    );

    // The menu row does the same thing the key does.
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('t'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "Rewind update");
    row_containing(&studio.rows(), "^⇧S");
    studio.press(KeyCode::Char('w'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.is_playing(),
        "Transport ▸ Rewind update is the once-off too"
    );
}
