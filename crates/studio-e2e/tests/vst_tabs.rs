//! Check the two vst tabs: the plugin folders on the settings sheet, and the
//! plugin list of the reference column with the test plugins. The plugin
//! rows of the memory breakdown are here too, and the completion lists of a
//! plugin call in the editor.
//!
//! Each hermetic studio has no standard plugin folder, so no test here reads
//! the plugins of the machine. The plugins are the fixture bundle of the
//! plugin host tests, in a temporary folder: 1 effect and 1 instrument.

// The `silent` output of the engine opens only when the allocator of this
// process is the audio-callback tripwire. The callback has to allocate
// nothing, and only a binary sets a global allocator. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_runtime::vst;
use rustel_studio_e2e::{Hermetic, hermetic, row_containing};

/// The preferences are written 600 ms after a change.
const PREFS_BUDGET: Duration = Duration::from_secs(5);

/// The fixture loads on the plugin thread in a few milliseconds, and the
/// host reads the plugin folders on that thread too.
const LOAD_BUDGET: Duration = Duration::from_secs(20);

/// Waits for a row with `text`: the page shows the result of a folder read
/// at the end of the read.
fn wait_for_row(studio: &mut Hermetic, text: &str) {
    studio.pump_until(LOAD_BUDGET, text, |studio| {
        studio.rows().iter().any(|row| row.contains(text))
    });
}

/// The `vst_folders` list of `studio.json`. Empty with no key.
fn saved_folders(studio: &Hermetic) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(studio.config_home().join("studio.json")) else {
        return Vec::new();
    };
    let prefs: serde_json::Value = serde_json::from_str(&text).expect("preferences");
    prefs["vst_folders"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|folder| folder.as_str().expect("a path").to_owned())
        .collect()
}

/// `a` opens the folder prompt over the vst page. The folder goes on the
/// page, into the preferences and to the plugin host. `d` takes the folder
/// off all three, and the rescan row counts the plugins again.
#[test]
fn a_plugin_folder_added_on_the_sheet_reaches_the_host_and_can_be_removed() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(folder.path());
    let path = folder.path().display().to_string();
    let name = folder.path().file_name().unwrap().to_string_lossy();

    // The vst page is the sixth of seven tabs.
    studio.chord("ctrl+o");
    for _ in 0..5 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows[row_containing(&rows, "add folder")].contains('▸'),
        "the vst page opens on its first action:\n{}",
        rows.join("\n")
    );
    row_containing(&rows, "none yet · a adds a folder");
    row_containing(&rows, "0 plugins found");

    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "plugin folder - type a path, or Tab to browse and Enter on . · Esc back"
    );
    studio.paste(&path);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), format!("{name} added"));
    let rows = studio.rows();
    assert!(
        rows[row_containing(&rows, &format!("▸ {path}"))].contains('│'),
        "the folder is a row of the list, with the cursor on the row:\n{}",
        rows.join("\n")
    );
    wait_for_row(&mut studio, "1 plugin found");
    assert_eq!(vst::user_folders(), [folder.path()]);
    studio.pump_until(PREFS_BUDGET, "the folder was saved", |studio| {
        saved_folders(studio) == [path.as_str()]
    });

    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), format!("{name} removed"));
    let rows = studio.rows();
    row_containing(&rows, "none yet · a adds a folder");
    wait_for_row(&mut studio, "0 plugins found");
    assert!(vst::user_folders().is_empty());
    studio.pump_until(PREFS_BUDGET, "the folder left the file", |studio| {
        saved_folders(studio).is_empty()
    });

    // The cursor went up to the rescan row with the last folder.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "reading the plugin folders");
    wait_for_row(&mut studio, "0 plugins found");
}

/// Open the reference column on its vst tab: the fifth tab.
fn open_vst_tab(studio: &mut Hermetic) {
    studio.chord("ctrl+d");
    for _ in 0..4 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(studio.reference_tab(), Some("vst"));
}

