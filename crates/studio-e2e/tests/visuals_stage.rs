//! Drive visualizers through playing scores and inspect rendered rows. Check
//! options and that disabling visuals returns the space to the editor.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use rustel_studio_e2e::hermetic;

/// Every canvas painter draws something: the frame with the widget
/// differs from the plain score's frame, and by more than a caret.
#[test]
fn every_canvas_painter_paints_the_stage() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    let plain = studio.render();

    for kind in [
        "pianoroll",
        "punchcard",
        "wordfall",
        "spiral",
        "pitchwheel",
        "scope",
        "spectrum",
    ] {
        studio.set_score(&format!("$: s(\"bd\")._{kind}()"));
        studio.chord("ctrl+s");
        studio.settle();
        let painted = studio.render();
        let changed = painted.iter().zip(&plain).filter(|(a, b)| a != b).count();
        assert!(
            changed > 4,
            "{kind} left the stage as it found it ({changed} rows changed):\n{}",
            painted.join("\n")
        );
    }
}

/// The time painters draw their axes and their events; the audio painters
/// say what they are waiting for rather than drawing a lie.
#[test]
fn the_painters_draw_their_own_kind_of_truth() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();

    // A roll with labels: events on a grid, with the sound's name. The
    // score must ask for labels, because `pianoroll` defaults to none. The
    // row must be one of the roll's own, because the editor also shows
    // `bd` in the source.
    studio.set_score("$: s(\"bd\")._pianoroll({ labels: 1 })");
    studio.chord("ctrl+s");
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("bd") && !row.contains("_pianoroll")),
        "the roll labels what it paints, on a row that is not the score:\n{}",
        rows.join("\n")
    );

    // The spiral's legend names only what sounds at this moment, so a
    // sound's name there would race the clock. The stable part is the
    // shape: Braille with a dotted guide track, where the roll draws a
    // block grid.
    studio.set_score("$: s(\"bd\")._spiral()");
    studio.chord("ctrl+s");
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row
            .chars()
            .any(|cell| ('\u{2800}'..='\u{28FF}').contains(&cell))),
        "the spiral draws in Braille, as the reference says:\n{}",
        rows.join("\n")
    );

    // No audio hardware in a hermetic studio is still an audio frame -
    // silent, but there - so the scope draws its graticule: the dotted
    // centre line of a flat trace, not a note about waiting.
    studio.set_score("$: s(\"bd\")._scope()");
    studio.chord("ctrl+s");
    studio.settle();
    // On a loaded machine the engine's first frame can arrive after the
    // install. Turn the loop until it arrives, with a deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        studio.pump();
        let rows = studio.rows();
        if !rows.iter().any(|row| row.contains("waiting for audio"))
            || std::time::Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains("waiting for audio")),
        "the scope has audio even in silence:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().any(|row| row.contains("⠉")),
        "the scope draws its centre line over silence:\n{}",
        rows.join("\n")
    );
}

/// An unknown canvas never reaches the stage: the linter refuses the
/// update in the checker's own words, and the last good score keeps
/// playing - the stage itself has no note to add.
#[test]
fn an_unknown_painter_says_so() {
    let mut studio = hermetic();

    // A good score first, so there is something playing to protect.
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing());

    studio.set_score("$: s(\"bd\")._nosuchpicture()");
    studio.chord("ctrl+s");
    studio.settle();
    let status = studio.status();
    assert!(
        status.contains("refused") && status.contains("unknown function"),
        "the refusal names the unknown painter: {status}"
    );
    assert!(
        studio.is_playing(),
        "the last good score survives the refusal"
    );
}

/// The stage does not eat the code: the score's own text stays readable
/// under the painter, and the editor keeps the caret and the keys.
#[test]
fn the_stage_does_not_eat_the_code() {
    let mut studio = hermetic();

    studio.set_score("$: s(\"bd\")._punchcard()");
    studio.chord("ctrl+s");
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("_punchcard")),
        "the score is still on screen:\n{}",
        rows.join("\n")
    );

    // Typing still edits the score while the painter paints.
    studio.type_text(" ");
    studio.type_text("hh");
    assert!(
        studio.score().contains("hh"),
        "the keys went to the score, not the picture: {:?}",
        studio.score()
    );
}

/// The visualizers switch is honoured live: off, the stage gives the
/// rows back to the code and nothing painter-drawn remains.
#[test]
fn turning_visualizers_off_gives_the_rows_back() {
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    let plain = studio.render();

    studio.set_score("$: s(\"bd\")._punchcard()");
    studio.chord("ctrl+s");
    studio.settle();
    let painted = studio.render();
    assert!(
        painted.iter().zip(&plain).any(|(a, b)| a != b),
        "the painter was painting before the switch"
    );

    // Off, through the settings sheet, by the drawn row's own label.
    // The sheet marks its selection with '▸', so walk until it sits on
    // the visualizers row, then flip it leftwards.
    studio.chord("ctrl+o");
    let mut reached = false;
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains('▸') && row.contains("visualizers"))
        {
            reached = true;
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    assert!(reached, "the visualizers row is selected in the sheet");
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "settings kept - drawing with cells",
        "the switch applies this frame, not next evaluation"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // The score keeps its text; the picture is gone from the rows.
    let after = studio.render();
    assert!(
        after.iter().any(|row| row.contains("_punchcard")),
        "the score survives the switch:\n{}",
        after.join("\n")
    );
    let painter_rows = after
        .iter()
        .filter(|row| row.contains("┊") && row.contains("▏") || row.contains("██"))
        .count();
    assert_eq!(
        painter_rows,
        0,
        "no painter rows remain:\n{}",
        after.join("\n")
    );
}
