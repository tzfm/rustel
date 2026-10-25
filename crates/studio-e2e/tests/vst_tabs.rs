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

/// The list `key` of the file `file` in the config home: `vst_folders` of
/// `studio.json`, or `plugin_audio_thread` of `rustel.json`. Empty with no
/// key.
fn saved_list(studio: &Hermetic, file: &str, key: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(studio.config_home().join(file)) else {
        return Vec::new();
    };
    let saved: serde_json::Value = serde_json::from_str(&text).expect("settings");
    saved[key]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| entry.as_str().expect("a text").to_owned())
        .collect()
}

/// `a` opens the folder prompt over the vst page. The folder goes on the
/// page, into the preferences and to the plugin host. Space on the plugin
/// of the folder turns its switch `audio thread only` on and off, in the
/// host and in `rustel.json`. The row takes the switch after the load of
/// its plugin. Before, it has the bundle name and takes no switch. A fault
/// of the file gives a line and changes nothing. `d` takes the folder off
/// the page, the preferences and the host, and the rescan row counts the
/// plugins again.
#[test]
fn a_plugin_folder_added_on_the_sheet_reaches_the_host_and_can_be_removed() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(folder.path());
    let path = folder.path().display().to_string();
    let name = folder.path().file_name().unwrap().to_string_lossy();

    // The vst page is the sixth tab.
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
        rows.iter().any(|row| {
            row.contains('▸') && row.contains(name.as_ref()) && row.contains('│')
        }),
        "the folder is a row of the list, with the cursor on the row:\n{}",
        rows.join("\n")
    );
    wait_for_row(&mut studio, "1 plugin found");
    assert_eq!(vst::user_folders(), [folder.path()]);
    studio.pump_until(PREFS_BUDGET, "the folder was saved", |studio| {
        saved_list(studio, "studio.json", "vst_folders") == [path.as_str()]
    });

    // The plugin row is under the folder. `rustel play` reads the switch
    // in `rustel.json` too.
    let plugin = rustel_vst3_fixture::NAME;
    let switched = || {
        let plugins = vst::host().plugins();
        let row = plugins.iter().find(|row| row.name == plugin);
        row.expect("the plugin of the folder").audio_thread
    };
    let saved = |studio: &Hermetic| saved_list(studio, "rustel.json", "plugin_audio_thread");
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    // Before its load, the row has the name of the bundle and takes no
    // switch. The load gives the bundle its 2 plugins.
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        format!("{plugin} · no switch before the load or the test of the plugin")
    );
    assert!(saved(&studio).is_empty());
    vst::host().resolve(plugin, true);
    wait_for_row(&mut studio, "2 plugins found");
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        format!("{plugin} · audio thread only on · for the copies that load from now")
    );
    row_containing(
        &studio.rows(),
        &format!("{plugin} · ready · audio thread only"),
    );
    assert!(switched());
    assert_eq!(saved(&studio), [plugin]);
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert!(studio.status().contains("audio thread only off"));
    assert!(!switched());
    assert!(saved(&studio).is_empty());
    // A file with a fault stays as it is, and so does the switch.
    let settings = studio.config_home().join("rustel.json");
    let faulty = r#"{"plugin_audio_thread": "OTT"}"#;
    std::fs::write(&settings, faulty).expect("settings");
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        format!("{plugin}: plugin_audio_thread must be a list of plugin names")
    );
    assert!(!switched());
    assert_eq!(
        std::fs::read_to_string(&settings).expect("settings"),
        faulty
    );
    studio.press(KeyCode::Up, KeyModifiers::NONE);

    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), format!("{name} removed"));
    let rows = studio.rows();
    row_containing(&rows, "none yet · a adds a folder");
    wait_for_row(&mut studio, "0 plugins found");
    assert!(vst::user_folders().is_empty());
    studio.pump_until(PREFS_BUDGET, "the folder left the file", |studio| {
        saved_list(studio, "studio.json", "vst_folders").is_empty()
    });

    // The cursor went up to the rescan row with the last folder.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    // No folder is left: the read ends in no time, and the line that says
    // it runs may already say the count it ended with.
    assert!(
        studio.status() == "reading the plugin folders" || studio.status() == "0 plugins found",
        "{}",
        studio.status()
    );
    wait_for_row(&mut studio, "0 plugins found");
}

