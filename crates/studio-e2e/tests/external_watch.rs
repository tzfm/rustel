//! Follow the open score's file when another program saves it: the text in
//! the editor, the score that plays, and the inline painters that stay put.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic};

/// Walk the settings rows by label until one is selected.
fn select_row(studio: &mut Hermetic, marker: &str) {
    for _ in 0..40 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        studio.pump();
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// Put watch external changes on the rung `presses` steps from off.
fn turn_watch_to(studio: &mut Hermetic, presses: usize) {
    studio.settle();
    studio.chord("ctrl+o");
    select_row(studio, "▸ watch external changes");
    for _ in 0..presses {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
    }
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // Let the watch take its first look: the first look at a whole set is
    // only a baseline, so a file changed after this is a real change.
    std::thread::sleep(Duration::from_millis(150));
    studio.pump();
}

/// Turn the loop until the watch lands. The watch checks the file ten times
/// a second and reads it after one more check sees the same stamp.
fn wait_for(studio: &mut Hermetic, done: impl Fn(&Hermetic) -> bool) {
    for _ in 0..80 {
        if done(studio) {
            return;
        }
        studio.pump();
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "the watch never landed - score {:?}, status {:?}",
        studio.score(),
        studio.status()
    );
}

#[test]
fn sync_reloads_the_open_score() {
    let mut studio = hermetic();
    turn_watch_to(&mut studio, 1);
    let path = studio.set_directory().join("first.strudel");
    std::fs::write(&path, "$: s(\"bd\") // from notepad\n").expect("external save");
    wait_for(&mut studio, |studio| {
        studio.score().contains("from notepad")
    });
    assert_eq!(studio.status(), "reloaded first from disk");
}

#[test]
fn sync_and_eval_plays_the_new_text_and_keeps_the_file() {
    let mut studio = hermetic();
    turn_watch_to(&mut studio, 2);
    let path = studio.set_directory().join("first.strudel");
    std::fs::write(&path, "$: s(\"bd\") // from notepad\n").expect("external save");
    wait_for(&mut studio, |studio| studio.is_playing());
    assert!(studio.score().contains("from notepad"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "$: s(\"bd\") // from notepad\n"
    );
}

#[test]
fn sync_and_eval_opens_an_unopened_score() {
    let mut studio = hermetic();
    let path = studio.set_directory().join("second.strudel");
    std::fs::write(&path, "$: s(\"bd\") // second\n").expect("second score");
    turn_watch_to(&mut studio, 2);

    std::fs::write(&path, "$: s(\"bd\") // edited second\n").expect("external save");
    wait_for(&mut studio, |studio| {
        studio.is_playing() && studio.score().contains("edited second")
    });
}

#[test]
fn sync_and_eval_switches_to_an_open_tab() {
    let mut studio = hermetic();
    let path = studio.set_directory().join("second.strudel");
    std::fs::write(&path, "$: s(\"bd\") // second\n").expect("second score");
    studio.paste(&path.display().to_string());
    studio.settle();
    assert!(studio.score().contains("second"), "the second tab is open");
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    studio.settle();
    assert!(!studio.score().contains("second"), "back on the first tab");

    turn_watch_to(&mut studio, 2);
    std::fs::write(&path, "$: s(\"bd\") // edited second\n").expect("external save");
    wait_for(&mut studio, |studio| {
        studio.is_playing() && studio.score().contains("edited second")
    });
}

#[test]
fn sync_keeps_the_inline_visualizer() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\")._pianoroll({ labels: 1 })");
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing());
    let before = studio.rows();
    assert!(
        before
            .iter()
            .any(|row| row.contains("bd") && !row.contains("_pianoroll")),
        "setup: the roll paints:\n{}",
        before.join("\n")
    );

    turn_watch_to(&mut studio, 1);
    let path = studio.set_directory().join("first.strudel");
    std::fs::write(
        &path,
        "// edited elsewhere\n$: s(\"bd\")._pianoroll({ labels: 1 })\n",
    )
    .expect("external save");
    wait_for(&mut studio, |studio| {
        studio.score().contains("edited elsewhere")
    });
    studio.pump();
    let after = studio.rows();
    assert!(
        after
            .iter()
            .any(|row| row.contains("bd") && !row.contains("_pianoroll")),
        "the roll still paints after the reload:\n{}",
        after.join("\n")
    );
}
