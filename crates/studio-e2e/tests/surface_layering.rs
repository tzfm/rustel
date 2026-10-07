//! Check paint order, keyboard ownership and Esc with overlapping surfaces:
//! panes, raised sheets, prompts, dropdowns, theme editor and help.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, row_containing};

/// A score low enough in the pane that the settings sheet - whose top
/// border takes the frame's row 9 at this size - covers its row with rows
/// to spare.
const LOW_SCORE: &str = "\n\n\n\n\n\n\n$: s(\"bd\")";

/// The dialogs take one another's place: a menu row opens one sheet and
/// puts the last one away, so the screen never holds two sheets claiming
/// the same keys. The focus goes with the survivor.
#[test]
fn sheets_take_one_anothers_place() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    row_containing(&studio.rows(), " settings ");

    // The device panel replaces it outright.
    studio.chord("ctrl+p");
    assert_eq!(studio.focus(), Some(PanelKind::Devices));
    assert!(
        !studio.rows().iter().any(|row| row.contains("╭─ settings ")),
        "the settings sheet went when the devices came up:\n{}",
        studio.rows().join("\n")
    );
    row_containing(&studio.rows(), "audio out");

    // And the theme picker replaces the devices.
    studio.chord("ctrl+t");
    assert_eq!(studio.focus(), Some(PanelKind::Theme));
    assert!(
        !studio.rows().iter().any(|row| row.contains("audio out")),
        "the devices went when the themes came up:\n{}",
        studio.rows().join("\n")
    );
    assert_eq!(
        studio.status(),
        "theme - type to search · ←/→/↑/↓ preview · Enter keep · Esc back"
    );

    // Esc from the survivor hands the keys back; nothing resurfaces.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert!(
        !studio.rows().iter().any(|row| row.contains("╭─ settings ")),
        "the replaced sheet stays replaced"
    );
}

/// The docked set panel stays under a sheet and comes back whole. The
/// settings sheet takes the full width and paints over the sidebar's lower
/// rows, because it was raised after the panel; Esc gives the rows back.
/// The test writes tapes first so the fold sits low in the sidebar, under
/// the sheet's top rows.
#[test]
fn the_set_panel_stays_under_a_sheet_and_returns() {
    let mut studio = hermetic();
    // Real tapes, written by the recorder, so the panel lists them the way
    // it lists any set's sessions: enough of them that the opened fold's
    // rows run down past the sheet's top rows.
    for hour in [
        "09-00-00", "10-00-00", "11-00-00", "12-00-00", "13-00-00", "14-00-00", "15-00-00",
        "16-00-00", "17-00-00", "18-00-00",
    ] {
        studio.write_tape(&format!("2026-09-05T{hour}"), [(0.0, "$: s(\"bd\")")]);
    }

    studio.chord("ctrl+b");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    studio.press(KeyCode::End, KeyModifiers::NONE);
    let fold_row = row_containing(&studio.rows(), "sessions (10)");
    assert!(
        fold_row >= 4,
        "the fold sits where tapes can stack under it: row {fold_row}"
    );
    // Open the fold: the tape rows run down from it, into the rows the
    // sheet will take.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let last_tape = studio
        .rows()
        .iter()
        .rposition(|row| row.contains("Sep 05"))
        .expect("tape rows are shown");
    assert!(
        last_tape >= 12,
        "the tapes run low enough to be under the sheet's interior: row {last_tape}"
    );

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    let rows = studio.rows();
    let sheet_top = rows
        .iter()
        .position(|row| row.contains(" settings "))
        .expect("the sheet is up");
    // The sheet is as tall as its rows and sits on the bottom of the frame,
    // so it paints over the sidebar's lower rows. Every tape row at or
    // below the sheet's top edge is hidden: the panel is under the sheet,
    // not closed by it.
    assert!(
        last_tape >= sheet_top,
        "the fold ran under the sheet, so there was something to cover: \
         tapes to row {last_tape}, sheet from row {sheet_top}"
    );
    let covered = rows
        .iter()
        .skip(sheet_top)
        .filter(|row| row.contains("Sep 05"))
        .count();
    assert_eq!(
        covered,
        0,
        "the sheet covered the panel's tape rows under it:\n{}",
        rows.join("\n")
    );
    row_containing(&rows, " settings ");

    // Esc puts the sheet away. A sheet never keeps the keys and never
    // passes them down a ladder: they go to the score, while the panel
    // itself - not a dialog - is still open, whole, on screen.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "the keys came back to the score");
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("sessions (10)")),
        "the panel survived the sheet that covered it"
    );
    let restored = studio
        .rows()
        .iter()
        .filter(|row| row.contains("Sep 05"))
        .count();
    assert_eq!(restored, 10, "every tape row is back after the sheet went");
}