/// The vst tab of the reference column lists the bundle by its name. →
/// loads the bundle with no wait: the parameters of the effect show
/// under its row, and the instrument of the bundle gets a row of its own.
/// Enter writes `.vst()` for the effect and `.vsti()` for the instrument at
/// the caret. The memory breakdown of the log then has a row for each.
#[test]
fn the_reference_tab_loads_a_plugin_and_writes_its_call() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(folder.path());
    vst::pin_standard_folders(vec![folder.path().to_path_buf()]);

    // After `$: s("bd")`, where a chain takes one more call.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    open_vst_tab(&mut studio);
    wait_for_row(&mut studio, "1 of 1 plugins");
    row_containing(&studio.rows(), "▸ Rustel Fixture");

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.pump_until(LOAD_BUDGET, "the plugin loaded", |studio| {
        studio.rows().iter().any(|row| row.contains("beatgate"))
    });
    let rows = studio.rows();
    assert!(rows[row_containing(&rows, "▾ Rustel Fixture")].contains("effect"));
    assert!(rows[row_containing(&rows, "gain  Gain")].contains("     gain"));
    row_containing(&rows, "beatgate  Beat Gate");
    assert!(rows[row_containing(&rows, "▸ Rustel Fixture Tone")].contains("instrument"));

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.reference_tab(), None, "the column closed");
    assert_eq!(studio.score(), "$: s(\"bd\").vst(\"Rustel Fixture\")\n");

    open_vst_tab(&mut studio);
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.score(),
        "$: s(\"bd\").vst(\"Rustel Fixture\").vsti(\"Rustel Fixture Tone\")\n"
    );

    // The docked log has the rows for the whole breakdown.
    studio.chord("f9");
    studio.chord("shift+f9");
    studio.settle();
    let rows = studio.rows();
    assert!(rows[row_containing(&rows, "plugins")].contains("2 loaded"));
    assert!(rows[row_containing(&rows, "Rustel Fixture · effect")].contains("in process"));
    assert!(rows[row_containing(&rows, "Rustel Fixture Tone · ")].contains("in process"));
}

/// Put `text` in the editor with the caret after its first `before`, and
/// open the completion there.
fn complete_after(studio: &mut Hermetic, text: &str, before: &str) {
    studio.set_score(text);
    let at = text.find(before).expect("the place of the caret");
    studio.set_caret(at + before.len());
    studio.chord("ctrl+space");
    studio.settle();
}

/// Ctrl+Space in a plugin call lists what the plugin host has. In the first
/// string: the plugin names, found as the host finds a name, and after the
/// load the effects for `.vst()` and the instruments for `.vsti()`. In the
/// object: the parameter keys of the plugin, with no wait for its load. In
/// the string of `preset`: the preset files of the plugin. Enter puts the
/// row in place of what is typed, including a default for a parameter key.
#[test]
fn a_completion_in_a_plugin_call_lists_names_keys_and_presets() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    let presets = folder.path().join("presets");
    let of_fixture = presets.join(rustel_vst3_fixture::NAME);
    std::fs::create_dir_all(&of_fixture).expect("preset folder");
    let preset = rustel_vst3_fixture::preset(0.5, 0.0);
    std::fs::write(of_fixture.join("Dark Bass.vstpreset"), preset).expect("preset file");
    let plugins = folder.path().join("plugins");
    rustel_vst3_fixture::install(&plugins);
    vst::host().set_preset_folder(presets);
    vst::pin_standard_folders(vec![plugins]);

    // The bundle is not loaded: its name is in the list for an effect and
    // in the list for an instrument. A part of the name in lower case
    // finds the row.
    for call in ["vst", "vsti"] {
        let text = format!("$: s(\"bd\").{call}(\"fix\")");
        complete_after(&mut studio, &text, "fix");
        assert_eq!(
            studio.status(),
            "completion - Enter puts the chosen name in place"
        );
        wait_for_row(&mut studio, "1 of 1 plugins");
        row_containing(&studio.rows(), "search: fix");
        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        studio.settle();
        assert_eq!(
            studio.score(),
            format!("$: s(\"bd\").{call}(\"Rustel Fixture\")")
        );
    }

    // The object of the call: `preset` at once, and the 3 keys of the
    // effect at the end of its load, each with its title.
    let text = "$: s(\"bd\").vst(\"rustel fixture\", { ga })";
    complete_after(&mut studio, text, "ga");
    wait_for_row(&mut studio, "1 of 4 parameter choices");
    assert!(studio.rows()[row_containing(&studio.rows(), " gain ")].contains("Gain · "));
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.score(),
        "$: s(\"bd\").vst(\"rustel fixture\", { gain: 1 })"
    );

    for (text, before, expected) in [
        (
            "$: s(\"bd\").vst(\"rustel fixture\", { ga: .2 })",
            "ga",
            "$: s(\"bd\").vst(\"rustel fixture\", { gain: .2 })",
        ),
        (
            "$: s(\"bd\").vst(\"rustel fixture\", { ga, gain: .2 })",
            "ga",
            "$: s(\"bd\").vst(\"rustel fixture\", { gain: .2 })",
        ),
        (
            "$: s(\"bd\").vst(\"rustel fixture\", { ga",
            "ga",
            "$: s(\"bd\").vst(\"rustel fixture\", { gain: 1",
        ),
        (
            "$: s(\"bd\").vst(\"rustel fixture\", { gain: .2, ga",
            ".2, ga",
            "$: s(\"bd\").vst(\"rustel fixture\", { gain: .2",
        ),
    ] {
        complete_after(&mut studio, text, before);
        wait_for_row(&mut studio, "1 of 4 parameter choices");
        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        studio.settle();
        assert_eq!(studio.score(), expected);
    }

    // The load told the effect from the instrument of the bundle.
    for (call, listed, other) in [
        ("vst", "Rustel Fixture ", "Rustel Fixture Tone"),
        ("vsti", "Rustel Fixture Tone", "Rustel Fixture  "),
    ] {
        let text = format!("$: note(\"c\").{call}(\"\")");
        complete_after(&mut studio, &text, &format!("{call}(\""));
        wait_for_row(&mut studio, "1 of 1 plugins");
        let rows = studio.rows();
        row_containing(&rows, listed);
        assert!(
            !rows.iter().any(|row| row.contains(other)),
            "{call}:\n{}",
            rows.join("\n")
        );
        studio.press(KeyCode::Esc, KeyModifiers::NONE);
    }

    // The string of `preset`: the preset files of the plugin.
    let text = "$: s(\"bd\").vst(\"rustel fixture\", { preset: \"dark\" })";
    complete_after(&mut studio, text, "dark");
    wait_for_row(&mut studio, "1 of 1 presets");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.score(),
        "$: s(\"bd\").vst(\"rustel fixture\", { preset: \"Dark Bass\" })"
    );
}

