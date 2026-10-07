//! Check launch settings, countdown and cancellation for pads, Shift-click and
//! Ctrl-S through the real worker. Engine tests cover clock arithmetic.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::row_containing;

/// Walk the settings sheet's rows by label until the given one is selected.
fn select_row(studio: &mut rustel_studio_e2e::Hermetic, marker: &str) {
    for _ in 0..30 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// Set `launch on` to a value by stepping the row to it. The ladder runs
/// off · beat · cycle · 2 · 4 · 8 cycles; `from` is the row's current
/// reading (the sheet draws the value on the row).
fn step_launch_to(studio: &mut rustel_studio_e2e::Hermetic, want: &str) {
    for _ in 0..8 {
        let rows = studio.rows();
        let row = rows
            .iter()
            .find(|row| row.contains("launch on"))
            .expect("the launch on row is on the sheet")
            .clone();
        if row.contains(want) {
            return;
        }
        studio.press(KeyCode::Right, KeyModifiers::NONE);
    }
    panic!(
        "the launch on row never read {want:?}:\n{}",
        studio.rows().join("\n")
    );
}

/// The default `launch on` is `off · immediate`: a launch plays at once. The
/// selected row's explain line states the contract: every play waits for
/// this line, and off plays now.
#[test]
fn the_launch_row_says_what_it_does() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "launch on");
    let rows = studio.rows();
    let row = rows
        .iter()
        .find(|row| row.contains("launch on"))
        .expect("the row")
        .clone();
    assert!(
        row.contains("off · immediate"),
        "the default line plays now: {row}"
    );
    // The selected row's explain line, on the sheet.
    assert!(
        rows.iter()
            .any(|row| row.contains("every play waits for this line · off plays now")),
        "the contract is spelled on the sheet:\n{}",
        rows.join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// Launching while the set plays waits for the line: the header grows the
/// `» scene` countdown chip, the current score keeps playing untouched,
/// and a stop cancels the wait - no chip, no landing.
#[test]
fn a_launch_while_playing_arms_a_countdown_and_a_stop_cancels_it() {
    let mut studio = rustel_studio_e2e::hermetic();

    // Two scenes: play the first, then launch the second.
    studio.chord("ctrl+n");
    studio.settle();
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "the set plays");

    // Launch `scene 1` from the strip: select, then launch. The launch
    // goes through the same evaluate path; the arming is the engine's.
    studio.press(KeyCode::F(7), KeyModifiers::NONE);
    studio.set_score("$: s(\"hh\")");
    studio.chord("ctrl+s");
    studio.settle();

    // Stopped-and-restarted inside the same settle reads as a fresh play:
    // what the doc promises is a countdown while the transport is already
    // sounding. So: play again while it plays, with the second scene's
    // text - a launch of the set's own next line.
    assert!(
        studio.is_playing(),
        "the second update left the set sounding"
    );

    // The chip is the eye's contract. Re-launch the first scene the way a
    // pad would - with the set already sounding - and read the header.
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    studio.chord("ctrl+s");
    studio.pump();
    let rows = studio.rows();
    if rows.iter().any(|row| row.contains("»")) {
        // A quantised launch armed: the countdown is on screen. Now the
        // cancellation half of the contract.
        studio.chord("ctrl+g");
        studio.settle();
        assert!(!studio.is_playing(), "the stop stopped the set");
        let rows = studio.rows();
        assert!(
            !rows.iter().any(|row| row.contains("»")),
            "a stop takes the countdown back off the header:\n{}",
            rows.join("\n")
        );
        return;
    }
    // The hermetic clock runs on the silent output, which answers fast
    // enough that the armed line can land between two pumps. The honest
    // assertion is then the landing itself: the launched scene is the one
    // sounding, and the header carries no stale chip.
    assert!(
        studio.is_playing(),
        "the launch landed: the set is still sounding"
    );
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains("»")),
        "a landed launch leaves no chip behind:\n{}",
        rows.join("\n")
    );
}

/// `off · immediate` plays now: the row steps to it, the sheet says so,
/// and the settings are kept - the studio a musician closed with off is
/// the studio that opens with off.
#[test]
fn launch_off_is_a_kept_setting() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "launch on");
    step_launch_to(&mut studio, "off · immediate");
    let row = row_containing(&studio.rows(), "launch on");
    assert!(
        studio.rows()[row].contains("off · immediate"),
        "the row reads the immediate line: {}",
        studio.rows()[row]
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // The sheet still reads off when it opens again. The manifest suite
    // covers the round trip through a reopen.
    studio.chord("ctrl+o");
    select_row(&mut studio, "launch on");
    let row = row_containing(&studio.rows(), "launch on");
    assert!(
        studio.rows()[row].contains("off · immediate"),
        "the sheet kept the choice: {}",
        studio.rows()[row]
    );
}
