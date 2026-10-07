//! Terminals deliver dropped paths as paste. Check folder/audio imports, score
//! tabs and unsupported paths across focused surfaces. Prompts retain path text;
//! a drop over the samples browser imports it.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, PanelKind, hermetic, reopen_over, wav_bytes};

/// A fresh folder under the temp root this one test owns: unique per tag
/// so two tests can drop two different folders in one run, and outside
/// every fixture's directories so nothing else can mistake it for set
/// furniture.
fn temp_folder(tag: &str) -> PathBuf {
    let at = std::env::temp_dir().join(format!("rustel-drop-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&at);
    std::fs::create_dir_all(&at).expect("temp folder");
    at
}

/// A folder of audio the way a dropped pack arrives: one bank folder with
/// one real WAV in it, so the import has something to count.
fn audio_folder(tag: &str) -> PathBuf {
    let at = temp_folder(tag);
    let bank = at.join("pack");
    std::fs::create_dir_all(&bank).expect("bank folder");
    std::fs::write(bank.join("hit.wav"), wav_bytes()).expect("sample");
    at
}

/// Best-effort: the temp root is the runner's to clean, but a suite that
/// leaves twenty folders behind is a suite that eventually trips over
/// its own name.
fn cleanup(at: &Path) {
    let _ = std::fs::remove_dir_all(at);
}

/// The folder name as the Sources page reads it - the label is the last
/// component, not the whole path, and the row says `(local)`.
fn source_row_holds(rows: &[String], folder: &Path) -> bool {
    let name = folder.file_name().unwrap().to_string_lossy();
    rows.iter()
        .any(|row| row.contains(name.as_ref()) && row.contains("(local)"))
}

/// A local import lands as `fetching its list of sounds` and settles, on
/// a later frame, to `N sound(s) imported` - the walk over the folder
/// runs off the input path. Bounded like every asynchronous wait in the
/// suite: five seconds of frames, then the assertion that follows says
/// what never arrived.
fn wait_for_import(studio: &mut Hermetic) {
    for _ in 0..500 {
        if studio.status().contains("sound(s) imported") {
            return;
        }
        studio.pump();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A dropped folder of audio is imported as a sample source. A terminal has
/// no folder window to show progress, so the studio opens the Sources page,
/// which lists what arrived.
#[test]
fn a_dropped_folder_is_imported_and_lands_on_sources() {
    let mut studio = hermetic();
    let folder = audio_folder("import");

    studio.paste(&folder.display().to_string());
    studio.settle();
    wait_for_import(&mut studio);

    let status = studio.status().to_owned();
    assert!(
        status.contains("sound(s) imported"),
        "the drop imported the folder: {status}"
    );
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Settings),
        "the import lands on the settings sheet, not in the score"
    );
    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &folder),
        "the imported folder is listed as a local source on the sheet:\n{}",
        rows.join("\n")
    );
    cleanup(&folder);
}

/// A take dropped from the set's own `sessions/` folder is refused: that
/// folder is where the studio writes, never a sample source. The status
/// says so, the score stays as it was and the Sources page never opens.
#[test]
fn a_take_dropped_from_the_sets_sessions_folder_is_not_imported() {
    let mut studio = hermetic();
    let sessions = studio
        .set_directory()
        .join(rustel_runtime::product::SESSIONS_DIRECTORY_NAME);
    std::fs::create_dir_all(&sessions).expect("sessions folder");
    let take = sessions.join("take.wav");
    std::fs::write(&take, wav_bytes()).expect("take");
    let score = studio.score();

    studio.paste(&take.display().to_string());
    studio.settle();

    assert_eq!(
        studio.status(),
        "sessions not imported - it is a set's own sessions folder, which the studio writes into"
    );
    assert_eq!(studio.score(), score, "the path was not typed");
    assert_ne!(studio.focus(), Some(PanelKind::Settings));
}