/// Switching from plugin-name completion to the vst tab keeps the plugin
/// of the call open. A parameter choice edits this call, regardless of the
/// caret's position, and never replaces a value already written there.
#[test]
fn a_plugins_parameters_are_available_throughout_its_call() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(folder.path());
    vst::pin_standard_folders(vec![folder.path().to_path_buf()]);

    let text = "$: s(\"bd\").vst(\"rustel fixture\")";
    studio.set_score(text);
    studio.set_caret(text.find("fixture").unwrap() + 3);
    studio.chord("ctrl+f");
    wait_for_row(&mut studio, "1 of 1 plugins");
    for _ in 0..4 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(studio.reference_tab(), Some("vst"));
    wait_for_row(&mut studio, "beatgate  Beat Gate");
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.score(),
        "$: s(\"bd\").vst(\"rustel fixture\", { gain: 1 })"
    );

    for (text, before, expected) in [
        (
            "$: s(\"bd\").vst(\"Rustel Fixture\", { beatgate: saw.range(0, .5) })",
            "range(0",
            "$: s(\"bd\").vst(\"Rustel Fixture\", { gain: 1, beatgate: saw.range(0, .5) })",
        ),
        (
            "$: s(\"bd\").vst(\"Rustel Fixture\", { gain: .2, beatgate: 0 })",
            "gain: .2",
            "$: s(\"bd\").vst(\"Rustel Fixture\", { gain: .2, beatgate: 0 })",
        ),
        (
            "$: s(\"bd\").vst(\"Rustel Fixture\", {})",
            ".vs",
            "$: s(\"bd\").vst(\"Rustel Fixture\", { gain: 1 })",
        ),
    ] {
        studio.set_score(text);
        studio.set_caret(text.find(before).unwrap() + before.len());
        studio.chord("ctrl+f");
        assert_eq!(studio.reference_tab(), Some("vst"), "{text}");
        wait_for_row(&mut studio, "beatgate  Beat Gate");
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        studio.settle();
        assert_eq!(studio.score(), expected);
    }

    // The same context follows an instrument call. The fixture instrument
    // has no parameters, but its own row opens instead of the effect's.
    let text = "$: note(\"c\").vsti(\"Rustel Fixture Tone\", {})";
    studio.set_score(text);
    studio.set_caret(text.find(".vs").unwrap() + 3);
    studio.chord("ctrl+f");
    assert_eq!(studio.reference_tab(), Some("vst"));
    wait_for_row(&mut studio, "▾ Rustel Fixture Tone");
    assert!(studio.rows().iter().all(|row| !row.contains("beatgate")));
}
