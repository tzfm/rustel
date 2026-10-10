//! Open keyboard help through F1, h, k when the menu bar is visible. Check the
//! context chip, NOW summary and return of focus after Esc.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio::app::harness::Studio;
use rustel_studio_e2e::{
    Hermetic, PanelKind, glyph_column, hermetic, hermetic_with_live_slider, row_containing,
};

/// Who has the keys: the panel that holds them, and whether a replay's
/// timeline does.
#[derive(Debug, PartialEq)]
struct KeyHolder {
    panel: Option<PanelKind>,
    timeline: bool,
}

impl KeyHolder {
    fn of(studio: &Studio) -> Self {
        Self {
            panel: studio.focus(),
            timeline: studio.timeline_focused(),
        }
    }
}

/// Open help the way the menu does - F1, Help, Keyboard reference - check
/// the page it opened on, and close it again. The menu is the only door
/// that works over a panel: F1 opens the bar, and the bar's Help menu
/// chooses the reference.
///
/// The page is held by the overlay's title, its context chip - ` chip ` -
/// and a fragment of its NOW line, what the surface does with a key; Esc
/// must then hand the keys back to exactly what held them.
fn assert_help_page(studio: &mut Studio, chip: &str, summary: &str) {
    let holder = KeyHolder::of(studio);
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('h'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('k'), KeyModifiers::NONE);
    assert!(studio.help_is_open(), "help opened over {chip:?}");
    let rows = studio.render();
    row_containing(&rows, " keyboard help ");
    row_containing(&rows, &format!(" {chip} "));
    row_containing(&rows, summary);

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!studio.help_is_open(), "Esc closed help over {chip:?}");
    assert_eq!(
        KeyHolder::of(studio),
        holder,
        "Esc handed the keys back to what held them under {chip:?}"
    );
}

/// With nothing raised, help opens on the editor page: the modeless source
/// editor, its summary, and the keys go back to the score.
#[test]
fn help_names_the_editor_by_default() {
    let mut studio = hermetic();
    assert_help_page(
        &mut studio,
        "editor",
        "Type normally; this is a modeless source editor.",
    );
    assert_eq!(studio.focus(), None, "the keys went back to the score");
}

/// The settings sheet has seven pages, and help follows the one being shown:
/// Settings, then Sources (imports and packs), then Reference (switches,
/// like Settings), then About (the terminal).
#[test]
fn help_follows_the_settings_pages() {
    let mut studio = hermetic();
    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    assert_help_page(
        &mut studio,
        "settings",
        "Settings change live and are remembered when they change.",
    );

    // Tab walks the pages until Sources, whose first row is its cache
    // switch - found by what the page shows, not by counting tabs.
    for _ in 0..6 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("fetch imports"))
        {
            break;
        }
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_help_page(
        &mut studio,
        "settings · samples",
        "Fetch imports, cache all remote packs, refresh their lists",
    );

    // The page after Sources is Reference, whose rows are switches.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_help_page(
        &mut studio,
        "settings",
        "Settings change live and are remembered when they change.",
    );

    // The page after Reference is About.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_help_page(
        &mut studio,
        "settings · about",
        "This page describes the terminal",
    );
}

/// A panel's door, the panel it gives the keys to, and the help page's
/// chip and NOW-line fragment for it.
type PanelPage = (fn(&mut Hermetic), PanelKind, &'static str, &'static str);

