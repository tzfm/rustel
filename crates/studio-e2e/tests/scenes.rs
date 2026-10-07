//! Check scene creation, duplication, renaming, closure, navigation and MIDI
//! pad launch. Inspect the set folder to verify persistence.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic, hermetic_sized, reopen_over, row_containing};

/// A strip too small for every chip still shows the lit scene: it scrolls
/// so the current chip is on screen and counts the chips it scrolled past
/// as `‹N`.
#[test]
fn a_long_set_scrolls_the_strip_to_the_lit_scene() {
    // Narrow enough that eleven chips cannot fit side by side.
    let mut studio = hermetic_sized(80, 40);

    for _ in 0..10 {
        studio.press(KeyCode::Char('n'), KeyModifiers::CONTROL);
    }
    assert_eq!(studio.scene_names().len(), 11);

    // Step forward with F7 to the last scene: the strip must have
    // scrolled so the lit chip is on screen, counting what it scrolled
    // past as ‹N.
    for _ in 0..15 {
        if studio.current_scene().as_deref() == Some("scene 10") {
            break;
        }
        studio.press(KeyCode::F(7), KeyModifiers::NONE);
    }
    assert_eq!(studio.current_scene().as_deref(), Some("scene 10"));
    let rows = studio.rows();
    let strip = rows
        .iter()
        .find(|row| row.contains("‹"))
        .expect("the strip scrolled and says so");
    assert!(
        strip.contains("scene 10"),
        "the lit chip is on screen: the strip scrolled to it - {strip:?}"
    );

    // Walk back with F6 to the first scene: the strip scrolls back, and
    // now the count is on the right - what lies ahead.
    for _ in 0..15 {
        if studio.current_scene().as_deref() == Some("first") {
            break;
        }
        studio.press(KeyCode::F(6), KeyModifiers::NONE);
    }
    assert_eq!(studio.current_scene().as_deref(), Some("first"));
    let rows = studio.rows();
    let strip = rows
        .iter()
        .find(|row| row.contains("›"))
        .expect("the strip scrolled back and says what lies ahead");
    assert!(
        strip.contains("first"),
        "the first chip is back on screen - {strip:?}"
    );
}

/// Ctrl+N makes an empty scene beside the first, writes its file at once,
/// and the strip shows both. The new scene takes the first free `scene N`
/// name - beside a starter `first`, that is `scene 1`.
#[test]
fn ctrl_n_makes_an_empty_scene_its_file_written_beside_the_set() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.chord("ctrl+n");
    studio.settle();

    assert_eq!(
        studio.status(),
        "scene 2 - scene 1 (empty, written beside the set); ^R names it"
    );
    assert_eq!(studio.scene_names(), vec!["first", "scene 1"]);
    assert!(
        set.join("scene 1.strudel").is_file(),
        "a scene exists on disk from the moment it exists on screen"
    );
}

/// Ctrl+Shift+N duplicates what the focused scene says - and a prebake tab
/// refuses to be duplicated, where a copy would be a score of nothing.
/// The prebake tab is reached the way a musician reaches it: the settings
/// sheet, Enter on the prebake row.
#[test]
fn ctrl_shift_n_duplicates_and_a_prebake_refuses() {
    let mut studio = hermetic();

    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+shift+n");
    studio.settle();
    assert_eq!(studio.score(), "$: s(\"bd\")", "the copy carries the text");
    assert_eq!(studio.scene_names().len(), 2);

    // Onto a prebake tab: the settings sheet, then Enter on the local
    // prebake row. The walk is by label, not by row count, so the suite
    // holds under every feature set's row list.
    studio.chord("ctrl+shift+p");
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
    row_containing(&studio.rows(), "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "prebake (local) - setup before the scores; ^Enter applies it, ^W closes the tab"
    );
    assert_eq!(studio.focus(), None, "a prebake tab is no panel");
    assert_eq!(
        studio.scene_names().last(),
        Some(&"prebake (local)".to_owned()),
        "the prebake sits at the end of the strip"
    );

    // And the refusal: setup is not a score to copy.
    studio.chord("ctrl+shift+n");
    assert_eq!(
        studio.status(),
        "prebake (local) is setup, not a score - ^N makes an empty scene"
    );
    assert_eq!(
        studio.scene_names().len(),
        3,
        "nothing was copied - the strip grew only by the prebake tab itself"
    );
}