/// `/` filters the plugin rows of the vst page by their letters. While
/// the letters build the filter, the keys the page binds type into it -
/// `a` opens no folder prompt - and Enter keeps it standing while the
/// arrows and Enter act on the rows it kept. Backspace takes the letters
/// off, and an empty filter goes with them.
#[test]
fn a_slash_filters_the_plugin_rows_of_the_settings_page() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(folder.path());
    let path = folder.path().display().to_string();

    // The vst page is the sixth tab, with the fixture's two plugins.
    studio.chord("ctrl+o");
    for _ in 0..5 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    studio.settle();
    studio.paste(&path);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    vst::host().resolve(rustel_vst3_fixture::NAME, true);
    wait_for_row(&mut studio, "2 plugins found");
    let plugins = |studio: &mut Hermetic| {
        studio
            .rows()
            .into_iter()
            .filter(|row| row.contains("Rustel Fixture"))
            .count()
    };

    // `/` starts the filter. `a` types into it - no folder prompt opens -
    // and no plugin name holds an `a`, so the filter says so.
    studio.press(KeyCode::Char('/'), KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "filter: a▏ · no plugins match");
    assert!(
        !studio.status().contains("plugin folder"),
        "`a` typed into the filter, not the page: {}",
        studio.status()
    );

    // Backspace takes the `a` off, and `tone` keeps the Tone plugin alone.
    studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    for letter in ['t', 'o', 'n', 'e'] {
        studio.press(KeyCode::Char(letter), KeyModifiers::NONE);
    }
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "filter: tone▏");
    assert_eq!(
        plugins(&mut studio),
        1,
        "the filter keeps the Tone plugin alone:\n{}",
        rows.join("\n")
    );

    // Enter keeps the filter standing, without its caret.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    row_containing(&studio.rows(), "filter: tone");
    assert_eq!(plugins(&mut studio), 1, "the standing filter holds");

    // `/` takes the standing filter back up, and Backspace takes its
    // letters off - an empty filter goes with them: both plugins are
    // rows again.
    studio.press(KeyCode::Char('/'), KeyModifiers::NONE);
    studio.settle();
    for _ in 0..5 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.settle();
    assert_eq!(plugins(&mut studio), 2, "the empty filter keeps all");
    assert!(
        !studio.rows().iter().any(|row| row.contains("filter:")),
        "the echo line went with the filter"
    );
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
    // finds the row. The pick of the first name opens the presets of the
    // plugin, which loads the bundle: the instrument list then has the
    // instrument of the bundle, and the effect list the effect.
    for (call, plugin) in [("vst", "Rustel Fixture"), ("vsti", "Rustel Fixture Tone")] {
        let text = format!("$: s(\"bd\").{call}(\"fix\")");
        complete_after(&mut studio, &text, "fix");
        assert_eq!(
            studio.status(),
            "completion - Enter puts the chosen name in place"
        );
        wait_for_row(&mut studio, "1 of 1 plugins");
        row_containing(&studio.rows(), "search: fix");
        if call == "vsti" {
            wait_for_row(&mut studio, "Rustel Fixture Tone");
        }
        studio.press(KeyCode::Enter, KeyModifiers::NONE);
        studio.settle();
        assert_eq!(studio.score(), format!("$: s(\"bd\").{call}({plugin:?})"));
    }

    // The object of the call: `preset` at once, and the 4 keys of the
    // effect at the end of its load, each with its title.
    let text = "$: s(\"bd\").vst(\"rustel fixture\", { ga })";
    complete_after(&mut studio, text, "ga");
    wait_for_row(&mut studio, "1 of 5 parameter choices");
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
        wait_for_row(&mut studio, "1 of 5 parameter choices");
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

    // The host is the one of the process: leave its preset root over a
    // folder with no files, so a later test counts no preset of this one.
    // On Windows the loaded plugin holds this test's folder, so its preset
    // files stay after the test ends.
    let empty = tempfile::tempdir().expect("empty preset root");
    vst::host().set_preset_folder(empty.path().to_path_buf());
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

/// A preset file that lands in the preset folder while the studio runs
/// reaches the open plugin with no rescan: the watch of the folders reads
/// them again. The preset the player saved mid-play is there once the
/// read ends, and the score keeps playing through it.
#[test]
fn a_preset_file_saved_while_the_studio_runs_reaches_the_open_plugin() {
    let mut studio = hermetic();
    let folder = tempfile::tempdir().expect("plugin folder");
    let presets = folder.path().join("presets");
    let of_fixture = presets.join(rustel_vst3_fixture::NAME);
    std::fs::create_dir_all(&of_fixture).expect("preset folder");
    std::fs::write(
        of_fixture.join("Dark Bass.vstpreset"),
        rustel_vst3_fixture::preset(0.5, 0.0),
    )
    .expect("preset file");
    let plugins = folder.path().join("plugins");
    rustel_vst3_fixture::install(&plugins);
    vst::host().set_preset_folder(presets);
    vst::pin_standard_folders(vec![plugins]);

    // The presets are the half of the plugin the saved file reaches. →
    // opens the plugin on its parameters, Tab goes to the presets.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    open_vst_tab(&mut studio);
    wait_for_row(&mut studio, "1 of 1 plugins");
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
    studio.pump_until(LOAD_BUDGET, "the presets of the read", |studio| {
        studio.rows().iter().any(|row| row.contains("Dark Bass"))
    });

    std::fs::write(
        of_fixture.join("Late Save.vstpreset"),
        rustel_vst3_fixture::preset(0.5, 0.0),
    )
    .expect("preset file");
    studio.pump_until(LOAD_BUDGET, "the preset of the watch", |studio| {
        studio.rows().iter().any(|row| row.contains("Late Save"))
    });
}
