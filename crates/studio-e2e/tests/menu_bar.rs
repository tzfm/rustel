//! Drive all seven menus through F1 and letter mnemonics, not direct dispatch.
//! Check enabled actions and ensure each action runs once.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, row_containing};

/// File ▸ Quit quits at once: it has no arming step and no second prompt.
/// The test reaches the row with F1, the title's mnemonic and the row's
/// own, then reads the flag that ends the real loop.
#[test]
fn file_quit_quits_without_asking() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('f'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "Quit");
    studio.press(KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(
        studio.wants_quit(),
        "the menu row quits outright: {}",
        studio.status()
    );
}

/// F1 opens the bar with nothing dropped; F1 again closes it. The bar owns
/// the keyboard while it is up: a letter that is nobody's mnemonic is
/// swallowed rather than typed into the score.
#[test]
fn f1_opens_the_bar_and_f1_closes_it() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    assert!(studio.menu_open(), "F1 opens the bar");
    for title in [
        "File",
        "Edit",
        "Scene",
        "Transport",
        "View",
        "Options",
        "Help",
    ] {
        row_containing(&studio.rows(), title);
    }

    // A letter that matches no title and no row must not reach the score -
    // with a menu down, everything is the menu's.
    studio.press(KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(studio.menu_open(), "an unknown letter leaves the bar up");
    assert_eq!(
        studio.score().trim_end(),
        "$: s(\"bd\")",
        "the score never saw it"
    );

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    assert!(!studio.menu_open(), "F1 closes the bar");
}

/// A title mnemonic opens that menu, dropped: `s` for Scene, straight from
/// the bar with nothing else down.
#[test]
fn a_title_mnemonic_opens_that_menu_dropped() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);

    assert!(studio.menu_open(), "the mnemonic opened Scene");
    let rows = studio.rows();
    row_containing(&rows, "New scene");
    row_containing(&rows, "Duplicate scene");
}

/// The right arrow walks the seven menus in order, and a dropdown that is
/// down follows the focus - which is what makes browsing one keypress each.
/// The walk's assertions are on item rows only a dropped menu can show.
#[test]
fn right_walks_the_menus() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "New scene");

    // From Scene the walk goes forward through the bar's order.
    for (title, item) in [
        ("Transport", "Record a take"),
        ("View", "Set panel"),
        ("Options", "Devices"),
        ("Help", "Keyboard reference"),
        ("File", "New set"),
        ("Edit", "Select all"),
    ] {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        row_containing(&studio.rows(), item);
        row_containing(&studio.rows(), title);
    }
    // Wraps: one more from Edit lands back on Scene.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    row_containing(&studio.rows(), "New scene");
}

/// Escape closes the bar and hands the keyboard back: the same letter that
/// was a mnemonic is a letter in the score afterwards.
#[test]
fn esc_closes_the_bar_and_returns_the_keys() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(studio.menu_open());

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!studio.menu_open());

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.type_text("hello");
    assert!(
        studio.score().trim_end().ends_with("hello"),
        "the editor has its keys back"
    );
}

/// Scene ▸ New scene makes a second scene - the same thing ^N does, which
/// is exactly the contract: the menu row mirrors its chord, once.
#[test]
fn scene_new_scene_makes_a_scene() {
    let mut studio = hermetic();

    assert_eq!(studio.scene_names().len(), 1);
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE);

    assert!(!studio.menu_open(), "a chosen row closes the bar");
    assert_eq!(
        studio.scene_names().len(),
        2,
        "the menu row did what ^N does"
    );
}

/// Edit ▸ Select all + Edit ▸ Comment lines toggles the comment on the
/// starter score. Choosing a row closes the bar - a command runs with the
/// bar already gone - so the second row is a fresh F1 away.
#[test]
fn edit_rows_act_on_the_score() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE); // Edit, dropped
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE); // Select all
    assert!(!studio.menu_open(), "a chosen row closes the bar");

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('m'), KeyModifiers::NONE); // Comment lines
    assert!(!studio.menu_open());

    assert!(
        studio.score().contains("// $: s(\"bd\")"),
        "the whole score was commented: {}",
        studio.score()
    );
}

