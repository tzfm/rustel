//! Drive set creation, opening and renaming through menu mnemonics and pickers;
//! verify status and files. New sets live under the config home's sets/ folder,
//! separate from the harness's initial set.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{
    Hermetic, PanelKind, hermetic, hermetic_recording, row_containing, wav_bytes,
};

/// The settings sheet's editor keybindings page, walked to by the sheet's
/// own keys. The same walk the vim suite makes.
fn open_editor_keybindings(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    for _ in 0..9 {
        studio.press(KeyCode::Home, KeyModifiers::NONE);
        if studio.rows().iter().any(|row| row.contains("▸ Terminal")) {
            studio.press(KeyCode::Down, KeyModifiers::NONE);
            assert!(
                studio
                    .rows()
                    .iter()
                    .any(|row| row.contains("▸ Editor keybindings"))
            );
            return;
        }
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    panic!("the editor keybindings row is missing");
}

fn enable_vim(studio: &mut Hermetic) {
    open_editor_keybindings(studio);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// How long a local import may take to walk its folder.
const IMPORT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// The File menu, dropped: the bar, then the title's mnemonic.
fn drop_file_menu(studio: &mut Hermetic) {
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('f'), KeyModifiers::NONE);
}

/// The folder new sets are made in: `sets/` under the config home, unless
/// the settings' sets-folder row points somewhere else.
fn sets_root(studio: &Hermetic) -> std::path::PathBuf {
    studio.config_home().join("sets")
}

/// In the Rename set… prompt: clear the name it offers, the set's own,
/// type `name` over it and press Enter.
fn retype_set_name(studio: &mut Hermetic, name: &str) {
    let offered = studio
        .set_directory()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the set's folder name")
        .to_owned();
    for _ in 0..offered.chars().count() {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.type_text(name);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
}

fn tapes(studio: &Hermetic) -> Vec<std::path::PathBuf> {
    let mut paths = std::fs::read_dir(studio.tapes_directory())
        .into_iter()
        .flatten()
        .map(|entry| entry.expect("session entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rustel-session"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[test]
fn creating_and_reopening_sets_records_in_each_sets_own_folder() {
    use rustel_runtime::session_log::SessionScript;

    let mut studio = hermetic_recording();
    let home = studio.set_directory();
    studio.chord("ctrl+s");
    studio.settle();
    let original = tapes(&studio);
    assert_eq!(original.len(), 1);
    let original_bytes = std::fs::read(&original[0]).unwrap();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE);
    assert_ne!(studio.set_directory(), home);
    assert!(tapes(&studio).is_empty(), "no empty tape before playing");
    studio.set_score("$: note(\"c3\").s(\"sine\")");
    studio.chord("ctrl+s");
    studio.settle();
    let new = tapes(&studio);
    assert_eq!(new.len(), 1, "playing the new set starts its own session");
    let saved = SessionScript::load(&new[0]).unwrap();
    assert_eq!(saved.saves.len(), 1);
    assert_eq!(&*saved.saves[0].source, studio.score());
    assert_eq!(std::fs::read(&original[0]).unwrap(), original_bytes);
    let new_bytes = std::fs::read(&new[0]).unwrap();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('r'), KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.set_directory(), home);
    studio.set_score("$: note(\"e3\").s(\"sine\")");
    studio.chord("ctrl+s");
    studio.settle();
    let reopened = tapes(&studio);
    assert_eq!(reopened.len(), 2, "returning starts a fresh session too");
    let latest = reopened
        .iter()
        .find(|path| !original.contains(path))
        .unwrap();
    assert_eq!(
        &*SessionScript::load(latest).unwrap().saves[0].source,
        studio.score()
    );
    assert_eq!(std::fs::read(&original[0]).unwrap(), original_bytes);
    assert_eq!(std::fs::read(&new[0]).unwrap(), new_bytes);
}

#[test]
fn set_browser_reveals_open_and_closed_scores_without_opening_or_editing_them() {
    let mut studio = hermetic();
    let closed = studio.set_directory().join("second.strudel");
    std::fs::write(&closed, "$: s(\"hh\")").unwrap();
    studio.set_score("$: s(\"bd*2\")");
    let edited = studio.score();
    let tabs = studio.scene_names();
    studio.chord("ctrl+b");
    let focus = studio.focus();

    for name in ["first.strudel", "second.strudel"] {
        studio.chord("alt+o");
        studio.settle();
        assert_eq!(studio.status(), format!("opened {name}"));
        assert_eq!(studio.score(), edited);
        assert_eq!(studio.scene_names(), tabs, "revealing does not open a tab");
        assert_eq!(studio.focus(), focus);
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(std::fs::read_to_string(closed).unwrap(), "$: s(\"hh\")");
}

#[test]
fn the_set_panel_opens_a_score_by_its_strip_number() {
    let mut studio = hermetic();
    std::fs::write(
        studio.set_directory().join("second.strudel"),
        "$: s(\"hh\")",
    )
    .unwrap();
    studio.chord("ctrl+b");
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.scene_names(), ["first", "second"]);
    assert_eq!(studio.score(), "$: s(\"hh\")");

    // A number no score holds names what it missed, and the panel keeps
    // the keyboard; a number on the strip opens its score, Enter on its
    // row in one key.
    studio.chord("ctrl+b");
    studio.chord("ctrl+b");
    studio.press(KeyCode::Char('5'), KeyModifiers::NONE);
    assert_eq!(studio.status(), "no scene 5 on the strip");
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    studio.press(KeyCode::Char('1'), KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "the opened score takes the keyboard");
    assert_eq!(studio.score(), "$: s(\"bd\")\n");
    assert_eq!(studio.scene_names(), ["first", "second"], "no tab added");

    studio.chord("ctrl+b");
    studio.chord("ctrl+b");
    studio.press(KeyCode::Char('2'), KeyModifiers::NONE);
    assert_eq!(studio.score(), "$: s(\"hh\")", "strip 2 selected");
    assert_eq!(studio.focus(), None);
}

/// `/` filters the set panel: letters hide the scores the query does not
/// answer - a name or any @field value - Enter leaves the filter
/// standing, Esc restores the folder. In vim `/` types at once, a second
/// `/` retypes the standing query, and the Esc ladder leaves insert
/// before it drops the filter.
#[test]
fn the_set_panel_search_filters_and_restores() {
    let mut studio = hermetic();
    for (name, body) in [
        ("alpha.strudel", "$: s(\"hh\")\n"),
        (
            "night.strudel",
            "/*\n@title night drive\n*/\n$: s(\"bd\")\n",
        ),
    ] {
        std::fs::write(studio.set_directory().join(name), body).unwrap();
    }
    studio.chord("ctrl+b");
    studio.settle();
    assert_eq!(studio.focus(), Some(PanelKind::Set));

    studio.press(KeyCode::Char('/'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('i'), KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("search: ni")),
        "the panel shows the query:\n{}",
        rows.join("\n")
    );
    row_containing(&rows, "night");
    assert!(
        rows.iter().all(|row| !row.contains("alpha")),
        "a score that does not answer hides:\n{}",
        rows.join("\n")
    );

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("alpha")),
        "Esc restores the folder:\n{}",
        rows.join("\n")
    );
    assert_eq!(studio.focus(), Some(PanelKind::Set), "Esc kept the panel");

    // Backspace over a standing filter edits the query, never the file
    // under it: pressed twice over an empty query, nothing is asked
    // about deleting and every score stays.
    studio.press(KeyCode::Char('/'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..2 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().all(|row| !row.contains("Enter deletes")),
        "no delete was armed:\n{}",
        rows.join("\n")
    );
    assert_eq!(
        studio.scene_names().as_slice(),
        ["first"],
        "every score is still on the strip"
    );
    assert!(
        studio.set_directory().join("alpha.strudel").is_file()
            && studio.set_directory().join("night.strudel").is_file(),
        "every file is still on disk"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // Vim: `/` types at once, and the Esc ladder leaves insert before
    // it drops the filter. The panel goes away first, so the vim half
    // opens it fresh, the way the studio does at startup.
    studio.chord("ctrl+b");
    assert_eq!(studio.focus(), None, "the panel is away");
    enable_vim(&mut studio);
    studio.chord("ctrl+b");
    studio.settle();
    assert_eq!(studio.focus(), Some(PanelKind::Set));
    studio.press(KeyCode::Char('/'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('i'), KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("search: ni")),
        "`/` types into the box at once:\n{}",
        rows.join("\n")
    );
    assert!(rows.iter().all(|row| !row.contains("alpha")));
    // The list letters type too: j is not held for the shipped jk
    // mapping, and jk does not leave Insert for the list keys.
    studio.press(KeyCode::Char('j'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('k'), KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("search: nijk")),
        "j and k typed at once, not held or walked:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().all(|row| !row.contains("command mode")),
        "the shipped jk mapping did not leave Insert:\n{}",
        rows.join("\n")
    );
    studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().all(|row| !row.contains("alpha")),
        "the first Esc leaves the filter standing:\n{}",
        rows.join("\n")
    );
    studio.press(KeyCode::Char('/'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('g'), KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("search: nig")),
        "a second `/` retypes the standing query:\n{}",
        rows.join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("alpha")),
        "the second Esc drops the filter:\n{}",
        rows.join("\n")
    );
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Set),
        "Esc answered the box, not the panel"
    );
}

