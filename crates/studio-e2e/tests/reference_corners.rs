//! Check example auditioning, pointer navigation and generator controls.
//! Copying/history and sample-browser Space live in snippets and panels_samples.
//! Use a sine example to avoid sample downloads. Read preview status before
//! settling: installation replaces it with the transport status.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use rustel_studio_e2e::{Hermetic, hermetic};

/// Open the reference column on its Examples tab - the sixth and last.
fn open_examples(studio: &mut Hermetic) {
    studio.chord("ctrl+f");
    for _ in 0..5 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    assert_eq!(studio.reference_tab(), Some("examples"));
}

/// Open the reference column on its Generator tab - the fifth.
fn open_generator(studio: &mut Hermetic) {
    studio.chord("ctrl+f");
    for _ in 0..4 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    assert_eq!(studio.reference_tab(), Some("generator"));
}

/// The screen cell where this text is drawn, as a click wants it.
fn text_at(studio: &mut Hermetic, needle: &str) -> (u16, u16) {
    let rows = studio.rows();
    let y = rows
        .iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row reads {needle:?}:\n{}", rows.join("\n")));
    let byte = rows[y].find(needle).expect("just found");
    let x = rows[y][..byte].chars().count() as u16 + 1;
    (x, y as u16)
}

/// A left click and release, left standing where it landed - the
/// caller reads the moment-of-the-click status before anything
/// asynchronous is given the chance to replace it.
fn click_now(studio: &mut Hermetic, x: u16, y: u16) {
    studio.mouse(
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
    );
    studio.mouse(
        MouseEventKind::Up(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
    );
}

fn click(studio: &mut Hermetic, x: u16, y: u16) {
    click_now(studio, x, y);
    studio.settle();
}

/// Click a row by its text.
fn click_row(studio: &mut Hermetic, needle: &str) {
    let (x, y) = text_at(studio, needle);
    click(studio, x, y);
}

/// A preview stopped alone takes the transport down with it, and the
/// stop is a graceful tail the engine finishes on its own clock -
/// waited out here the way `transport.rs` waits one, bounded.
fn wait_for_stop(studio: &mut Hermetic) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while studio.is_playing() || studio.is_stopping() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stop never landed"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        studio.pump();
    }
}

/// The generator control rows draw their value as the row's last
/// number: the label and the rail carry none.
fn control_value(studio: &mut Hermetic, label: &str) -> u32 {
    let row = studio
        .rows()
        .into_iter()
        .find(|row| row.contains(label))
        .unwrap_or_else(|| panic!("no control reads {label:?}:\n{}", studio.rows().join("\n")));
    let mut last: Option<String> = None;
    let mut run = String::new();
    for character in row.chars() {
        if character.is_ascii_digit() {
            run.push(character);
        } else {
            if !run.is_empty() {
                last = Some(std::mem::take(&mut run));
            }
            run.clear();
        }
    }
    if !run.is_empty() {
        last = Some(run);
    }
    last.unwrap_or_else(|| panic!("the {label} row reads no number: {row}"))
        .parse()
        .expect("the value is a number")
}

/// The pointer walks the examples tree as the arrows do: a click opens a
/// section, a click opens a shelf, and a click on an example selects it and
/// plays it. Space and → are then the same play-and-stop key for the row
/// that is sounding.
#[test]
fn clicking_through_the_examples_tree_plays_what_it_lands_on() {
    let mut studio = hermetic();
    open_examples(&mut studio);

    // TRACKS ships folded; Techno is its second shelf; Sub bass is a
    // sine-wave example - engine voices only, nothing to fetch.
    click_row(&mut studio, "TRACKS");
    click_row(&mut studio, "Techno");
    let (x, y) = text_at(&mut studio, "Sub bass");
    click_now(&mut studio, x, y);
    assert_eq!(
        studio.status(),
        "previewing the snippet",
        "the clicked example plays on its own"
    );
    studio.settle();
    assert!(studio.is_playing(), "the audition started the transport");

    // Space on the row that is sounding is the stop - and the stopped
    // preview takes the transport it started with it, tail and all.
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview stopped");
    wait_for_stop(&mut studio);
    assert!(!studio.is_playing(), "the stopped preview is silence again");

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "previewing the snippet",
        "and Space starts it again - one key, play and stop"
    );
    studio.settle();
    assert!(studio.is_playing());

    // → on a leaf row plays what Space plays - and mid-sound, the same
    // snippet under the cursor, it is the stop.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "preview stopped",
        "the arrow is the same play-and-stop key"
    );
    wait_for_stop(&mut studio);
    assert!(!studio.is_playing());
}

