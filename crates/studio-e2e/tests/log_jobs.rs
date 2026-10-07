//! Check log docking and the background-jobs sheet. panels_devices_log covers
//! log content, scrolling, follow mode, badges and memory readings.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, row_containing};

/// The log panel's own title row, and where it sits: `log - <where it is
/// written>` set into the panel's top border. The status line says "log"
/// too - "log - also in studio.log" when the log opens, "the log stays
/// docked" when Esc leaves it - so the needle is the title on a border
/// row, which only the panel draws.
fn log_title_row(rows: &[String]) -> Option<usize> {
    rows.iter()
        .position(|row| row.contains(" log - ") && row.contains('─'))
}

/// F9 opens the log as a sheet with the keyboard; Esc hands the keys back
/// and the sheet is gone from the frame.
#[test]
fn f9_opens_the_log_sheet_and_esc_closes_it() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(9), KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    assert!(
        studio.status().starts_with("log"),
        "opening the log says so: {}",
        studio.status()
    );
    assert!(
        log_title_row(&studio.rows()).is_some(),
        "the sheet is drawn:\n{}",
        studio.screen()
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "Esc gives the keys back");
    assert_eq!(
        log_title_row(&studio.rows()),
        None,
        "Esc closed the sheet:\n{}",
        studio.screen()
    );
}

/// Shift+F9 docks the log along the bottom and says how to move and size
/// it; `e` flips it to the top band; Esc leaves the keyboard while the dock
/// stays standing where it was put. Each claim is read off where the
/// panel's own title row is drawn, not off the status line.
#[test]
fn shift_f9_docks_the_log_and_e_moves_it() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(9), KeyModifiers::SHIFT);
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    assert_eq!(
        studio.status(),
        "log docked at the bottom - e top/bottom · -/+ height · ↑/↓/PgUp/PgDn/Home/End scroll · Esc leaves the keyboard"
    );
    let rows = studio.rows();
    let bottom = log_title_row(&rows).expect("the dock is drawn");
    assert!(
        bottom > rows.len() / 2,
        "the dock sits in the bottom half (title on row {bottom} of {}):\n{}",
        rows.len(),
        rows.join("\n")
    );

    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "log docked at the top",
        "e moved the dock to the other band"
    );
    let rows = studio.rows();
    let top = log_title_row(&rows).expect("the dock is still drawn");
    assert!(
        top < rows.len() / 2,
        "the dock sits in the top half now (title on row {top} of {}):\n{}",
        rows.len(),
        rows.join("\n")
    );

    // Esc leaves the keyboard but the dock keeps its place on the screen.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert_eq!(
        log_title_row(&studio.rows()),
        Some(top),
        "the dock stayed standing where it was:\n{}",
        studio.screen()
    );
}

/// View ▸ Background jobs raises the read-only jobs sheet over the score,
/// naming itself and closing on Esc.
#[test]
fn background_jobs_opens_and_closes() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE); // View
    studio.press(KeyCode::Char('b'), KeyModifiers::NONE); // Background jobs
    assert_eq!(studio.status(), "background jobs - Esc closes");
    // With nothing running, the sheet says so rather than an empty frame.
    row_containing(&studio.rows(), "background jobs - none running");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("background jobs - none running")),
        "Esc closed the sheet"
    );
}