/// File ▸ Strudel patterns… ▸ Export…: the set's scores as one pattern export at
/// the sets root, keyed by name, the open score as it stands on screen,
/// and the file offered again by the open-set prompt.
#[test]
fn export_patterns_writes_the_sets_scores_beside_the_sets() {
    let mut studio = hermetic();
    let set = studio.set_directory();
    let name = set
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap()
        .to_owned();
    // A closed score beside the open one, and an unsaved edit on screen.
    std::fs::write(set.join("spare.strudel"), "$: s(\"cp\")\n").unwrap();
    studio.type_text(" ");
    let score = studio.score();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('p'), KeyModifiers::NONE); // Strudel patterns
    studio.press(KeyCode::Char('x'), KeyModifiers::NONE); // Export…
    // The status names the whole path to the export - shortened in the
    // middle when the row cannot hold it, so the file's own name survives -
    // and a click on it shows the file in the desktop's file manager.
    let status = studio.status();
    assert!(status.starts_with("exported 2 patterns to "), "{}", status);
    assert!(
        status.contains(&format!("{name} patterns ")),
        "the path names the export: {}",
        status
    );
    assert!(
        status.ends_with(" - click the path to show it"),
        "{}",
        status
    );
    let root = sets_root(&studio);
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| {
                    file.starts_with(&format!("{name} patterns")) && file.ends_with(".json")
                })
        })
        .collect();
    assert_eq!(found.len(), 1, "one export beside the sets");
    let text = std::fs::read_to_string(found.remove(0)).unwrap();
    let rows: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(rows.len(), 2, "one entry a score: {text}");
    assert_eq!(
        rows["first"].as_str(),
        Some(score.as_str()),
        "the open score goes out as it stands"
    );
    assert_eq!(rows["spare"].as_str(), Some("$: s(\"cp\")\n"));

    // The open-set prompt offers the export beside the sets.
    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('o'), KeyModifiers::NONE); // Open set…
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "Strudel export - 2 patterns");
}