/// A single dropped sound imports the folder it sits in. That folder is the
/// source, so the sound's sibling files come with it.
#[test]
fn a_dropped_audio_file_imports_the_folder_it_sits_in() {
    let mut studio = hermetic();
    let folder = audio_folder("one-file");
    let file = folder.join("pack").join("hit.wav");

    studio.paste(&file.display().to_string());
    studio.settle();
    wait_for_import(&mut studio);

    let status = studio.status().to_owned();
    assert!(
        status.contains("sound(s) imported"),
        "the file's folder imported: {status}"
    );
    assert!(
        status.contains("pack"),
        "the receipt names the folder the file sits in, not the file: {status}"
    );
    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &folder.join("pack")),
        "the source listed is the folder, not the file:\n{}",
        rows.join("\n")
    );
    cleanup(&folder);
}

/// A dropped score opens as a tab of the current set - the same landing
/// as opening it from the set panel, so a drop and a click end in the
/// same place.
#[test]
fn a_dropped_score_opens_as_a_tab() {
    let mut studio = hermetic();
    let folder = temp_folder("score");
    let score = folder.join("second.strudel");
    std::fs::write(&score, "$: s(\"hh*4\")").expect("score");

    studio.paste(&score.display().to_string());
    studio.settle();

    let status = studio.status().to_owned();
    assert!(
        status.contains("scene 2") && status.contains("second"),
        "the drop opened the score as the second scene: {status}"
    );
    assert!(
        studio.scene_names().iter().any(|name| name == "second"),
        "the set now holds the dropped score: {:?}",
        studio.scene_names()
    );
    assert_eq!(
        studio.score(),
        "$: s(\"hh*4\")",
        "the editor shows the dropped score's own text"
    );
    cleanup(&folder);
}

/// A score that is already open is selected, not opened twice: the drop
/// says so and the tab count stays put.
#[test]
fn a_dropped_score_that_is_already_open_is_selected_not_duplicated() {
    let mut studio = hermetic();
    let starter = studio.set_directory().join("first.strudel");

    studio.paste(&starter.display().to_string());
    studio.settle();

    assert_eq!(
        studio.status(),
        "first.strudel is already open",
        "the drop of an open score says so rather than making a second tab"
    );
    assert_eq!(
        studio.scene_names().len(),
        1,
        "the set did not grow a duplicate scene"
    );
    assert_eq!(
        studio.focus(),
        None,
        "the keys stay with the score - no sheet opened for a non-event"
    );
}

/// A drop that is neither audio, nor a folder of audio, nor a score is
/// refused with its reason, and the path is not typed into the score.
#[test]
fn a_dropped_file_that_is_neither_audio_nor_score_is_refused() {
    let mut studio = hermetic();
    let folder = temp_folder("refuse");
    let note = folder.join("note.txt");
    std::fs::write(&note, "not a sound").expect("note");

    studio.paste(&note.display().to_string());
    studio.settle();

    assert_eq!(
        studio.status(),
        "nothing to import from note.txt - audio, a folder of audio, or a score",
        "the refusal names the file and the three things a drop can be"
    );
    assert!(
        !studio.score().contains("note.txt"),
        "the refused path was not typed into the score"
    );
    cleanup(&folder);
}

/// A mixed selection does both halves at once: what can import does, and
/// what cannot is named in the status rather than silently lost.
#[test]
fn a_mixed_drop_imports_what_it_can_and_names_what_it_skipped() {
    let mut studio = hermetic();
    let folder = audio_folder("mixed");
    let note = folder.join("note.txt");
    std::fs::write(&note, "not a sound").expect("note");

    studio.paste(&format!("{} {}", folder.display(), note.display()));

    // Read before the frames settle: the import's own receipt replaces
    // the status once the folder's scan lands, and the skipped file's
    // naming is the first thing the drop says.
    assert_eq!(
        studio.status(),
        "note.txt skipped: not audio or a score",
        "the skipped file is named"
    );

    wait_for_import(&mut studio);
    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &folder),
        "and the folder beside it still imported:\n{}",
        rows.join("\n")
    );
    cleanup(&folder);
}

