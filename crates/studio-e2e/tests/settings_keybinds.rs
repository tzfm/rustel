//! Check terminal-profile defaults, binding conflicts, clearing and reset.
//! Mapping tests arm learn mode without sending physical hardware input.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic};

/// Walk the sheet's rows by label until the given one is selected. The
/// Keybinds page has a row per action, so the walk is longer than a
/// settings page - sixty steps covers the table twice over.
fn select_row(studio: &mut Hermetic, marker: &str) {
    for _ in 0..60 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        studio.pump();
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// The row that carries this label, as drawn.
fn row_with(studio: &mut Hermetic, label: &str) -> String {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains(label))
        .unwrap_or_else(|| panic!("no row reads {label:?}:\n{}", studio.rows().join("\n")))
}

/// Find Keybinds from either remembered settings tab.
fn open_keybinds(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    for _ in 0..7 {
        if studio.rows().iter().any(|row| row.contains("▸ Terminal")) {
            break;
        }
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    assert!(
        row_with(studio, "Terminal").contains("automatic"),
        "the Keybinds page opens on the terminal-profile row:\n{}",
        studio.rows().join("\n")
    );
}

/// Open the sheet on its Mapping page - the third of seven tabs.
fn open_mapping(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("not assigned")),
        "the Mapping page opens on its unbound slots:\n{}",
        studio.rows().join("\n")
    );
}

/// The terminal-profile row is the first row: it cycles the studio's own
/// shortcut profiles - every default re-resolved at once - and Delete
/// hands it back to `automatic`.
#[test]
fn the_terminal_profile_row_cycles_and_returns_to_automatic() {
    let mut studio = hermetic();
    open_keybinds(&mut studio);
    assert!(
        row_with(&mut studio, "Terminal").contains("automatic (rustel-e2e)"),
        "the row reads the profile the fixture's terminal got: {}",
        row_with(&mut studio, "Terminal")
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    let status = studio.status().to_owned();
    assert!(
        status.starts_with("shortcut profile: ") && status.ends_with("- custom bindings kept"),
        "cycling the profile says what was kept: {status}"
    );
    assert!(
        !row_with(&mut studio, "Terminal").contains("automatic"),
        "and the row now names a terminal: {}",
        row_with(&mut studio, "Terminal")
    );

    studio.press(KeyCode::Delete, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "shortcut profile: automatic (rustel-e2e) - custom bindings kept",
        "Delete is the road back to automatic"
    );
    assert!(
        row_with(&mut studio, "Terminal").contains("automatic (rustel-e2e)"),
        "the row reads automatic again: {}",
        row_with(&mut studio, "Terminal")
    );
}

/// The values chord and the docs chord do different things at the caret, so
/// each has a row. The values row also names Ctrl+Space.
#[test]
fn the_values_chord_and_the_docs_chord_have_a_row_each() {
    let mut studio = hermetic();
    open_keybinds(&mut studio);
    select_row(&mut studio, "▸ docs for the function");
    let values = row_with(&mut studio, "argument values, reference");
    assert!(
        values.contains("^F") && values.contains("also ^Space"),
        "the values row names Ctrl+F and Ctrl+Space: {values}"
    );
    let docs = row_with(&mut studio, "docs for the function");
    assert!(
        docs.contains("^D") && !docs.contains("also"),
        "the docs row names Ctrl+D alone: {docs}"
    );
}

/// A learn binds a free chord directly. A typing key and Enter are refused
/// with a status message, never bound silently. Delete then restores the
/// row's default, and the learned chord is the chord that fires.
#[test]
fn a_keybind_learns_a_free_chord_and_delete_restores_the_default() {
    let mut studio = hermetic();
    open_keybinds(&mut studio);
    select_row(&mut studio, "▸ piano mode");
    assert!(
        row_with(&mut studio, "piano mode").contains("F12"),
        "setup: the piano wears its shipped F12: {}",
        row_with(&mut studio, "piano mode")
    );

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "press the chord - Esc calls it off, and the sheet's own keys stay the sheet's"
    );

    // A bare letter is the score's; the learn says so and stays armed.
    studio.press(KeyCode::Char('q'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "Q is a typing key - hold ctrl to make it a shortcut",
        "the refusal is said, not silent"
    );

    // Enter arms and confirms; learned, it could never be worked again.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "Enter is reserved - it can't be a shortcut"
    );

    // A key nothing holds is taken outright.
    studio.press(KeyCode::F(13), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "piano mode · F13");
    assert!(
        row_with(&mut studio, "piano mode").contains("F13"),
        "the row wears what was learned: {}",
        row_with(&mut studio, "piano mode")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(13), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("PIANO")),
        "and the learned chord is the chord that fires:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    open_keybinds(&mut studio);
    select_row(&mut studio, "▸ piano mode");
    studio.press(KeyCode::Delete, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "piano mode · back to the default");
    assert!(
        row_with(&mut studio, "piano mode").contains("F12"),
        "the default is back on the row: {}",
        row_with(&mut studio, "piano mode")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("PIANO")),
        "and F12 is the piano's again:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
}