/// File ▸ Open recent…: the set just left is offered, and Enter goes back
/// to it - with the score exactly as it was left, because the set was
/// written when the studio moved away from it.
#[test]
fn open_recent_goes_back_to_the_set_left() {
    let mut studio = hermetic();
    let home = studio.set_directory();

    // Leave a mark in the score, saved by the set file when the studio
    // moves away: coming back means coming back to it.
    studio.type_text(" ");
    let edited = studio.score();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE); // New set
    assert!(
        studio.status().starts_with("new set "),
        "the studio moved to a new set: {}",
        studio.status()
    );
    assert_ne!(
        studio.set_directory(),
        home,
        "the new set is not the one left"
    );

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('r'), KeyModifiers::NONE); // Open recent…
    assert_eq!(
        studio.status(),
        "open a recent set - Enter opens the chosen one · Esc back"
    );
    // The list holds every set except the open one, so the set just left
    // is the offered row. The picker truncates a long folder name, so match
    // the row by its score count.
    let rows = studio.rows();
    let row = rows
        .iter()
        .position(|row| row.contains("▸ setx") && row.contains("1 score"))
        .expect("the set left is the selected candidate");
    let _ = row;

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        studio.set_directory(),
        home,
        "Enter opens the set just left"
    );
    assert_eq!(
        studio.score(),
        edited,
        "the score came back exactly as it was left"
    );
}

/// File ▸ Rename set… carries a sample folder imported from inside the
/// set: the source names the folder where it is now, so walking it again
/// from Sources reads its sounds rather than missing.
#[test]
fn rename_set_carries_a_sample_source_inside_it() {
    let mut studio = hermetic();
    let home = studio.set_directory();
    let kit = home.join("kit");
    std::fs::create_dir_all(kit.join("pack")).expect("bank folder");
    std::fs::write(kit.join("pack").join("hit.wav"), wav_bytes()).expect("sample");
    studio.paste(&kit.display().to_string());
    studio.settle();
    studio.pump_until(IMPORT_BUDGET, "the import settled", |studio| {
        studio.status().contains("sound(s) imported")
    });
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE); // Rename set…
    retype_set_name(&mut studio, "gig night");
    assert!(
        studio.status().starts_with("renamed the set"),
        "setup: the set was renamed: {}",
        studio.status()
    );

    // The sheet opens on the page it was left on, Sources; Down walks to
    // the source's row and Enter walks the folder it names again.
    studio.chord("ctrl+o");
    for _ in 0..40 {
        if studio.rows().iter().any(|row| row.contains("▸ kit")) {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let source_row = |studio: &mut Hermetic| {
        studio
            .rows()
            .into_iter()
            .find(|row| row.contains("▸ kit") && row.contains("(local)"))
            .unwrap_or_default()
    };
    studio.pump_until(IMPORT_BUDGET, "the source walked again", |studio| {
        let row = source_row(studio);
        row.contains("sound(s)") || row.contains("missing")
    });
    // No ⚠ check: the set's own scan lists the same bank, so the page may
    // warn that the two share a name once the walk is in.
    let row = source_row(&mut studio);
    assert!(
        row.contains("sound(s)"),
        "the moved source reads its sounds: {row}"
    );
}