/// A sheet raised after the reference column paints over it and takes Esc.
/// Once focused, a modal sheet also owns panel chords until it closes.
#[test]
fn the_front_sheet_owns_esc_and_panel_chords() {
    // Reference first, sheet second: the sheet is in front, so Esc is
    // the sheet's and the reference stays.
    let mut studio = hermetic();
    studio.chord("ctrl+f");
    assert_eq!(studio.focus(), Some(PanelKind::Reference));
    row_containing(&studio.rows(), "search:");

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "the sheet took the Esc");
    assert_eq!(
        studio.focus(),
        None,
        "the reference, under, did not take the keys"
    );
    row_containing(&studio.rows(), "search:");

    // A focused modal sheet owns panel chords too: Reference cannot leap
    // above it. Esc therefore closes Settings itself.
    drop(studio);
    let mut studio = hermetic();
    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    studio.chord("ctrl+f");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert!(
        !studio.rows().iter().any(|row| row.contains("╭─ settings ")),
        "Esc put the focused sheet away:\n{}",
        studio.rows().join("\n")
    );
}

/// A sheet paints over the score's own decorations where it covers them:
/// the orphan bracket's error colour must not show through the sheet. This
/// is the `clear_surface` contract: a surface resets the cells it covers
/// and does not only patch their styles.
#[test]
fn a_sheet_erases_the_score_decorations_it_covers() {
    let mut studio = hermetic();

    // The opener on its own line, low enough to be under the sheet, with
    // the caret on it: an orphan, lit in the error colour.
    studio.set_score("\n\n\n\n\n\n\n$: s(\"bd\"");
    let head = studio.score().find("(\"bd\"").expect("the call");
    studio.set_caret(head);
    studio.settle();

    let rows = studio.rows();
    let row = rows
        .iter()
        .position(|text| text.contains("$: s("))
        .expect("the score's row");
    let open_x = rows[row].find("$: s(").expect("the call") as u16 + 4;
    assert!(
        row >= 10,
        "the bracket's row must sit under the sheet's interior: row {row}"
    );
    assert_eq!(
        studio.cell_underline_color(open_x, row as u16),
        Some(studio.theme_error_color()),
        "the orphan is lit before the sheet comes up"
    );

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    let covered = studio.cell_underline_color(open_x, row as u16);
    assert_ne!(
        covered,
        Some(studio.theme_error_color()),
        "the error colour leaked through the sheet at ({open_x}, {row})"
    );

    // Esc, and the decoration is lit exactly as it was.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert_eq!(
        studio.cell_underline_color(open_x, row as u16),
        Some(studio.theme_error_color()),
        "the sheet took nothing with it on the way out"
    );
}

/// Help covers everything, including the theme editor's sheet, and gives
/// it back byte for byte. Help is painted absolutely last, over toasts and
/// dropdowns alike - the one surface nothing steps in front of.
#[test]
fn help_covers_the_theme_editor_and_gives_it_back() {
    let mut studio = hermetic();

    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    assert_eq!(studio.focus(), Some(PanelKind::ThemeEditor));
    row_containing(&studio.rows(), " settings ");

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('h'), KeyModifiers::NONE); // Help, dropped
    studio.press(KeyCode::Char('k'), KeyModifiers::NONE); // Keyboard reference
    assert!(studio.help_is_open());
    // The editor's sheet showed the subtitle "a new theme - s saves it
    // under a name" on its second row; help's clear must have taken it.
    // (The sheet's ` settings ` tab title would be no proof: help's own
    // context chip echoes that word.)
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("a new theme - s saves it")),
        "help covered the editor's sheet:\n{}",
        studio.rows().join("\n")
    );
    row_containing(&studio.rows(), "keyboard help");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!studio.help_is_open());
    assert_eq!(studio.focus(), Some(PanelKind::ThemeEditor));
    row_containing(&studio.rows(), " settings ");
}

/// A set prompt raised over the other dialogs takes their place - the set
/// panel it belongs to stays - and its Esc goes back to that panel, not
/// to the score. The set panel is deliberately not a dialog: only its
/// prompt goes.
#[test]
fn a_set_prompt_replaces_the_sheets_and_keeps_the_panel() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));

    // ^⇧O opens a set: the settings sheet is put away, the prompt is the
    // one thing on top.
    studio.chord("ctrl+shift+o");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    assert!(
        !studio.rows().iter().any(|row| row.contains("╭─ settings ")),
        "the prompt replaced the sheet:\n{}",
        studio.rows().join("\n")
    );
    assert_eq!(
        studio.status(),
        "open a set - Enter opens the chosen set · Tab browses · type a path · Esc back"
    );

    // The prompt's Esc is not the score's: it closes only the prompt.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "set prompt closed");
    assert_eq!(studio.focus(), None);
    assert!(
        !studio.rows().iter().any(|row| row.contains("╭─ settings ")),
        "the dismissed sheet stays dismissed"
    );
}

