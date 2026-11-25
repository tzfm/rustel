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
use rustel_studio_e2e::{Hermetic, hermetic, reopen_over, wav_bytes};

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
    let name = folder
        .file_name()
        .expect("the path has a name")
        .to_string_lossy();
    rows.iter()
        .any(|row| row.contains(name.as_ref()) && row.contains("(local)"))
}

/// Wait for each dropped folder to appear on the Sources page.
///
/// The source row is the durable result. The import receipt is a transient
/// status, and the source's own `fetching its list of sounds` line can
/// replace it before a poll sees it.
fn wait_for_import(studio: &mut Hermetic, folders: &[&Path]) {
    studio.pump_until(
        std::time::Duration::from_secs(30),
        "the dropped folders reached Sources",
        |studio| {
            let rows = studio.rows();
            folders.iter().all(|folder| source_row_holds(&rows, folder))
        },
    );
}

/// A single dropped sound imports the folder it sits in. That folder is the
/// source, so the sound's sibling files come with it.
#[test]
fn a_dropped_audio_file_imports_the_folder_it_sits_in() {
    let mut studio = hermetic();
    let folder = audio_folder("one-file");
    let pack = folder.join("pack");
    let file = pack.join("hit.wav");

    studio.paste(&file.display().to_string());
    studio.settle();
    wait_for_import(&mut studio, &[&pack]);

    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &pack),
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

/// A drop that is neither audio, nor a folder of audio, nor a score, nor a
/// MIDI file is refused with its reason, and the path is not typed into the
/// score.
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
        "nothing to import from note.txt - audio, a folder of audio, a score, or a MIDI file",
        "the refusal names the file and the four things a drop can be"
    );
    assert!(
        !studio.score().contains("note.txt"),
        "the refused path was not typed into the score"
    );
    cleanup(&folder);
}

/// Several folders in one drop are one selection: they import together.
#[test]
fn dropping_two_folders_imports_them_together() {
    let mut studio = hermetic();
    let first = audio_folder("two-a");
    let second = audio_folder("two-b");

    studio.paste(&format!("{} {}", first.display(), second.display()));
    studio.settle();
    wait_for_import(&mut studio, &[&first, &second]);

    let rows = studio.rows();
    assert!(
        source_row_holds(&rows, &first) && source_row_holds(&rows, &second),
        "both folders are listed:\n{}",
        rows.join("\n")
    );
    cleanup(&first);
    cleanup(&second);
}

/// A text prompt keeps a pasted path as text: renaming a scene to a
/// string that happens to be a real path renames the scene - the prompt
/// claimed the paste before the drop dispatch ever saw it.
#[test]
fn a_drop_into_a_text_prompt_stays_text() {
    let mut studio = hermetic();
    let starter = studio.set_directory().join("first.strudel");

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.type_text("sr");
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
    wait_for_import(&mut studio, &[&folder]);

    // Close the studio the way the shell would: the pref is written on a
    // debounce, so the wait is what makes the quit honest.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);

    studio.chord("ctrl+o");
    // The sources page is the fifth tab.
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
