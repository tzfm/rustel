//! Check dock focus, widget lists, add sheets and status through the menus.
//! visuals_stage covers rendering.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic};

/// Whether any row below `below` carries the dock's art glyphs - sextant
/// blocks the score never draws. A dock is on screen exactly when its
/// pictures are; the title is no proof, because an unfocused dock paints
/// no title, and the status line echoes the dock's name regardless.
fn art_is_on_screen(studio: &mut rustel_studio_e2e::Hermetic, below: usize) -> bool {
    studio.rows().iter().take(below).any(|row| {
        row.chars()
            .any(|glyph| matches!(glyph, '▀' | '▄' | '▌' | '▐'))
    })
}

/// The row of the dock's title, which only a focused dock paints - and
/// only in the dock's column, far above the footer where the status
/// echoes the same words.
fn title_row(studio: &mut rustel_studio_e2e::Hermetic, title: &str) -> Option<usize> {
    studio
        .rows()
        .iter()
        .take(30)
        .position(|row| row.contains(title))
}

/// The docks have no chord: View ▸ Visuals 1 and 2 open them. F1, `v` drops
/// View, and `u` or `v` picks the dock. A toggle row runs its change and
/// closes the bar, so the tests continue with the bar closed.
fn open_dock(studio: &mut rustel_studio_e2e::Hermetic, index: usize) {
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE); // View, dropped
    let row = if index == 0 { 'u' } else { 'v' };
    studio.press(KeyCode::Char(row), KeyModifiers::NONE);
}

/// The hide row while the dock holds the keys: the dock goes, the keys
/// come back, and the status says the honest thing.
#[test]
fn the_hide_row_takes_the_dock_and_the_keys_back_to_the_score() {
    let mut studio = hermetic();

    open_dock(&mut studio, 0);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    assert!(
        art_is_on_screen(&mut studio, 30),
        "the dock is up, its art with it"
    );

    open_dock(&mut studio, 0);
    assert_eq!(studio.status(), "visuals 1 hidden");
    assert_eq!(studio.focus(), None, "the keys are the score's again");
    assert!(
        !art_is_on_screen(&mut studio, 30),
        "the dock's pictures are off the screen:\n{}",
        studio.rows().join("\n")
    );
}

/// Two docks, two menu rows: View ▸ Visuals 1 and 2, each remembering
/// its own widgets, and the keyboard following the last one focused.
#[test]
fn two_docks_hold_their_own_widgets_and_the_keyboard_follows_focus() {
    let mut studio = hermetic();

    open_dock(&mut studio, 0);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    open_dock(&mut studio, 1);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    let status = studio.status();
    assert!(
        status.starts_with("visuals 2 - "),
        "the second dock took the keyboard: {status}"
    );
    // Focused docks paint their titles; the first, unfocused now, paints
    // only its art.
    assert_eq!(
        title_row(&mut studio, " visuals 2 "),
        Some(3),
        "the second dock's title is on its column"
    );
    assert!(
        art_is_on_screen(&mut studio, 30),
        "both docks' pictures are up"
    );

    // Hide the second: the keyboard goes to the first, which is still
    // open - the docks hand the keys between themselves before falling
    // back to the score.
    open_dock(&mut studio, 1);
    assert_eq!(studio.status(), "visuals 2 hidden");
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Viz),
        "the first dock, still open, took the keys"
    );
    let status = studio.status();
    assert!(
        status.starts_with("visuals 2 hidden"),
        "the hide is what was last said: {status}"
    );
    assert_eq!(
        title_row(&mut studio, " visuals 1 "),
        Some(3),
        "the first dock is focused again, title and all"
    );
    assert_eq!(
        title_row(&mut studio, " visuals 2 "),
        None,
        "the second dock is gone"
    );
    assert!(
        art_is_on_screen(&mut studio, 30),
        "the first dock's pictures are still up"
    );
}
