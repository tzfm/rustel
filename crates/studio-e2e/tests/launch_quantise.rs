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