/// Options ▸ Devices opens the device panel. Options holds what the studio
/// is set to; View holds what it shows.
#[test]
fn options_devices_opens_the_device_panel() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('o'), KeyModifiers::NONE); // Options, dropped
    studio.press(KeyCode::Char('d'), KeyModifiers::NONE); // Devices…

    assert!(!studio.menu_open());
    row_containing(&studio.rows(), "audio out");
}

/// Options ▸ Menu bar hides the bar it was chosen from: the menu goes with
/// it, and F1 then means help, as it does in zen.
#[test]
fn options_menu_bar_hides_the_bar_and_f1_then_means_help() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('o'), KeyModifiers::NONE); // Options, dropped
    row_containing(&studio.rows(), "Header");
    row_containing(&studio.rows(), "Footer");
    studio.press(KeyCode::Char('m'), KeyModifiers::NONE); // Menu bar

    assert!(!studio.menu_open(), "the menu outlived its bar");
    assert!(
        studio.status().contains("opens settings"),
        "the status names the way back: {}",
        studio.status()
    );
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    assert!(!studio.menu_open(), "F1 opened a hidden bar");
    assert!(studio.help_is_open(), "F1 did not mean help");
}

/// Help ▸ Keyboard reference opens the help overlay - where the terminal
/// has room for it, which the hermetic 150×40 always does. The title
/// mnemonic drops Help directly; the dropdown needs no Down first.
#[test]
fn help_keyboard_reference_opens_help() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('h'), KeyModifiers::NONE); // Help, dropped
    studio.press(KeyCode::Char('k'), KeyModifiers::NONE); // Keyboard reference

    assert!(!studio.menu_open(), "the overlay closed the bar");
    assert!(studio.help_is_open(), "the overlay is up");
    row_containing(&studio.rows(), "keyboard");
}

/// Transport ▸ Update evaluates the score - the transport outranks the
/// menu on the chord path, and through the menu it simply works.
#[test]
fn transport_update_evaluates() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('t'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('u'), KeyModifiers::NONE);
    studio.settle();

    assert!(studio.is_playing(), "the menu row updated the transport");
}

/// Rows that cannot act are greyed and choosing them does nothing: with
/// one scene, Scene ▸ Delete scene is disabled, and its mnemonic is inert.
#[test]
fn a_greyed_row_does_not_act() {
    let mut studio = hermetic();

    // One score scene that cannot be deleted: the starter is the last.
    let before = studio.scene_names();
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('t'), KeyModifiers::NONE); // Delete scene: greyed

    assert_eq!(studio.scene_names(), before, "a greyed row is inert");
    assert!(studio.menu_open(), "a greyed row leaves the menu open");
}

/// Zen mode has no bar: F1 goes on meaning help, because a key that does
/// nothing at all is worse than a key that does the old thing.
#[test]
fn f1_means_help_when_there_is_no_bar() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(11), KeyModifiers::NONE); // zen folds the chrome away
    studio.settle();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    assert!(!studio.menu_open(), "zen has no bar to open");
    assert!(studio.help_is_open(), "F1 went on meaning help");

    // Help is modal for input: Esc puts it away first, then F11 leaves zen.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.press(KeyCode::F(11), KeyModifiers::NONE);
    studio.settle();
    assert!(!studio.help_is_open(), "help closed");
    assert!(!studio.menu_open());
    row_containing(&studio.rows(), "File");
}

/// View ▸ Set panel opens the same panel as ^B, with its tapes fold. The
/// bar-level letter for View is 'v'; 's' is the Set panel item's mnemonic
/// and works only while the menu is down.
#[test]
fn view_set_panel_opens_the_tapes_fold() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE); // View, dropped
    studio.press(KeyCode::Char('s'), KeyModifiers::NONE); // Set panel

    // A toggle item runs and closes the bar: the switch has flipped, and
    // the bar is out of the way of the room it opened.
    assert!(!studio.menu_open(), "a toggle closes the menu");
    assert_eq!(studio.focus(), Some(PanelKind::Set), "^B's own room");
    row_containing(&studio.rows(), "sessions");

    // Esc gives the keyboard back to the score; the panel stays docked.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!studio.menu_open());
    assert_eq!(studio.focus(), None);
    row_containing(&studio.rows(), "sessions");
}