/// Several folders in one drop are one selection: they import together
/// and the receipt counts what arrived, not what was asked for.
#[test]
fn dropping_two_folders_imports_them_together() {
    let mut studio = hermetic();
    let first = audio_folder("two-a");
    let second = audio_folder("two-b");

    studio.paste(&format!("{} {}", first.display(), second.display()));
    studio.settle();
    wait_for_import(&mut studio);

    assert!(
        studio.status().contains("2 source(s) imported"),
        "one drop, two sources, one receipt: {}",
        studio.status()
    );
    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &first) && source_row_holds(&rows, &second),
        "both folders are listed:\n{}",
        rows.join("\n")
    );
    cleanup(&first);
    cleanup(&second);
}

/// A pasted path that is not there is a paste, not a drop: the
/// filesystem is the arbiter of what a drop was, so text that merely
/// looks like a path still reaches the editor.
#[test]
fn a_pasted_path_that_is_not_there_is_text_not_a_drop() {
    let mut studio = hermetic();
    let missing =
        std::env::temp_dir().join(format!("rustel-drop-missing-{}.wav", std::process::id()));
    assert!(!missing.exists(), "the fixture path must not exist");

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.paste(&missing.display().to_string());
    studio.settle();

    assert!(
        studio.score().contains("rustel-drop-missing"),
        "the nonexistent path was typed into the score: {}",
        studio.score()
    );
    assert!(
        !studio.status().contains("imported"),
        "nothing imported from a path that is not there: {}",
        studio.status()
    );
}

/// The samples search box is the deliberate exception: a folder dropped
/// while the browser holds the keyboard imports instead of being typed
/// into the filter - the gesture that most wants to work, since the
/// samples panel is where you look to see whether the drop landed.
#[test]
fn a_drop_over_the_samples_search_box_imports_instead_of_typing() {
    let mut studio = hermetic();
    let folder = audio_folder("over-samples");

    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    assert!(
        studio.focus().is_some(),
        "setup: the samples browser holds the keyboard"
    );

    studio.paste(&folder.display().to_string());
    studio.settle();
    wait_for_import(&mut studio);

    let status = studio.status().to_owned();
    assert!(
        status.contains("sound(s) imported"),
        "the drop over the search box imported: {status}"
    );
    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &folder),
        "and it landed on Sources:\n{}",
        rows.join("\n")
    );
    cleanup(&folder);
}

/// A text prompt keeps a pasted path as text: renaming a scene to a
/// string that happens to be a real path renames the scene - the prompt
/// claimed the paste before the drop dispatch ever saw it.
#[test]
fn a_drop_into_a_text_prompt_stays_text() {
    let mut studio = hermetic();
    let starter = studio.set_directory().join("first.strudel");

    studio.chord("ctrl+r");
    studio.settle();
    let path = starter.display().to_string();
    studio.paste(&path);
    studio.settle();

    // The field is narrower than a full temp-root path and shows its
    // head, so the assertion is on the head of the paste: the prompt is
    // holding the path as text, whatever end of it the field can show.
    let head: String = path.chars().take(20).collect();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains(&head)),
        "the prompt field holds the pasted path as its text:\n{}",
        rows.join("\n")
    );
    assert!(
        !studio.status().contains("already open"),
        "the drop dispatch never ran while a prompt held the keys: {}",
        studio.status()
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.scene_names(),
        vec!["first".to_owned()],
        "and the cancelled rename left the scene alone"
    );
}

/// An imported source is a pref, and prefs are written on a debounce:
/// the folder a drop imported is still a source when the studio reopens
/// over the same set.
#[test]
fn an_imported_drop_survives_a_reopen() {
    let mut studio = hermetic();
    let folder = audio_folder("persist");

    studio.paste(&folder.display().to_string());
    studio.settle();
    wait_for_import(&mut studio);
    assert!(
        studio.status().contains("sound(s) imported"),
        "setup: the drop imported"
    );

    // Close the studio the way the shell would: the pref is written on a
    // debounce, so the wait is what makes the quit honest.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);

    studio.chord("ctrl+o");
    for _ in 0..4 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &folder),
        "the reopened studio still lists the dropped folder:\n{}",
        rows.join("\n")
    );
    cleanup(&folder);
}