/// Ctrl+R renames the scene, its file follows, and Esc cancels without
/// touching anything.
#[test]
fn ctrl_r_renames_and_esc_cancels() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.chord("ctrl+r");
    assert_eq!(
        studio.status(),
        "renaming - type the new name, Enter keeps it, Esc cancels"
    );

    // Esc first: nothing happens.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.scene_names(), vec!["first"]);
    assert!(set.join("first.strudel").is_file());

    // Then the real rename: the old file goes, the new one is there, and
    // the queued save lands at the renamed path.
    studio.chord("ctrl+r");
    studio.type_text("intro");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.status(), "renamed \"first\" to \"intro\"");
    studio.settle();
    assert_eq!(studio.scene_names(), vec!["intro"]);
    assert!(set.join("intro.strudel").is_file());
    assert!(
        !set.join("first.strudel").exists(),
        "the old file does not linger"
    );
    assert!(
        studio.status().contains("intro.strudel"),
        "the save lands at the renamed path: {}",
        studio.status()
    );
}

/// Ctrl+W closes a clean scene - saying its file is still on disk - and
/// the edit on the first scene survives the round trip.
#[test]
fn ctrl_w_closes_a_clean_scene_saying_its_file_stays() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.set_score("$: s(\"hh\")");
    assert!(studio.is_dirty());

    studio.chord("ctrl+n");
    studio.settle();
    studio.chord("ctrl+w");
    studio.settle();

    assert_eq!(
        studio.status(),
        "closed \"scene 1\" - its file stays in the set; ^B lists it"
    );
    assert_eq!(studio.scene_names(), vec!["first"]);
    assert_eq!(
        studio.score(),
        "$: s(\"hh\")",
        "the edit survives on the first scene"
    );
    assert!(
        set.join("first.strudel").is_file(),
        "the scene is still on disk"
    );
}

/// Ctrl+W on a dirty scene writes it on its way out: nothing is ever lost
/// by closing, and the file the folder keeps is the text the strip had.
#[test]
fn ctrl_w_writes_a_dirty_scene_on_its_way_out() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.set_score("$: s(\"hh\")");
    studio.chord("ctrl+n");
    studio.settle();

    // Back onto the dirty first scene, and close it there.
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(studio.scene_names().first(), Some(&"first".to_owned()));
    studio.chord("ctrl+w");
    studio.settle();

    assert_eq!(studio.scene_names(), vec!["scene 1"]);
    let written = std::fs::read_to_string(set.join("first.strudel")).expect("dirty scene written");
    assert_eq!(written, "$: s(\"hh\")", "the closing write kept the edit");
}

/// Closing the last score saves it and leaves a fresh empty score, through
/// either the shortcut or the menu.
#[test]
fn closing_the_last_scene_opens_an_empty_scene() {
    for through_menu in [false, true] {
        let mut studio = hermetic();
        let set = studio.set_directory().to_path_buf();
        studio.set_score("$: s(\"hh\")");
        if through_menu {
            studio.press(KeyCode::F(1), KeyModifiers::NONE);
            studio.press(KeyCode::Char('s'), KeyModifiers::NONE);
            studio.press(KeyCode::Char('c'), KeyModifiers::NONE);
        } else {
            studio.chord("ctrl+w");
        }
        studio.settle();
        assert_eq!(studio.scene_names(), vec!["scene 1"]);
        assert_eq!(studio.score(), "");
        assert_eq!(studio.errors(), None);
        assert_eq!(
            std::fs::read_to_string(set.join("first.strudel")).unwrap(),
            "$: s(\"hh\")"
        );
        assert_eq!(
            std::fs::read_to_string(set.join("scene 1.strudel")).unwrap(),
            ""
        );
    }
}

/// F6/F7 walk the strip; stepping into a one-scene set says so rather
/// than silently doing nothing.
#[test]
fn f6_and_f7_walk_the_strip() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(studio.status(), "this set has one scene - ^N adds another");

    studio.chord("ctrl+n");
    studio.settle();
    assert_eq!(studio.scene_names(), vec!["first", "scene 1"]);

    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(studio.scene_names().first(), Some(&"first".to_owned()));
    studio.press(KeyCode::F(7), KeyModifiers::NONE);
    assert_eq!(studio.scene_names().last(), Some(&"scene 1".to_owned()));
}

/// The manifest remembers the set: a name written here is the name a
/// fresh studio over the same folder reads.
#[test]
fn the_manifest_remembers_the_set() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+r");
    studio.type_text("kept");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.scene_names(), vec!["kept"]);

    // A one-scene set keeps no manifest - there is nothing to remember
    // beyond the files themselves. A second scene is what earns one.
    assert!(!set.join("rustel-set.json").exists());
    studio.chord("ctrl+n");
    studio.settle();
    assert!(set.join("rustel-set.json").is_file());

    // Close the studio - worker joined, manifest and files long since
    // written - and open a fresh one over the same folder.
    let reopened = reopen_over(studio);
    assert_eq!(
        reopened.scene_names(),
        vec!["kept", "scene 1"],
        "a fresh studio reads the set the way the first left it"
    );
    assert!(set.join("kept.strudel").is_file());
    assert!(set.join("rustel-set.json").is_file());
}

