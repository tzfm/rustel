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

/// Typing lands in the score, one character at a time, at the caret. The
/// starter score opens with the caret at buffer position 0, so End first
/// puts it where a musician would be - after the first call.
#[test]
fn typing_edits_the_score() {
    let mut studio = hermetic();

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.type_text("x");

    assert_eq!(
        studio.source(),
        "$: s(\"bd\")x\n",
        "the `x` landed at the caret, after the first call"
    );
}

/// Typing while the reference column is open still reaches the score when
/// the column has no search box - reading an entry and typing what it
/// teaches is the point. (An open column in *browse* mode does take the
/// letters; that half lives with the panel's own tests.)
#[test]
fn typing_with_a_panel_open_reaches_the_score() {
    let mut studio = hermetic();

    // End then six Lefts put the caret just past the `s` of `s("bd")`.
    // Ctrl+D opens that entry and moves focus into the column. An entry
    // has no search box to type in.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    for _ in 0..6 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+d");
    studio.settle();
    assert_eq!(
        studio.focus(),
        Some(rustel_studio_e2e::PanelKind::Reference),
        "setup: the reference opened and took focus"
    );

    studio.type_text("9");

    assert!(
        studio.source().contains("9"),
        "the score took the key even with the reference open:\n{}",
        studio.source()
    );
}

/// Ctrl+Z undoes one edit at a time, and Ctrl+Shift+Z (the spelling the
/// docs table gives) brings it back.
#[test]
fn undo_and_redo_walk_history() {
    let mut studio = hermetic();

    studio.type_text("$: s(\"hh\")\n");
    assert!(studio.can_undo(), "an edit made history");

    studio.chord("ctrl+z");
    assert!(
        !studio.source().contains("hh"),
        "undo removed the edit:\n{}",
        studio.source()
    );

    studio.chord("ctrl+shift+z");
    assert!(
        studio.source().contains("hh"),
        "redo brought the edit back:\n{}",
        studio.source()
    );
}

/// Ctrl+Y also redoes, as the docs table's *always available* column says.
#[test]
fn ctrl_y_also_redoes() {
    let mut studio = hermetic();

    studio.type_text("$: s(\"hh\")\n");
    studio.chord("ctrl+z");
    assert!(!studio.source().contains("hh"), "setup: undone");

    studio.chord("ctrl+y");
    assert!(
        studio.source().contains("hh"),
        "Ctrl+Y redid:\n{}",
        studio.source()
    );
}

/// Ctrl+/ comments the line under the caret and uncomment on a second
/// press. On a legacy keyboard the chord may arrive as Ctrl+7 (the 0x1F
/// byte); both spellings must work, which is why the alias exists.
#[test]
fn comment_toggle_comments_and_uncomments() {
    let mut studio = hermetic();

    studio.chord("ctrl+/");
    assert!(
        studio.source().starts_with("//"),
        "the line was commented:\n{}",
        studio.source()
    );

    studio.chord("ctrl+/");
    assert!(
        !studio.source().starts_with("//"),
        "a second press uncommented:\n{}",
        studio.source()
    );
}

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

/// Enter after a chained call keeps its indentation. A second Enter with
/// nothing typed removes it: the caret is at the start of the same line and
/// no line is added. One undo brings the indentation back.
#[test]
fn a_second_enter_leaves_the_indentation_of_a_chain() {
    let mut studio = hermetic();
    let chain = "$: s(\"piano\")\n    .lpf(1000)";
    studio.set_score(chain);
    studio.set_caret(0);
    let (start_column, first_row) = studio.cursor().expect("setup: the caret is drawn");
    studio.set_caret(chain.len());

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.source(), format!("{chain}\n    "));
    assert_eq!(studio.cursor(), Some((start_column + 4, first_row + 2)));

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.source(), format!("{chain}\n"), "no line was added");
    assert_eq!(studio.cursor(), Some((start_column, first_row + 2)));

    studio.chord("ctrl+z");
    assert_eq!(studio.source(), format!("{chain}\n    "));
}

/// A bracketed paste arrives through the paste event, exactly as a terminal
/// delivers it, and lands as text.
#[test]
fn a_paste_lands_as_text() {
    let mut studio = hermetic();

    studio.paste("$: s(\"cp\")\n");

    assert_eq!(
        studio.source(),
        "$: s(\"cp\")\n$: s(\"bd\")\n",
        "the paste replaced the selection-less caret region with its text"
    );
}

/// The raw-paste batch: a terminal delivering bracketed paste one chunk at
/// a time floods the app with key events, and the editor must take them in
/// order without dropping or duplicating. 480 keys is the batch size the
/// plan names; the app's raw-paste path exists precisely for this flood.
#[test]
fn the_raw_paste_batch_takes_every_key_in_order() {
    let mut studio = hermetic();

    // 480 printable characters through the key path, the way a terminal
    // without bracketed paste delivers a paste.
    let text = "abc".repeat(160);
    for character in text.chars() {
        studio.press(KeyCode::Char(character), KeyModifiers::NONE);
    }
    studio.settle();

    let source = studio.source();
    let count = source.matches("abc").count();
    assert_eq!(
        count, 160,
        "every triple landed, none dropped or duplicated:\n{source}"
    );
}

/// Undo after a paste removes the whole paste as one edit - a paste is one
/// moment, not one per character.
#[test]
fn undo_removes_a_paste_whole() {
    let mut studio = hermetic();

    studio.paste("$: s(\"cp\")\n");
    assert!(studio.source().contains("cp"), "setup: pasted");
    assert!(studio.can_undo(), "the paste made history");

    studio.chord("ctrl+z");
    assert!(
        !studio.source().contains("cp"),
        "one undo took the whole paste back:\n{}",
        studio.source()
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