/// The same prompt over the docked set panel: the panel stays through the
/// prompt's whole life - opened over it, and after the prompt's Esc the
/// panel still has the keys and its fold is still on screen.
#[test]
fn the_set_panel_survives_its_own_prompt() {
    let mut studio = hermetic();

    studio.chord("ctrl+b");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    studio.press(KeyCode::End, KeyModifiers::NONE);
    row_containing(&studio.rows(), "sessions");

    studio.chord("ctrl+shift+o");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    assert_eq!(
        studio.status(),
        "open a set - Enter opens the chosen set · Tab browses · type a path · Esc back"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "set prompt closed");
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Set),
        "the panel took the keys back, not the score"
    );
    row_containing(&studio.rows(), "sessions");
}

/// A toast over an open sheet: the confirmation is painted in the same
/// pass as the sheets, after them, so it reads on top of whatever is up -
/// and it is gone on its own soon after, leaving the sheet exactly as it
/// was.
#[test]
fn a_toast_reads_over_an_open_sheet() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));

    // The theme editor is a sheet; a sheet raised over the settings puts
    // it away. Its copy chord fires a toast while the editor is up.
    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    assert_eq!(studio.focus(), Some(PanelKind::ThemeEditor));
    studio.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
    row_containing(&studio.rows(), "theme copied - paste it anywhere");

    // The toast expires on its own clock; wait it out and pump, then the
    // editor underneath is untouched.
    std::thread::sleep(
        rustel_studio::app::harness::Studio::toast_duration()
            + std::time::Duration::from_millis(100),
    );
    studio.pump();
    studio.settle();
    assert!(
        !studio.rows().iter().any(|row| row.contains("theme copied")),
        "the toast did not outlive its welcome"
    );
}

/// The visuals docks' add sheet is a popup of its own: raised over every
/// dialog by the dock's `a`, dismissed with the dock's Esc, and gone
/// without a trace either way.
#[test]
fn the_viz_add_sheet_replaces_the_dialogs_and_esc_gives_back() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));

    // The docks have no chord: View ▸ Visuals 1 is the way in (a toggle
    // row runs its change and closes the bar itself, so the dock's own
    // keys are driven with the bar already gone).
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE); // View, dropped
    studio.press(KeyCode::Char('u'), KeyModifiers::NONE); // Visuals 1
    assert!(!studio.menu_open(), "the toggle closed the bar");
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    row_containing(&studio.rows(), " add a widget ");
    assert!(
        !studio.rows().iter().any(|row| row.contains("╭─ settings ")),
        "the dock's add sheet put the settings away:\n{}",
        studio.rows().join("\n")
    );

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
}

/// A sheet that hides the score takes the keys that would edit it. The
/// theme editor's sheet covers nearly the whole pane, so typing never
/// reaches the score (`key_would_edit_hidden_score`), and the keys return
/// when the sheet closes. The settings sheet hides only the bottom of the
/// pane and lets plain typing through; this test does not cover that.
#[test]
fn keys_never_fall_through_a_sheet_into_the_hidden_score() {
    let mut studio = hermetic();
    studio.set_score(LOW_SCORE);
    let before = studio.score();

    // The theme editor's sheet is up and holds the keyboard.
    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    assert_eq!(studio.focus(), Some(PanelKind::ThemeEditor));
    studio.type_text("$: x");
    assert_eq!(
        studio.score(),
        before,
        "the sheet swallowed the typing: {:?}",
        studio.score()
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Theme));
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    studio.type_text(" ");
    assert!(
        studio.score().contains("$: s(\"bd\") "),
        "the score takes its keys back: {:?}",
        studio.score()
    );
}

/// A dropped menu covers the score's caret. The caret is the terminal's own
/// cursor, which the terminal draws over any cell, so the view must not
/// show the caret where a dropdown is. The bar alone and a dropdown clear
/// of the caret leave it lit, and Esc restores it at the same place.
#[test]
fn a_dropped_menu_covers_the_caret() {
    let mut studio = hermetic();

    // The score's caret is lit where the editor put it, below the bar.
    let caret = studio.cursor().expect("the caret is lit");
    assert_ne!(caret.1, 0, "the caret is not on the menu bar's row");

    // The bar alone holds one row; the caret under it stays lit.
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    assert!(studio.menu_open());
    assert_eq!(
        studio.cursor(),
        Some(caret),
        "the bar alone leaves the caret lit: {:?}",
        studio.cursor()
    );

    // File's dropdown hangs from the bar's left edge - over the caret.
    studio.press(KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(
        studio.cursor().is_none(),
        "the dropdown over the caret hid it: {:?}",
        studio.cursor()
    );

    // Walking right parks the dropdown under Help, clear of the caret:
    // the caret is only hidden where a dropdown actually stands.
    for _ in 0..6 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
    }
    assert_eq!(
        studio.cursor(),
        Some(caret),
        "a dropdown clear of the caret leaves it lit: {:?}",
        studio.cursor()
    );

    // Esc hands the keys - and the caret - back where they were.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!studio.menu_open());
    assert_eq!(studio.cursor(), Some(caret));
}
