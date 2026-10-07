//! The editor's two decorations the chord table names and no other suite
//! row had: word wrap (^U), and the bracket match under the caret. What
//! breaks if these fail: a long line reads as one row the editor scrolls
//! away, or a missing bracket is invisible until the linter says so.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::row_containing;

/// A line far wider than the 150-cell frame, so wrapping it has to take
/// more than one row.
const LONG_LINE: &str = "$: note(\"c3 e3 g3 b3 c4 e4 g4 b4 c5 e5 g5 b5 c6 e6 g6 b6\").s(\"sawtooth\").lpf(800).room(0.5).gain(0.9).pan(0.2).delay(0.25).attack(0.01).release(0.4)";

/// Word wrap ships on for every scene: a line wider than the pane continues
/// onto the rows under it. ^U turns it off, and ^U again restores wrapping.
/// Toggling the view never edits the text.
#[test]
fn ctrl_u_wraps_a_long_line_and_back() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.set_score(LONG_LINE);

    // Wrapped by default: the long line occupies more rows than one - the
    // row after the first holds the line's tail, not the next line's start.
    let rows = studio.rows();
    let first = row_containing(&rows, "sawtooth");
    assert!(
        first + 1 < rows.len() && rows[first + 1].contains(".release(0.4)"),
        "the line continues onto the next row:\n{}",
        rows[first..first + 3].join("\n")
    );

    // Turn wrapping off: the line runs off the edge.
    studio.press(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert!(
        studio.status().starts_with("no wrap - "),
        "the status names the disabled mode: {}",
        studio.status()
    );
    let rows = studio.rows();
    let first = row_containing(&rows, "sawtooth");
    assert!(
        !rows[first + 1].contains(".release(0.4)"),
        "unwrapped, the continuation is gone:\n{}",
        rows[first..first + 2].join("\n")
    );

    // Turn wrapping back on and recover the continuation.
    studio.press(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert!(
        studio.status().starts_with("word wrap - "),
        "the status names the restored mode: {}",
        studio.status()
    );
    let rows = studio.rows();
    let first = row_containing(&rows, "sawtooth");
    assert!(
        first + 1 < rows.len() && rows[first + 1].contains(".release(0.4)"),
        "the line wraps again:\n{}",
        rows[first..first + 3].join("\n")
    );
    assert_eq!(studio.score(), LONG_LINE, "wrap never edits the text");
}

/// The bracket match: the caret beside a bracket lights both halves in the
/// theme's bracket underline, an orphan lights in the error colour, and the
/// setting's switch is on the sheet. The test asserts what a musician sees,
/// the colour of the two cells, and not editor internals.
#[test]
fn the_bracket_under_the_caret_lights_its_partner() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.set_score("$: s(\"bd\")\n");
    // The caret onto the closing paren of the s() call: beside both
    // halves, which is the match position the docs name.
    let close_at = studio.score().find("(\"bd\")").expect("the call") + 6;
    studio.set_caret(close_at);
    studio.settle();

    let (open_at, close_at) = bracket_cells(&mut studio);
    let pair = studio.theme_bracket_color();
    assert_eq!(
        studio.cell_underline_color(open_at.0, open_at.1),
        Some(pair),
        "the opening bracket is lit"
    );
    assert_eq!(
        studio.cell_underline_color(close_at.0, close_at.1),
        Some(pair),
        "its partner is lit the same way"
    );

    // An orphan: no closer anywhere in the score, and the opener is
    // marked in the error colour - before the linter has to say so.
    studio.set_score("$: s(\"bd\"");
    let head = studio.score().find("(\"bd\"").expect("the call");
    studio.set_caret(head);
    studio.settle();
    let rows = studio.rows();
    let row = rows
        .iter()
        .position(|row| row.contains("$: s("))
        .expect("the score's row") as u16;
    let open_x = rows[row as usize].find("$: s(").expect("the call") as u16 + 4;
    let underline = studio.cell_underline_color(open_x, row);
    assert_eq!(
        underline,
        Some(studio.theme_error_color()),
        "an orphan wears the error colour: {underline:?}"
    );
}

#[test]
fn changing_the_caret_shape_changes_the_bracket_mark_immediately() {
    let mut studio = rustel_studio_e2e::hermetic();
    studio.set_score("$: s(\"bd\")\n");
    let caret = studio.score().find('(').unwrap();
    studio.set_caret(caret);
    for shape in 0..6 {
        studio.chord("ctrl+o");
        for _ in 0..40 {
            if studio
                .rows()
                .iter()
                .any(|row| row.contains("▸ caret shape"))
            {
                break;
            }
            studio.press(KeyCode::Down, KeyModifiers::NONE);
        }
        assert!(
            studio
                .rows()
                .iter()
                .any(|row| row.contains("▸ caret shape"))
        );
        if shape > 0 {
            studio.press(KeyCode::Right, KeyModifiers::NONE);
        }
        studio.press(KeyCode::Esc, KeyModifiers::NONE);
        studio.settle();
        let (open, close) = bracket_cells(&mut studio);
        if shape < 4 {
            let color = Some(studio.theme_bracket_color());
            assert_eq!(
                studio.cell_underline_color(open.0, open.1),
                color,
                "shape {shape}"
            );
            assert_eq!(
                studio.cell_underline_color(close.0, close.1),
                color,
                "shape {shape}"
            );
        } else {
            let fill = studio.cell_bg(open.0, open.1);
            assert_ne!(fill, studio.cell_bg(open.0 - 1, open.1), "shape {shape}");
            assert_eq!(fill, studio.cell_bg(close.0, close.1), "shape {shape}");
            assert_ne!(
                fill,
                Some(studio.theme_bracket_color()),
                "soft fill, not solid accent"
            );
        }
    }
}

/// The screen cells of the score's `s("bd")` call: the opening paren and,
/// when the score carries one, the closing paren. Found off the rendered
/// rows so the test does not guess at layout arithmetic.
fn bracket_cells(studio: &mut rustel_studio_e2e::Hermetic) -> ((u16, u16), (u16, u16)) {
    let rows = studio.rows();
    let row = rows
        .iter()
        .position(|row| row.contains("$: s("))
        .expect("the score's row") as u16;
    let text = &rows[row as usize];
    let open = text.find("$: s(").expect("the call") as u16 + 4;
    let close = text.find(')').expect("the closer") as u16;
    ((open, row), (close, row))
}

/// The settings sheet carries a `bracket match` switch; flipping it is the
/// same live, kept move every other switch is, and it exists so a musician
/// who finds the lighting loud can turn it off.
#[test]
fn the_bracket_switch_is_on_the_sheet() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.chord("ctrl+o");
    // Down until the row is the selected one - the sheet marks it `▸ ` -
    // not merely visible, which it is from the first frame.
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ bracket match"))
        {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    let row = row_containing(&studio.rows(), "bracket match");
    assert!(
        studio.rows()[row].contains("● on"),
        "the switch ships on: {}",
        studio.rows()[row]
    );

    // Off, then on again - the same live flip the other switches get.
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    let row = row_containing(&studio.rows(), "bracket match");
    assert!(
        studio.rows()[row].contains("○ off"),
        "the flip is live: {}",
        studio.rows()[row]
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    row_containing(&studio.rows(), "● on");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}