/// Under a playing score the preview joins it rather than replacing
/// it - the status says under - and stopping the preview puts the
/// score back, still playing.
#[test]
fn an_example_previews_under_a_playing_score_and_puts_it_back() {
    let mut studio = hermetic();
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "setup: the set is playing");

    open_examples(&mut studio);
    click_row(&mut studio, "TRACKS");
    click_row(&mut studio, "Techno");
    let (x, y) = text_at(&mut studio, "Sub bass");
    click_now(&mut studio, x, y);
    assert_eq!(
        studio.status(),
        "previewing the snippet under the score",
        "the preview measures itself against the music in flight"
    );
    studio.settle();
    assert!(studio.is_playing(), "the set never stopped for the preview");

    // Space takes the snippet back out and puts the score back - the
    // promise is in the status, and the transport keeps running
    // through the swap.
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview stopped; the score plays on");
    studio.settle();
    assert!(studio.is_playing(), "the score is still the thing playing");
}

/// The generator answers its two composing keys from the keyboard: `g`
/// makes a fresh idea every press, `v` develops the idea in hand - and
/// `c` copies whichever is on the shelf, the code and not the name.
#[test]
fn g_composes_fresh_and_v_develops_the_idea_in_hand() {
    let mut studio = hermetic();
    open_generator(&mut studio);

    // The open direction lists its whole shelf: the action rows, the
    // copy row and every control the direction carries.
    let rows = studio.rows();
    for label in [
        "Glassy pulse",
        "Generate",
        "Similar",
        "Copy",
        "Activity",
        "Tone",
        "Motion",
        "Space",
        "Variation",
        "Drums",
        "Delay",
    ] {
        assert!(
            rows.iter().any(|row| row.contains(label)),
            "the shelf lists {label}:\n{}",
            rows.join("\n")
        );
    }

    let take = |studio: &mut Hermetic| {
        studio.press(KeyCode::Char('c'), KeyModifiers::NONE);
        studio.settle();
        studio
            .clipboard_text()
            .expect("c copies the idea on the shelf")
    };

    studio.press(KeyCode::Char('g'), KeyModifiers::NONE);
    studio.settle();
    let fresh = take(&mut studio);
    assert!(
        fresh.contains("$:"),
        "a composed idea is score code: {fresh}"
    );

    studio.press(KeyCode::Char('g'), KeyModifiers::NONE);
    studio.settle();
    let other = take(&mut studio);
    assert_ne!(other, fresh, "every g is a NEW idea");

    studio.press(KeyCode::Char('v'), KeyModifiers::NONE);
    studio.settle();
    let similar = take(&mut studio);
    assert_ne!(similar, other, "v develops the idea in hand into another");
    assert!(
        similar.contains("$:"),
        "and it is score code too: {similar}"
    );
}

/// The control rows walk their whole range: → is a step, ⇧→ is five,
/// Home and End are the ends, and the floor and ceiling are plain
/// stops rather than wraps.
#[test]
fn the_generator_controls_walk_their_whole_range() {
    let mut studio = hermetic();
    open_generator(&mut studio);

    // A click on the label selects the control without dragging it -
    // the rail starts further right.
    click_row(&mut studio, "Activity");
    let start = control_value(&mut studio, "Activity");
    assert!(
        (0..=100).contains(&start),
        "the control reads inside its range: {start}"
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        control_value(&mut studio, "Activity"),
        (start + 1).min(100),
        "→ is one step"
    );

    studio.press(KeyCode::Right, KeyModifiers::SHIFT);
    studio.settle();
    assert_eq!(
        control_value(&mut studio, "Activity"),
        (start + 6).min(100),
        "⇧→ is five"
    );

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        control_value(&mut studio, "Activity"),
        0,
        "Home is the floor"
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(control_value(&mut studio, "Activity"), 1);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        control_value(&mut studio, "Activity"),
        0,
        "and the floor is a plain stop, not a wrap"
    );

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        control_value(&mut studio, "Activity"),
        100,
        "End is the ceiling"
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        control_value(&mut studio, "Activity"),
        100,
        "the ceiling holds"
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(control_value(&mut studio, "Activity"), 99);
}
