//! Check update and stop from every surface. The app unit test covers key
//! recognition; this suite checks that transport receives keys before panels.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::hermetic;

/// One real evaluation: the starter score is heard, and the header says so.
#[test]
fn update_plays_the_score_and_says_so() {
    let mut studio = hermetic();

    studio.chord("ctrl+s");
    studio.settle();

    assert!(
        studio.is_playing(),
        "the score is sounding after Ctrl+S settles"
    );
    assert!(
        !studio.is_evaluating(),
        "nothing is left in flight once the engine has answered"
    );
    assert!(
        !studio.is_stopping(),
        "nothing is stopping: it is playing, not cutting"
    );

    // `is_playing()` reads the app's state; the header is what the musician
    // reads. The tempo readout identifies the header row, so this is a word on
    // the row that carries it rather than a substring found anywhere on screen.
    let header = studio
        .rows()
        .into_iter()
        .find(|row| row.contains("bpm"))
        .expect("the header row carries the tempo");
    assert!(
        header.contains("PLAYING"),
        "the header still reads stopped while the score sounds: {header}"
    );
}

/// F5 is the portable spelling of the same command.
///
/// The footer chip it lights is `Update`, the same one Ctrl+S lights - but a
/// lit chip is a cell attribute, and the harness reads text, so this holds the
/// command's effect rather than its highlight.
#[test]
fn f5_is_the_portable_update() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(5), KeyModifiers::NONE);
    studio.settle();

    assert!(studio.is_playing(), "F5 updates and plays");
}

/// Ctrl+G cuts the transport. The engine answers asynchronously, so the
/// test observes the two states a musician can see: something in flight
/// (stopping or already stopped) once the key has been handled.
#[test]
fn stop_cuts_the_transport() {
    let mut studio = hermetic();

    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        studio.is_playing(),
        "setup: the score plays before the stop"
    );

    studio.chord("ctrl+g");
    studio.settle();

    assert!(
        studio.is_stopping() || !studio.is_playing(),
        "the first stop begins a graceful tail or finishes it"
    );
}

/// F8 is the portable stop.
#[test]
fn f8_is_the_portable_stop() {
    let mut studio = hermetic();

    studio.chord("ctrl+s");
    studio.settle();
    studio.press(KeyCode::F(8), KeyModifiers::NONE);
    studio.settle();

    assert!(
        studio.is_stopping() || !studio.is_playing(),
        "F8 begins a graceful tail or finishes the stop"
    );
}

/// The transport outranks an open panel: with the settings sheet up and the
/// keyboard inside it, Ctrl+S still updates. This is the contract the
/// comment above `transport_key` states; here it is held with a real sheet.
#[test]
fn the_transport_wins_from_inside_an_open_panel() {
    let mut studio = hermetic();

    studio.chord("ctrl+b");
    studio.settle();
    assert!(
        studio.focus().is_some(),
        "setup: the settings sheet holds the keyboard"
    );

    studio.chord("ctrl+s");
    studio.settle();

    assert!(studio.is_playing(), "the transport ran under the sheet");
}

/// A stop pressed while a stop is already running does not queue a second
/// one. The second cut lands, the transport stays cut, and the second stop
/// does not re-arm the tail.
#[test]
fn a_stop_while_stopping_cuts_the_tail() {
    let mut studio = hermetic();

    studio.chord("ctrl+s");
    studio.settle();
    studio.chord("ctrl+g");
    studio.chord("ctrl+g");
    studio.settle();

    assert!(
        !studio.is_playing() && !studio.is_stopping(),
        "two stops land one cut: no tail, no re-arm"
    );
}

#[test]
fn piano_over_a_stopped_scores_tail_settles_the_transport_header() {
    let mut studio = hermetic();
    studio.set_score("$: note(36).s('sine').slow(2)");
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing());
    studio.chord("ctrl+g");
    studio.settle();
    assert!(studio.is_stopping(), "the long score note is still ringing");
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.is_stopping(),
        "a piano key must not finish the score tail"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while studio.is_playing() || studio.is_stopping() {
        assert!(
            std::time::Instant::now() < deadline,
            "score tail did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        studio.pump();
    }
    let header = studio
        .rows()
        .into_iter()
        .find(|row| row.contains("bpm"))
        .unwrap();
    assert!(
        header.contains("STOPPED"),
        "piano must not keep the score stopping: {header}"
    );
    let cycle = |row: &str| {
        row.split_once("cycle ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .expect("the header shows the score cycle")
            .to_owned()
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    studio.pump();
    let later = studio
        .rows()
        .into_iter()
        .find(|row| row.contains("bpm"))
        .unwrap();
    assert!(
        later.contains("STOPPED"),
        "the score stays stopped: {later}"
    );
    assert_eq!(
        cycle(&header),
        cycle(&later),
        "the stopped score's cycle stays frozen while piano is open"
    );
}

/// Ctrl+S writes the file it played: the set on disk holds the text that
/// was sounding. In the studio a save is an update, so the write is part of
/// the contract and not a side effect.
#[test]
fn update_writes_the_file_it_played() {
    let mut studio = hermetic();

    // End puts the caret after the starter call, so the typed statement is
    // a second line rather than a prefix of the first (which would not
    // parse, and an unparseable score is not one the transport can play).
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.type_text("\n$: s(\"sine\")");
    studio.chord("ctrl+s");
    studio.settle();

    let score = studio.directory().join("first.strudel");
    let on_disk = std::fs::read_to_string(&score).expect("the score is on disk");
    assert!(
        on_disk.contains("sine"),
        "the file the studio wrote holds what it played:\n{on_disk}"
    );
    assert!(!studio.is_dirty(), "the saved scene is no longer dirty");
}

/// An update in the same input batch installs the edited text and tempo.
#[test]
fn update_in_the_same_batch_as_a_text_burst_installs_the_new_text() {
    let mut studio = hermetic();

    studio.chord("ctrl+s");
    studio.settle();
    let header_of = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio
            .rows()
            .into_iter()
            .find(|row| row.contains("bpm"))
            .expect("the header row carries the tempo")
    };
    let before = header_of(&mut studio);

    // Do not settle between the edit and update. The new score halves
    // the starter's 0.5 cps tempo.
    studio.chord("ctrl+a");
    studio.type_text("setcps(0.25)");
    studio.chord("enter");
    studio.type_text("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();

    let after = header_of(&mut studio);
    assert!(
        after.contains("0.25") && after.contains("cps"),
        "the same-batch update left the old tempo sounding: {after} (was {before})"
    );
    assert_eq!(studio.source(), "setcps(0.25)\n$: s(\"bd\")");
}

/// An unchanged update restores fader controls after a select-all paste.
#[test]
fn updating_identical_pasted_playing_source_restores_its_fader_controls() {
    let mut studio = hermetic();
    let source = "setcpm(90)\n$: s(\"bd\").lpf(slider(2900, 0, 5000, 50)).gain(0.06)";
    studio.set_score(source);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "{}", studio.status());
    assert_eq!(studio.errors(), None);

    studio.chord("ctrl+a");
    studio.paste(source);
    studio.chord("ctrl+s");
    assert!(studio.status().starts_with("unchanged - already playing"));
    assert!(!studio.is_evaluating(), "the update preserves playback");
    studio.settle();

    studio.set_caret(source.find("2900").expect("the slider value"));
    studio.press(KeyCode::Up, KeyModifiers::ALT);
    studio.settle();
    assert_eq!(studio.source(), source.replace("2900", "2950"));
    assert!(studio.is_playing());
    assert_eq!(studio.errors(), None);
}
