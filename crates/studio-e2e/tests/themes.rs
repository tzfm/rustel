//! Check theme browsing, saving and deletion. Selection applies immediately;
//! Enter and Esc both keep it. New/edit/delete require the primary modifier
//! because unmodified letters search. Built-ins must not be overwritten.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, row_containing};

const STARTER: &str = "rustel-dark";

/// The picker's front door: opening it says what the keys now do, and
/// typing filters the list - the search, not a verb, owns bare letters.
#[test]
fn the_picker_opens_and_typing_filters_the_list() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    assert_eq!(studio.focus(), Some(PanelKind::Theme));
    assert_eq!(
        studio.status(),
        "theme - type to search · ←/→/↑/↓ preview · Enter keep · Esc back"
    );

    studio.type_text("basket");
    let row = row_containing(&studio.rows(), "basketball");
    assert!(
        studio.rows()[row].contains("▸"),
        "the only match is the selected one: {}",
        studio.rows()[row]
    );
}

/// Browsing applies the theme once the keys stop - the studio is the
/// preview, debounced so a held arrow does not compile a sketch per row -
/// and Esc keeps what is on screen, remembers it, and hands the keys back.
#[test]
fn browsing_applies_the_theme_and_esc_keeps_it() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.type_text("basket");
    studio.settle();
    assert_eq!(
        studio.theme_name(),
        "basketball",
        "walking the list applies the theme for real"
    );
    assert_eq!(studio.status(), "theme - basketball");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "the keys go back to the score");
    assert_eq!(
        studio.theme_name(),
        "basketball",
        "Esc keeps what is on screen"
    );
    assert!(
        studio
            .status()
            .starts_with("theme basketball - remembered in "),
        "{}",
        studio.status()
    );
}

/// Enter keeps what the list landed on, exactly as Esc does - both doors
/// out of the picker are the same door.
#[test]
fn enter_keeps_the_theme_the_list_landed_on() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.type_text("basket");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.theme_name(), "basketball", "the theme is kept");
    assert_eq!(studio.focus(), None, "the keys go back to the score");
    assert!(
        studio
            .status()
            .starts_with("theme basketball - remembered in "),
        "{}",
        studio.status()
    );
}

/// `n` (primary) shapes a new theme from the current one in the editor;
/// an untouched editor's Esc walks back to the picker, not to the score.
#[test]
fn n_opens_the_editor_and_esc_walks_back_to_the_picker() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    assert_eq!(studio.focus(), Some(PanelKind::ThemeEditor));
    assert_eq!(
        studio.status(),
        "theme editor - everything applies live; s saves, Esc goes back to the themes"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Theme),
        "the editor's Esc lands on the theme list"
    );
    assert_eq!(
        studio.status(),
        "theme - type to search · ←/→/↑/↓ preview · Enter keep · Esc back"
    );
    assert_eq!(studio.theme_name(), STARTER, "nothing changed on the way");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "the picker's Esc hands the keys back");
}

/// The save sheet names the draft and refuses nothing silently: an empty
/// name is asked for, a taken name is offered a free variation.
#[test]
fn the_save_sheet_refuses_an_empty_and_a_taken_name() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    // Plain `s` opens the save sheet, seeded with a free name; clear it.
    studio.type_text("s");
    for _ in 0..20 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    row_containing(&studio.rows(), "a theme needs a name");

    // A built-in's name is taken - the sheet says so and offers a free
    // variation instead of quietly writing over someone.
    studio.type_text(STARTER);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let row = row_containing(&studio.rows(), "is taken");
    assert!(
        studio.rows()[row].contains("rustel-dark-2"),
        "the sheet offers the free name: {}",
        studio.rows()[row]
    );
}

/// A saved-and-kept theme becomes the studio's theme, lands in the
/// picker's list marked yours, and the picker's Esc keeps it there.
#[test]
fn a_saved_theme_is_kept_and_listed_as_yours() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    studio.type_text("s");
    for _ in 0..20 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.type_text("suite-dusk");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        studio.status().starts_with("theme saved and kept - "),
        "the status names the saved file: {}",
        studio.status()
    );
    assert_eq!(studio.theme_name(), "suite-dusk");
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Theme),
        "keeping lands back on the theme list"
    );

    // The list is open on the new theme, marked yours.
    let row = row_containing(&studio.rows(), "suite-dusk");
    assert!(
        studio.rows()[row].contains("(yours)"),
        "a user theme says so: {}",
        studio.rows()[row]
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        studio.theme_name(),
        "suite-dusk",
        "the picker's Esc keeps the current theme"
    );
    assert_eq!(studio.focus(), None);
}

/// Saving without keeping (Tab to ` save ` in the sheet) writes the file
/// and leaves the editor open wearing the draft; the ladder's Esc then
/// puts the pre-editor theme back, and the file stays listed as yours.
#[test]
fn saving_without_keeping_writes_the_file_and_the_ladder_puts_the_original_back() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    studio.type_text("s");
    for _ in 0..20 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.type_text("suite-two");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        studio.status().starts_with("theme saved - "),
        "{}",
        studio.status()
    );
    assert_eq!(
        studio.focus(),
        Some(PanelKind::ThemeEditor),
        "without keeping, the editor stays open"
    );

    // The ladder's last Esc puts the pre-editor theme back.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.theme_name(), STARTER);
    assert_eq!(studio.focus(), Some(PanelKind::Theme));

    // Search for it and check the LIST row, not the search box: the
    // query itself renders in a `search:` row, so the needle has to name
    // the mark only a list row carries.
    studio.type_text("suite-two");
    let listed = studio
        .rows()
        .iter()
        .find(|row| row.contains("suite-two") && row.contains("(yours)"))
        .expect("the saved theme is in the picker's list")
        .clone();
    assert!(listed.contains("(yours)"));
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// Deleting asks twice: the first ^D names what would go and the second
/// removes the file. The deleted theme leaves the list and the studio
/// continues on a theme that still exists.
#[test]
fn deleting_a_user_theme_asks_twice() {
    let mut studio = hermetic();

    // A theme to delete: saved and kept, so the picker opens on it.
    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    studio.type_text("s");
    for _ in 0..20 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.type_text("suite-dusk");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.theme_name(), "suite-dusk");

    // One d asks, the next lets.
    studio.chord("ctrl+d");
    assert!(
        studio.status().contains("again to delete suite-dusk"),
        "{}",
        studio.status()
    );
    studio.chord("ctrl+d");
    // The delete re-selects and previews what is left, so the status the
    // test sees is that preview - the deletion itself is what the list
    // below proves.
    assert_ne!(
        studio.theme_name(),
        "suite-dusk",
        "the studio no longer wears the deleted theme"
    );

    // It is gone from the list. The `search:` row echoes the query, so the
    // needle is the `(yours)` mark only a list row carries.
    studio.type_text("suite-dusk");
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("suite-dusk") && row.contains("(yours)")),
        "the deleted theme is no longer in the list"
    );

    // The picker's Esc keeps the theme the delete's re-selection put on
    // screen - a theme that still exists, not the deleted one.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert_ne!(
        studio.theme_name(),
        "suite-dusk",
        "the studio cannot keep wearing a deleted theme"
    );
    assert!(studio.status().starts_with("theme "), "{}", studio.status());
}
