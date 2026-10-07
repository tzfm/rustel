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
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_sized, row_containing};

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

/// View ▸ Visuals 1 opens the first dock with the keyboard; the status
/// names what the dock is and how to leave it.
#[test]
fn the_menu_row_opens_the_dock_focused_and_says_how_to_leave() {
    let mut studio = hermetic();

    open_dock(&mut studio, 0);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    let status = studio.status();
    assert!(
        status.starts_with("visuals 1 - "),
        "the dock names itself: {status}"
    );
    assert!(
        status.contains("a adds"),
        "the status offers the add key: {status}"
    );
    assert!(
        status.contains("View ▸ Visuals 1 hides"),
        "the status names the hide row: {status}"
    );
    // The dock's column is on screen: focused, so it paints its title -
    // up in the dock's own rows, not the footer's echo of the same words.
    assert_eq!(
        title_row(&mut studio, " visuals 1 "),
        Some(3),
        "the dock's title is on its column"
    );
}

/// A dock opened on a screen with no room for it must not take the keys:
/// the dock is hidden again and the score keeps the keys.
#[test]
fn a_dock_with_nowhere_to_go_does_not_hold_the_keyboard() {
    // Small enough that the layout finds no room for a right-edge dock:
    // the pane keeps only the width a score needs.
    let mut studio = hermetic_sized(80, 24);

    open_dock(&mut studio, 0);
    let status = studio.status().to_owned();
    let focused = studio.focus();
    if focused == Some(PanelKind::Viz) {
        // Wherever the dock was placed, it must actually be on screen -
        // the status promised it, so its column must be there.
        row_containing(&studio.rows(), "visuals 1");
    } else {
        assert_eq!(
            focused, None,
            "focus claimed by a dock that is not shown: {status}"
        );
        assert_eq!(
            studio.score().trim_end(),
            "$: s(\"bd\")",
            "the score kept its keys"
        );
    }
}

/// The add sheet: `a` raises it over every dialog, ↑/↓ choose, Enter adds
/// the chosen kind, and the dock's status says what was added - the
/// widget is in the dock's list from then on.
#[test]
fn the_add_sheet_adds_the_chosen_kind_to_the_dock() {
    let mut studio = hermetic();

    open_dock(&mut studio, 0);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "add a widget - ↑/↓ choose its kind, Enter adds it, Esc back"
    );
    row_containing(&studio.rows(), " add a widget ");

    // Walk to the scope and add it.
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "added scope",
        "the dock says what it added"
    );
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains(" add a widget ")),
        "the add sheet went away when the choice was made"
    );
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Viz),
        "the dock keeps the keys"
    );
}

/// Escape inside the add sheet cancels the add, not the dock; the second
/// Escape (a third press, after the add sheet's) hands the keys back to
/// the score and the dock stays on screen, now without the keyboard.
#[test]
fn esc_cancels_the_add_then_leaves_the_dock_open_but_unfocused() {
    let mut studio = hermetic();

    open_dock(&mut studio, 0);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    row_containing(&studio.rows(), " add a widget ");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "add cancelled");
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains(" add a widget ")),
        "the add sheet is gone"
    );
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Viz),
        "the dock keeps the keys its sheet held"
    );

    // The dock's own Esc: the keys go to the score, the column stays -
    // unfocused now, so without title or hints, but with its art.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    let status = studio.status();
    assert!(
        status.starts_with("back to the score"),
        "the status says where the keys went: {status}"
    );
    assert!(status.contains("hides it"), "and how to hide it: {status}");
    assert_eq!(studio.focus(), None);
    assert!(
        art_is_on_screen(&mut studio, 30),
        "the dock stays on screen, its art with it"
    );
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
