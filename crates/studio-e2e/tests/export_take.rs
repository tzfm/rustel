//! Render real WAVs into the temporary set's exports folder. Check naming,
//! completion and cancellation through the export sheet.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, row_containing};

/// How long an export may run. Generous on purpose: the MP3 flip renders
/// and encodes sixteen cycles in a debug build, and a loaded runner can
/// stretch that well past a half-minute. The wait returns the moment the
/// status moves on, so a fast render still costs nothing; only a genuinely
/// stuck export waits out the budget. (This flaked once under the full
/// serial suite's load at 30 s.)
const EXPORT_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

/// Waits until the running export has landed: the status names the take
/// ("exported …") or its failure - anything but the running or ending
/// states, which only say the render is still going.
fn wait_for_export(studio: &mut rustel_studio_e2e::Hermetic) {
    studio.pump_until(EXPORT_BUDGET, "the export finished", |studio| {
        let status = studio.status();
        !status.starts_with("exporting") && !status.starts_with("ending")
    });
}

/// Ctrl+Shift+X opens the sheet on the focused scene: one cycle count,
/// a format, and a target the musician can find without hunting.
#[test]
fn the_sheet_opens_on_the_focused_scene() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+x");
    assert_eq!(studio.focus(), Some(PanelKind::Export));
    assert_eq!(studio.status(), "export - first as written; Enter renders");
    row_containing(&studio.rows(), "16 cycles");
    row_containing(&studio.rows(), "Enter renders");

    // Esc closes without starting anything.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "export closed");
    assert_eq!(studio.focus(), None);
    assert!(
        !studio.set_directory().join("exports").exists(),
        "nothing was rendered by opening and closing the sheet"
    );
}

/// Right on the format flips WAV to MP3, and the target's extension
/// follows - the file is named for what it will be, before it is.
#[test]
fn the_format_flip_retargets_the_file() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+x");
    // Down Down: Length → First → Format (no silence fields on Cycles).
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    let row = row_containing(&studio.rows(), "mp3");
    assert!(
        studio.rows()[row].contains("mp3"),
        "the format row shows the flip: {}",
        studio.rows()[row]
    );

    // And the render lands as an .mp3-named take. (The sheet's encoder
    // is the real one; only the container's name is asserted here.)
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio.status().starts_with("exporting first"),
        "{}",
        studio.status()
    );
    wait_for_export(&mut studio);
    let status = studio.status().to_owned();
    assert!(
        status.contains(".mp3"),
        "the take lands under the flipped extension: {status}"
    );
}

/// Enter renders the scene as written: a real WAV appears in the set's
/// exports folder, the status says where, and the log remembers.
#[test]
fn enter_renders_the_scene_into_the_exports_folder() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+shift+x");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio
            .status()
            .starts_with("exporting first (16 cycles) → "),
        "{}",
        studio.status()
    );
    assert!(
        studio
            .status()
            .ends_with("ends it early and keeps the file"),
        "the status says how to stop: {}",
        studio.status()
    );

    wait_for_export(&mut studio);
    let status = studio.status().to_owned();
    assert!(status.starts_with("exported first - "), "{status}");
    assert!(
        status.contains("(0:32, ") && status.contains("s to render)"),
        "the take's length is the sixteen cycles it rendered: {status}"
    );

    // The file is really there, in the set's own exports folder.
    let exports = set.join("exports");
    let files: Vec<_> = std::fs::read_dir(&exports)
        .expect("exports directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(files.len(), 1, "one take, not a folder of attempts");
    let take = &files[0];
    assert_eq!(take.extension().and_then(|ext| ext.to_str()), Some("wav"));
    let named = take
        .file_name()
        .and_then(|name| name.to_str())
        .expect("named");
    assert!(
        named.starts_with("first-2"),
        "the scene's name leads the timestamped file: {named}"
    );
    assert!(
        std::fs::metadata(take).expect("take metadata").len() > 44,
        "more than a bare WAV header was written"
    );

    // The log remembers the take.
    studio.chord("ctrl+shift+d");
    row_containing(&studio.rows(), "export");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// Ctrl+Shift+X while the render runs ends it early: the take keeps what
/// was rendered, faded out, and is still a file a musician can open.
#[test]
fn ctrl_shift_x_ends_a_running_export_early() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+shift+x");
    // Longer than the default so the early end has something to cut:
    // First steps 16 → 20 cycles, some two seconds of render.
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let started = studio.status().to_owned();
    assert!(
        started.starts_with("exporting first (20 cycles)"),
        "{started}"
    );

    studio.chord("ctrl+shift+x");
    assert_eq!(
        studio.status(),
        "ending the export of first - fading out",
        "the status says the tail is being faded"
    );

    wait_for_export(&mut studio);
    let status = studio.status().to_owned();
    assert!(
        status.starts_with("exported first - ") && status.contains(", ended early"),
        "{status}"
    );

    let exports = set.join("exports");
    let count = std::fs::read_dir(&exports)
        .expect("exports directory")
        .count();
    assert_eq!(count, 1, "the early end kept the one file");
}

/// A prebake tab refuses to export: setup makes no sound of its own.
#[test]
fn a_prebake_refuses_to_export() {
    let mut studio = hermetic();

    // Onto the local prebake, the settings-sheet way: the row lives at the
    // bottom of the sheet, Enter opens it as a tab.
    studio.chord("ctrl+o");
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ local prebake"))
        {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        studio.scene_names().last(),
        Some(&"prebake (local)".to_owned())
    );

    studio.chord("ctrl+shift+x");
    assert_eq!(
        studio.status(),
        "prebake (local) is setup, not a score - select a scene to export"
    );
    assert_eq!(
        studio.focus(),
        None,
        "no sheet opened, so no sheet holds the keys"
    );
}
