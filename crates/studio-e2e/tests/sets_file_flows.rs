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
use rustel_studio_e2e::{Hermetic, hermetic, hermetic_recording, row_containing, wav_bytes};

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

/// File ▸ New set: a folder named after the day appears under the sets
/// folder with a starter scene and its set file, and it is taken up at
/// once - the header, the score and the disk all agree.
#[test]
fn the_file_menu_makes_a_new_set() {
    let mut studio = hermetic();
    let _ = std::fs::create_dir_all(sets_root(&studio));
    let before: Vec<std::path::PathBuf> = std::fs::read_dir(sets_root(&studio))
        .expect("sets root reads")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('n'), KeyModifiers::NONE); // New set

    let after: Vec<std::path::PathBuf> = std::fs::read_dir(sets_root(&studio))
        .expect("sets root reads")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    let made: Vec<_> = after.iter().filter(|path| !before.contains(path)).collect();
    assert_eq!(
        made.len(),
        1,
        "exactly one folder was made: {:?}",
        after
            .iter()
            .filter(|p| !before.contains(p))
            .collect::<Vec<_>>()
    );
    let made = made[0].clone();

    // On disk: the starter scene and the set file, written as soon as the
    // set was made - a set taken up on stage must exist to be opened.
    assert!(
        made.join("scene 1.strudel").is_file(),
        "the starter scene is on disk in {}",
        made.display()
    );
    assert!(
        made.join("rustel-set.json").is_file(),
        "the set file is written at once"
    );

    // On screen: the set is the one taken up, and the status says so.
    let status = studio.status();
    assert!(
        status.starts_with("new set "),
        "the status names the act: {status}"
    );
    row_containing(&studio.rows(), "scene 1");
    assert_eq!(
        studio.scene_names().as_slice(),
        ["scene 1"],
        "the set taken up is the one made"
    );
    assert_eq!(
        studio.set_directory(),
        made,
        "the studio sits in the folder it made"
    );
}

/// File ▸ Open set…: the sets folder's other sets are offered by name
/// with their score counts, and Enter on one opens it - the score on
/// screen becomes that folder's.
#[test]
fn open_set_lists_the_others_and_opens_one() {
    let mut studio = hermetic();
    let root = sets_root(&studio);
    std::fs::create_dir_all(&root).expect("sets root made");
    rustel_studio::scenes::SceneSet::create_in(&root, "guest set", "$: s(\"cp\")")
        .expect("the second set is made");

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('o'), KeyModifiers::NONE); // Open set…
    assert_eq!(
        studio.status(),
        "open a set - Enter opens the chosen set · Tab browses · type a path · Esc back"
    );
    // The other set is offered by name, with what is in it.
    let rows = studio.rows();
    let row = row_containing(&rows, "guest set");
    assert!(
        rows[row].contains("1 score"),
        "the candidate says what it holds: {}",
        rows[row]
    );

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let status = studio.status();
    assert!(
        status.starts_with("opened set guest set"),
        "the status names the set taken up: {status}"
    );
    assert_eq!(
        studio.set_directory(),
        root.join("guest set"),
        "the studio moved into the chosen folder"
    );
    row_containing(&studio.rows(), "$: s(\"cp\")");
    assert_eq!(
        studio.scene_names().as_slice(),
        ["scene 1"],
        "the opened set's scene is on the strip"
    );
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

/// File ▸ Rename set…: the current name is offered for editing, Enter
/// moves the folder, and everything follows - the scores' paths are
/// rewritten and the tape being kept inside it is carried over.
#[test]
fn rename_set_moves_the_folder_and_everything_follows() {
    let mut studio = hermetic();

    // A tape inside the set, so the rename has something being kept to
    // carry with it.
    studio.write_tape("2026-09-05T12-00-00", [(0.0, "$: s(\"bd\")")]);
    let home = studio.set_directory();

    drop_file_menu(&mut studio);
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE); // Rename set…
    assert_eq!(
        studio.status(),
        "rename the set - type the new name and press Enter · Esc back"
    );

    retype_set_name(&mut studio, "gig night");

    let status = studio.status();
    assert!(
        status.starts_with("renamed the set"),
        "the status names the act: {status}"
    );
    let renamed = home
        .parent()
        .expect("the set home sits in a parent folder")
        .join("gig night");
    assert!(
        renamed.join("first.strudel").is_file(),
        "the scores followed the folder to {}",
        renamed.display()
    );
    assert!(
        renamed
            .join("sessions")
            .join(format!(
                "session-2026-09-05T12-00-00{}",
                rustel_runtime::product::SESSION_FILE_SUFFIX
            ))
            .is_file(),
        "the tape followed the folder"
    );
    assert!(!home.exists(), "the old folder is gone, not copied");
    assert_eq!(studio.set_directory(), renamed, "the studio moved with it");
    row_containing(&studio.rows(), "gig night");
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