/// A chord that another action holds is asked about, not taken: the row
/// keeps the pending chord, Esc changes nothing, and Enter takes the chord
/// and unbinds the other action, which can be restored the same way.
#[test]
fn learning_a_taken_chord_asks_and_enter_takes_it() {
    let mut studio = hermetic();
    open_keybinds(&mut studio);

    select_row(&mut studio, "▸ background jobs");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "piano mode is already bound to F12 - Enter to rebind it here, Esc to keep it"
    );

    // Esc keeps it: the piano still wears F12 afterwards.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "learn cancelled");
    assert!(
        row_with(&mut studio, "piano mode").contains("F12"),
        "the kept chord is still the piano's: {}",
        row_with(&mut studio, "piano mode")
    );

    // The same ask, answered: jobs takes F12 and the piano goes unbound.
    select_row(&mut studio, "▸ background jobs");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "background jobs is now F12 - piano mode was unbound"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        !studio.rows().iter().any(|row| row.contains("PIANO")),
        "F12 no longer opens the piano:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    // Restore both: F12 is jobs' now, so the piano's learn goes through
    // the same ask - and taking it unbinds jobs, which Delete then puts
    // back to shipping unbound.
    open_keybinds(&mut studio);
    select_row(&mut studio, "▸ piano mode");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "background jobs is already bound to F12 - Enter to rebind it here, Esc to keep it"
    );
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "piano mode is now F12 - background jobs was unbound"
    );

    select_row(&mut studio, "▸ background jobs");
    studio.press(KeyCode::Delete, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "background jobs · back to the default");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("PIANO")),
        "and the keyboard is back the way it shipped:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
}

/// Resetting every shortcut is a two-step: the ask names the profile,
/// Esc cancels it without touching anything, and the second Enter is the
/// only thing that resets - a learned chord comes home to its default.
#[test]
fn reset_all_shortcuts_asks_twice_and_cancels_cleanly() {
    let mut studio = hermetic();
    open_keybinds(&mut studio);

    // Give the reset something to undo.
    select_row(&mut studio, "▸ piano mode");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(13), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "piano mode · F13", "setup: a custom chord");

    select_row(&mut studio, "▸ Reset all shortcuts");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "reset all shortcuts for automatic (rustel-e2e) - Enter confirms · Esc cancels"
    );
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("Reset all to automatic (rustel-e2e) defaults? Enter: confirm")),
        "and the page itself asks:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "shortcut reset cancelled");
    select_row(&mut studio, "▸ piano mode");
    assert!(
        row_with(&mut studio, "piano mode").contains("F13"),
        "the cancelled reset changed nothing: {}",
        row_with(&mut studio, "piano mode")
    );

    select_row(&mut studio, "▸ Reset all shortcuts");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.status().contains("Enter confirms · Esc cancels"),
        "the ask is back: {}",
        studio.status()
    );
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "shortcuts reset - automatic (rustel-e2e)");
    select_row(&mut studio, "▸ piano mode");
    assert!(
        row_with(&mut studio, "piano mode").contains("F12"),
        "and the learned chord came home: {}",
        row_with(&mut studio, "piano mode")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::F(13), KeyModifiers::NONE);
    studio.settle();
    assert!(
        !studio.rows().iter().any(|row| row.contains("PIANO")),
        "F13 is nobody's again:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("PIANO")),
        "F12 is the piano's:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
}

/// The Mapping page walks its twelve slots in three columns and every
/// slot answers for itself: Enter arms a learn for hardware that may
/// never arrive, Space on an unbound slot says so rather than pretending,
/// `t` steps the takeover mode, and the status names the slot the arrows
/// moved to.
#[test]
fn the_mapping_page_walks_its_slots_and_arms_a_learn() {
    let mut studio = hermetic();
    open_mapping(&mut studio);

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "slot 1: move the knob, fader or stick that should drive it - Esc cancels"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "learn cancelled",
        "the armed learn is called off, and the sheet kept"
    );

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "slot 1 is not bound to anything",
        "unbinding nothing says so"
    );

    studio.press(KeyCode::Char('t'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.status().starts_with("slot 1 · "),
        "the takeover mode is the slot's own and is named: {}",
        studio.status()
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "slot 2 is not bound to anything",
        "Right stepped the selection a slot along"
    );

    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "slot 5 is not bound to anything",
        "and Down steps a row of the three-column grid"
    );
}