/// Each panel names itself on the help page: the log, the devices
/// browser, the set panel, the reference column, the mixer desk and the
/// theme picker. A panel the eye is on must not send the ear to the score.
#[test]
fn help_names_the_docked_panels() {
    let mut studio = hermetic();
    let panels: [PanelPage; 6] = [
        (
            |studio| studio.press(KeyCode::F(9), KeyModifiers::NONE),
            PanelKind::Log,
            "log",
            "The newest studio messages are at the bottom of the log.",
        ),
        (
            |studio| studio.chord("ctrl+p"),
            PanelKind::Devices,
            "devices",
            "Select audio input/output hardware or copy a MIDI port call.",
        ),
        (
            |studio| studio.chord("ctrl+b"),
            PanelKind::Set,
            "set panel",
            "The set's folder: its scores",
        ),
        (
            |studio| studio.chord("ctrl+f"),
            PanelKind::Reference,
            "reference",
            "Search, browse, preview, and insert from the reference column.",
        ),
        (
            |studio| studio.press(KeyCode::F(4), KeyModifiers::NONE),
            PanelKind::Mixer,
            "mixer",
            "The desk: a strip each for the audio input",
        ),
        (
            |studio| studio.chord("ctrl+t"),
            PanelKind::Theme,
            "theme picker",
            "Moving the selection previews each theme",
        ),
    ];
    for (open, panel, chip, summary) in panels {
        open(&mut studio);
        assert_eq!(studio.focus(), Some(panel), "{chip} took the keys");
        assert_help_page(&mut studio, chip, summary);
        studio.press(KeyCode::Esc, KeyModifiers::NONE);
    }
}

/// The export sheet and the theme editor's form are sheets, not panels, and
/// each has its own page. The theme editor's page names the tab it is on.
#[test]
fn help_names_the_sheets() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+x");
    assert_eq!(studio.focus(), Some(PanelKind::Export));
    assert_help_page(
        &mut studio,
        "export",
        "Choose a length and format for the focused scene's render.",
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // The picker's primary opens the editor on its form tab.
    studio.chord("ctrl+t");
    studio.chord("ctrl+n");
    assert_eq!(studio.focus(), Some(PanelKind::ThemeEditor));
    assert_help_page(
        &mut studio,
        "theme editor · form",
        "Every change applies live; nothing is kept until saved.",
    );
}

/// The visuals dock names itself, and so do the scene strip's two states:
/// renaming, and a learn in flight - the learn reached through a virtual
/// controller the harness plugs in, no hardware on the runner.
#[test]
fn help_names_the_visuals_dock_and_the_strip_states() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::SHIFT);
    assert_eq!(studio.focus(), Some(PanelKind::Viz));
    assert_help_page(
        &mut studio,
        "visuals panel",
        "Widgets that move with the music",
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    studio.chord("ctrl+r");
    assert_help_page(
        &mut studio,
        "scene rename",
        "The scene name is being edited; the score stays untouched.",
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // The learn state, ^L with a controller plugged in: its page tells the
    // musician what the studio is waiting for.
    studio.attach_midi_controller("e2e pads");
    studio.chord("ctrl+l");
    assert_help_page(
        &mut studio,
        "scene pad learn",
        "The current scene is waiting for a MIDI pad press.",
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "learn cancelled");
}

/// An armed slider is a surface of its own: help says the plain arrows are
/// the control's while it is armed.
#[test]
fn help_names_an_armed_slider() {
    let mut studio = hermetic_with_live_slider();
    let (knob, y) = glyph_column(&studio.rows(), '█');
    studio.click(knob, y);

    assert_help_page(
        &mut studio,
        "live slider",
        "The last pointer-touched slider owns the plain left/right arrows.",
    );
}

/// A replay's timeline has the keyboard after Alt+→, and its help page names
/// the arrows that choose the block the editor shows.
#[test]
fn help_names_the_timeline() {
    let mut studio = hermetic();
    studio.write_tape(
        "2026-09-05T12-09-48",
        [
            (0.0, "$: s(\"bd\")"),
            (4.0, "$: s(\"sd\")"),
            (8.0, "$: s(\"hh\")"),
        ],
    );
    studio.open_newest_tape();
    // Alt+→ both walks a block and gives the timeline the keyboard.
    studio.press(KeyCode::Right, KeyModifiers::ALT);
    assert!(studio.timeline_focused(), "the timeline took the keyboard");

    assert_help_page(
        &mut studio,
        "timeline",
        "its arrows choose the block shown in the editor",
    );
}