/// Two scenes with the second holding a real score, a virtual MIDI
/// controller plugged in, and its note 36 learnt onto that second scene -
/// the learn every pad test starts from. The controller exists only in the
/// harness: no port on the runner is opened, and its notes take the same
/// path a real port's driver delivers on.
fn learn_a_pad() -> (Hermetic, std::path::PathBuf) {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.chord("ctrl+n");
    studio.settle();
    studio.set_score("$: s(\"bd\")");
    studio.attach_midi_controller("e2e pads");
    studio.chord("ctrl+l");
    assert_eq!(
        studio.status(),
        "learning - hit the pad that should launch \"scene 1\" (Esc cancels)",
        "a controller is plugged in, so the learn arms"
    );
    studio.midi_note_on("e2e pads", 0, 36, 100);
    studio.settle();
    assert_eq!(studio.status(), "c2/0 launches \"scene 1\"");
    (studio, set)
}

/// ^L learns the pad that launches the focused scene: the chord arms the
/// learn, the controller's next note binds - said in the status, kept in
/// the set's manifest - and the pad plays the scene from wherever the
/// strip stands, the two verbs together the way a DAW's pad row works.
#[test]
fn ctrl_l_learns_a_pad_and_the_pad_launches_its_scene() {
    let (mut studio, set) = learn_a_pad();

    // The learnt pad travels with the set: the manifest carries it beside
    // its scene.
    let manifest = std::fs::read_to_string(set.join("rustel-set.json")).expect("manifest kept");
    assert!(
        manifest.contains("\"pad\"") && manifest.contains("\"note\": 36"),
        "the manifest carries the pad beside its scene: {manifest}"
    );

    // Walk away from the scene the pad launches; the pad selects it back
    // and plays it. A click on a chip only selects - a pad is a play key,
    // that is its job.
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(studio.current_scene().as_deref(), Some("first"));
    assert!(!studio.is_playing(), "nothing plays before the pad is hit");
    // Past the mirror gate's window: the learn's own press was this note,
    // and one physical press must be one launch.
    std::thread::sleep(std::time::Duration::from_millis(60));
    studio.midi_note_on("e2e pads", 0, 36, 100);
    studio.settle();
    assert_eq!(studio.current_scene().as_deref(), Some("scene 1"));
    assert!(
        studio.is_playing(),
        "the pad selected the scene and played it"
    );
}

/// ^⇧L forgets the pad the scene wore: the status names it, the manifest
/// loses it, and the note that launched the scene no longer does.
#[test]
fn ctrl_shift_l_forgets_the_learnt_pad() {
    let (mut studio, set) = learn_a_pad();

    studio.chord("ctrl+shift+l");
    assert_eq!(studio.status(), "\"scene 1\" no longer launches from c2/0");
    studio.settle();
    let manifest = std::fs::read_to_string(set.join("rustel-set.json")).expect("manifest kept");
    assert!(
        !manifest.contains("\"pad\""),
        "the manifest keeps no pad once it is forgotten: {manifest}"
    );

    // And the note is only a note again: the strip stands where it stood
    // and nothing plays.
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    std::thread::sleep(std::time::Duration::from_millis(60));
    studio.midi_note_on("e2e pads", 0, 36, 100);
    studio.settle();
    assert_eq!(studio.current_scene().as_deref(), Some("first"));
    assert!(!studio.is_playing(), "a forgotten pad launches nothing");
}

/// Esc calls a learn off - the strip stops learning and the controller's
/// next note binds nothing: no pad in the manifest, no launch, and ^⇧L
/// agrees there is nothing to forget.
#[test]
fn esc_cancels_a_learn_and_the_note_binds_nothing() {
    let mut studio = hermetic();
    let set = studio.set_directory().to_path_buf();

    studio.chord("ctrl+n");
    studio.settle();
    studio.attach_midi_controller("e2e pads");
    studio.chord("ctrl+l");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "learn cancelled");

    studio.midi_note_on("e2e pads", 0, 36, 100);
    studio.settle();
    assert_eq!(
        studio.status(),
        "learn cancelled",
        "the note after a cancelled learn is only a note"
    );
    let manifest = std::fs::read_to_string(set.join("rustel-set.json")).expect("manifest kept");
    assert!(!manifest.contains("\"pad\""), "no pad was kept: {manifest}");

    studio.chord("ctrl+shift+l");
    assert_eq!(studio.status(), "\"scene 1\" has no pad");
}
