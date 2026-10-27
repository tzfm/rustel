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
use rustel_studio_e2e::{PanelKind, hermetic, loud_wav_bytes, row_containing};

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

/// The sheet stays open over the bounce it starts, as a DAW's render
/// window: the row says how far with a percent, and the hint names the
/// way out. Esc closes the window, and the bounce runs on in the header
/// until it lands.
#[test]
fn the_sheet_shows_the_render_over_its_bounce() {
    let mut studio = hermetic();

    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+shift+x");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    // The first frame can predate the worker's first write, so the percent
    // arrives with the render's own first progress.
    studio.pump_until(
        EXPORT_BUDGET,
        "the render row showed its percent",
        |studio| {
            let rows = studio.rows();
            rows.iter()
                .any(|row| row.contains("rendering first") && row.contains('%'))
        },
    );
    let rows = studio.rows();
    row_containing(&rows, "ends it early and keeps the file");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        studio.status().contains("runs on"),
        "the status says where the bounce went: {}",
        studio.status()
    );
    row_containing(&studio.rows(), "⇣ first");

    studio.pump_until(EXPORT_BUDGET, "the export landed", |studio| {
        studio.status().starts_with("exported")
    });
    assert!(
        studio.status().starts_with("exported first - "),
        "{}",
        studio.status()
    );
}

/// Stems from the sheet: beside the take a `<name> stems` folder holds the
/// same bounce as `main.wav` plus one wav a sounding orbit, and the status
/// names the folder, because the take itself is inside it.
#[test]
fn stems_write_the_main_and_one_wav_a_sounding_orbit() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.set_score("$: s(\"bd\").orbit(0)\n$: s(\"bd\").orbit(1)");
    studio.chord("ctrl+shift+x");
    // Down Down Down Down: Length → First → Format → Limiter → Stems.
    for _ in 0..4 {
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    let row = row_containing(&studio.rows(), "stems");
    assert!(
        studio.rows()[row].contains("on"),
        "the stems choice turned on: {}",
        studio.rows()[row]
    );
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio
            .status()
            .starts_with("exporting first (16 cycles) → "),
        "{}",
        studio.status()
    );

    wait_for_export(&mut studio);
    let status = studio.status().to_owned();
    assert!(status.starts_with("exported first - "), "{status}");
    assert!(
        status.contains("stems"),
        "the status names the stems folder: {status}"
    );

    // The folder sits beside where the one file would have landed, holding
    // the main bounce and one wav an orbit that sounded.
    let exports = set.join("exports");
    let mut stems_folders: Vec<_> = std::fs::read_dir(&exports)
        .expect("exports directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(stems_folders.len(), 1, "one stems folder, nothing else");
    let folder = stems_folders.remove(0);
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .expect("folder named");
    assert!(
        folder_name.starts_with("first-") && folder_name.ends_with(" stems"),
        "named for the take: {folder_name}"
    );
    let mut names: Vec<_> = std::fs::read_dir(&folder)
        .expect("stems folder")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["1.wav", "2.wav", "main.wav"],
        "one a sounding orbit"
    );
    for name in &names {
        assert!(
            std::fs::metadata(folder.join(name))
                .expect("stem metadata")
                .len()
                > 44,
            "{name} carries more than a bare WAV header"
        );
    }
}

/// Samples from outside the default library sound in the bounce: a folder
/// imported as a sample source, then the set's own folder.
#[test]
fn imported_and_set_folder_samples_sound_in_the_export() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();
    let imported = tempfile::tempdir().expect("imported folder");
    for bank in [imported.path().join("pack"), set.join("kit")] {
        std::fs::create_dir_all(&bank).expect("bank folder");
        std::fs::write(bank.join("hit.wav"), loud_wav_bytes()).expect("sample");
    }
    studio.paste(&imported.path().display().to_string());
    studio.pump_until(EXPORT_BUDGET, "the folder imported", |studio| {
        studio.status().contains("sound(s) imported")
    });
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    studio.set_score("$: s(\"pack kit\")");
    studio.chord("ctrl+shift+x");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    wait_for_export(&mut studio);
    let status = studio.status().to_owned();
    assert!(status.starts_with("exported first - "), "{status}");

    let takes: Vec<_> = std::fs::read_dir(set.join("exports"))
        .expect("exports directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(takes.len(), 1, "one take: {takes:?}");
    let pcm = rustel_runtime::test_support::read_pcm16_stereo_wav(&takes[0]).expect("WAV");
    // Sixteen cycles at 0.5 cps: `pack` starts each two-second cycle and
    // `kit` its second half.
    let second = pcm.len() / 32;
    let peak = |from: usize| {
        pcm[from * second..(from + 1) * second]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    };
    assert!(peak(0) > 0.1, "the imported pack sounds: {}", peak(0));
    assert!(peak(1) > 0.1, "the set's kit sounds: {}", peak(1));
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

/// The bounce runs the setups before the score, as play does: a name the
/// set's prebake defines is there for the export instead of a refusal.
#[test]
fn a_prebakes_names_reach_the_export() {
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
    studio.set_score("globalThis.half = () => 0.5");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(studio.status(), "prebake (local) applied");

    studio.chord("ctrl+w");
    studio.set_score("$: s('sine').gain(half())");
    studio.chord("ctrl+shift+x");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        studio.status().starts_with("exporting first"),
        "{}",
        studio.status()
    );
    wait_for_export(&mut studio);
    assert!(
        studio.status().starts_with("exported first"),
        "the setup ran before the bounce: {}",
        studio.status()
    );
    let set = studio.set_directory().join("exports");
    assert_eq!(
        std::fs::read_dir(&set).expect("exports directory").count(),
        1,
        "the bounce landed as one file"
    );
}
