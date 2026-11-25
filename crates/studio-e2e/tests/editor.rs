//! Editing: what keystrokes do to the score's text.
//!
//! What breaks if these fail: the musician types and the score does not
//! say what they typed. State assertions (source, history) carry these
//! tests rather than goldens - the text is the contract, its rendering is
//! the editor pane's business.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic, row_containing};

/// Tab indents the line; Shift+Tab (BackTab, what a terminal sends for the
/// shifted key) outdents it.
#[test]
fn tab_indents_and_backtab_outdents() {
    let mut studio = hermetic();

    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        studio.source(),
        "    $: s(\"bd\")\n",
        "tab indented the line by the editor's step"
    );

    studio.press(KeyCode::BackTab, KeyModifiers::NONE);
    assert_eq!(
        studio.source(),
        "$: s(\"bd\")\n",
        "backtab outdented it back"
    );
}

/// The error line appears for a score that does not parse, and clears when
/// the text is fixed - editing's feedback loop, end to end. The screen
/// assertion is a region one: the error line is live text, never golden.
#[test]
fn a_broken_edit_says_so_and_fixing_it_clears_the_line() {
    let mut studio = hermetic();

    studio.type_text("((( ");
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("✗ 1 problem")),
        "a broken score says so in the header:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter()
            .any(|row| { row.trim_start().starts_with("✗ 1:") && row.contains("Expected") }),
        "the footer carries the parser's own words:\n{}",
        rows.join("\n")
    );

    // Repair the score; the problem marker and the footer line go with it.
    studio.set_score("$: s(\"bd\")");
    studio.settle();
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains("problem")),
        "a score that parses again says nothing:\n{}",
        rows.join("\n")
    );
}

/// The syntax check's three modes, on the settings sheet. On update, a
/// broken edit says nothing while it is typed; an update still refuses it
/// and marks where, until the text moves on. Off, the refusal marks nothing
/// but the footer still says why. Full again, the score is checked at once,
/// without another keystroke.
#[test]
fn syntax_check_modes_keep_quiet_and_an_update_still_refuses() {
    let mut studio = hermetic();

    set_syntax_check(&mut studio, "on update");
    studio.type_text("((( ");
    studio.settle();
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains('✗')),
        "nothing is marked while the check is off:\n{}",
        rows.join("\n")
    );

    // The update is the engine's gate, and the switch leaves it alone.
    studio.chord("ctrl+s");
    assert!(
        studio.status().starts_with("refused - line 1: "),
        "an update still refuses a broken score: {}",
        studio.status()
    );
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("✗ 1 problem")),
        "the refusal marks where it was refused:\n{}",
        rows.join("\n")
    );

    // The text moves on: nothing checks it again, so the marks go with it.
    studio.type_text(" ");
    studio.settle();
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains('✗')),
        "the refusal's marks do not outstay the text they were about:\n{}",
        rows.join("\n")
    );

    // Off: the update refuses, the footer says why, and nothing is marked.
    set_syntax_check(&mut studio, "off");
    studio.chord("ctrl+s");
    assert!(
        studio.status().starts_with("refused - line 1: "),
        "off still says why in the footer: {}",
        studio.status()
    );
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains('✗')),
        "off marks nothing:\n{}",
        rows.join("\n")
    );

    // Full: the broken score is checked and marked straight away.
    set_syntax_check(&mut studio, "full");
    studio.settle();
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("✗ 1 problem")),
        "switched back on, the broken score says so at once:\n{}",
        rows.join("\n")
    );
}

/// Open the settings sheet, walk down until `syntax check` is the
/// selected row, step it to `mode`, and close the sheet again.
fn set_syntax_check(studio: &mut Hermetic, mode: &str) {
    studio.chord("ctrl+o");
    for _ in 0..40 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ syntax check"))
        {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    let rows = studio.rows();
    let row = row_containing(&rows, "syntax check");
    assert!(
        rows[row].contains("▸ syntax check"),
        "the row is selected: {}",
        rows[row]
    );
    // The value sits right after the label; the explanation further along
    // names every mode, so only the value column is read.
    let reads = |studio: &mut Hermetic| {
        let rows = studio.rows();
        let row = row_containing(&rows, "syntax check");
        rows[row]
            .split("syntax check")
            .nth(1)
            .is_some_and(|rest| rest.trim_start().starts_with(mode))
    };
    for _ in 0..3 {
        if reads(studio) {
            break;
        }
        studio.press(KeyCode::Right, KeyModifiers::NONE);
    }
    assert!(reads(studio), "the row reads {mode}");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}
