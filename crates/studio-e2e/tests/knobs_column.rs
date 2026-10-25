//! The knobs column: the call under the caret becomes rails, a rail writes
//! the score, the far-left stop takes an entry out and a move right puts it
//! back, and the column follows the caret from call to call.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use rustel_studio_e2e::{Hermetic, PanelKind, hermetic, row_containing};

/// A studio over `score`, saved and settled, with the caret one key left of
/// the last line's end - inside the last call, before its closing paren.
fn studio_over(score: &str) -> Hermetic {
    let mut studio = hermetic();
    studio.chord("ctrl+a");
    studio.type_text(score);
    studio.chord("ctrl+s");
    studio.settle();
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio
}

/// View ▸ Knobs, the column's one door: it ships with no chord.
fn open_knobs(studio: &mut Hermetic) {
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('k'), KeyModifiers::NONE);
    assert!(!studio.menu_open(), "the toggle closes the bar");
}

/// The column's own cells: the rightmost stretch of each row, where the
/// rails live. The column's title shows only while it has the keyboard,
/// so once the score has the keys the reading says which call it reads.
fn column_text(rows: &[String]) -> Vec<String> {
    const WIDTH: usize = 26;
    rows.iter()
        .map(|row| {
            let chars: Vec<char> = row.chars().collect();
            let take = chars.len().min(WIDTH);
            chars[chars.len() - take..].iter().collect()
        })
        .collect()
}

/// The call under the caret becomes rails: its name in the column's title,
/// each numeric entry a label with its value. Esc hands the keyboard back
/// and the column stays, the way the desk does.
#[test]
fn the_call_under_the_caret_becomes_rails() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").set({ cutoff: 800, resonance: 5 })");
    assert_eq!(studio.errors(), None, "the score plays");
    open_knobs(&mut studio);
    assert_eq!(studio.focus(), Some(PanelKind::Knobs));
    row_containing(&studio.rows(), "knobs · set");
    let rail_row = |rows: &[String], label: &str, value: &str| {
        rows.iter()
            .any(|row| row.contains(label) && row.contains(value) && !row.contains(':'))
    };
    assert!(
        rail_row(&column_text(&studio.rows()), "cutoff", "800"),
        "the column's own row for the cutoff, not the score's:\n{}",
        column_text(&studio.rows()).join("\n")
    );
    assert!(
        studio.score().contains("cutoff: 800"),
        "the score keeps what it wrote:\n{}",
        studio.score()
    );

    assert!(
        column_text(&studio.rows())
            .iter()
            .any(|row| row.contains("⌫ off")),
        "the footer names the key that takes the entry out:\n{}",
        column_text(&studio.rows()).join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "Esc returns the keyboard");
    assert!(
        rail_row(&column_text(&studio.rows()), "resonance", "5"),
        "the column stays with its rows:\n{}",
        column_text(&studio.rows()).join("\n")
    );
}

/// One move on a rail writes the score: the number under the label changes
/// in place, and the status says what it became.
#[test]
fn a_move_on_a_rail_writes_the_score() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").set({ pan: 0.5, cutoff: 800 })");
    open_knobs(&mut studio);

    // The first rail is the pan, 0 to 1 as the reference gives it.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.score().contains("pan: 0.51"),
        "the rail writes the score in place:\n{}",
        studio.score()
    );
    assert!(
        studio.status().contains("pan 0.51"),
        "the status says the value: {}",
        studio.status()
    );

    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.score().contains("pan: 0.5,"),
        "and back:\n{}",
        studio.score()
    );
    assert_eq!(
        studio.errors(),
        None,
        "every write is a score the engine plays"
    );
}

/// The far-left stop takes an optional entry out of the score and a move
/// right puts it back as it was.
#[test]
fn the_far_left_stop_takes_an_entry_out_and_back() {
    let score = "$: note(\"c3\").s(\"sine\").set({ pan: 0.5, cutoff: 800 })";
    let mut studio = studio_over(score);
    open_knobs(&mut studio);

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.settle();
    assert!(
        !studio.score().contains("pan"),
        "the off stop removes the entry:\n{}",
        studio.score()
    );
    assert!(
        column_text(&studio.rows())
            .iter()
            .any(|row| row.contains("pan") && row.contains("off")),
        "the rail stays in the column, held off:\n{}",
        column_text(&studio.rows()).join("\n")
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.score().trim_end(),
        score,
        "a move right writes the entry back as it was"
    );
}

/// e opens the form on the rail's four numbers, and Enter writes the typed
/// value into the score. A value past a bound moves the bound with it.
#[test]
fn the_form_writes_an_exact_value() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").set({ cutoff: 800, pan: 0.5 })");
    open_knobs(&mut studio);

    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    row_containing(&studio.rows(), "value");
    studio.type_text("1200");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.score().contains("cutoff: 1200"),
        "Enter writes the exact value:\n{}",
        studio.score()
    );
    assert!(
        studio.status().contains("cutoff is 1200"),
        "the status says the value: {}",
        studio.status()
    );

    // Past the cutoff's 20000: written as typed, and the top moves to it.
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    studio.type_text("30000");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.score().contains("cutoff: 30000"),
        "the value and its new top:\n{}",
        studio.score()
    );
}

/// The form remembers the bounds it set, so the rail it leaves behind
/// holds its value at the ceiling the form wrote.
#[test]
fn the_form_remembers_its_bounds() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").set({ cutoff: 800 })");
    open_knobs(&mut studio);

    // max: select the field's own text and type over it.
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Char('a'), KeyModifiers::CONTROL);
    studio.type_text("800");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();

    // The remembered ceiling holds the rail: a move right cannot pass
    // 800, where the cutoff's own range would go on to 20000.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.score().contains("cutoff: 800"),
        "the remembered bound holds the value:\n{}",
        studio.score()
    );
}

/// A rail stops at the engine's own limit: the top of `unison` is the 32
/// voices the voice renders, and the score it writes still plays.
#[test]
fn a_rail_stops_at_the_engines_limit() {
    let mut studio = studio_over("$: note(\"c3 e3\").s(\"supersaw\").unison(16)");
    open_knobs(&mut studio);
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        studio.score().contains("unison(32)"),
        "the voice's own ceiling:\n{}",
        studio.score()
    );
    assert_eq!(studio.errors(), None, "a score the engine plays");
    studio.pump_until(
        std::time::Duration::from_secs(5),
        "the update plays",
        |studio| studio.is_playing(),
    );
}

/// A drag keeps the rail it pressed, the way the score's own sliders keep
/// theirs: the pointer can leave the column and cross the score and other
/// rows, and the rail follows its column, held at its ends. The far left
/// is the off stop.
#[test]
fn a_drag_keeps_its_rail_wherever_the_pointer_goes() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").set({ pan: 0.5, cutoff: 800 })");
    open_knobs(&mut studio);
    let rows = studio.rows();
    let column = column_text(&rows);
    let label = column
        .iter()
        .position(|row| row.contains("pan") && row.contains("0.5"))
        .unwrap_or_else(|| panic!("the pan rail:\n{}", column.join("\n")));
    let y = label as u16 + 1;
    let width = rows[0].chars().count() as u16;
    let rail = rows[y as usize]
        .chars()
        .enumerate()
        .skip(width as usize - 26)
        .find(|(_, glyph)| matches!(glyph, '·' | '─' | '█'))
        .map(|(x, _)| x as u16)
        .expect("the rail's cells");
    let mouse = |studio: &mut Hermetic, kind, x, y| studio.mouse(kind, x, y, KeyModifiers::NONE);

    mouse(
        &mut studio,
        MouseEventKind::Down(MouseButton::Left),
        rail + 10,
        y,
    );
    mouse(&mut studio, MouseEventKind::Drag(MouseButton::Left), 0, 2);
    assert!(
        !studio.score().contains("pan"),
        "far left, over the score, is the off stop:\n{}",
        studio.score()
    );
    mouse(
        &mut studio,
        MouseEventKind::Drag(MouseButton::Left),
        width - 1,
        30,
    );
    assert!(
        studio.score().contains("pan: 1,"),
        "past the screen's right edge is the top:\n{}",
        studio.score()
    );
    mouse(
        &mut studio,
        MouseEventKind::Up(MouseButton::Left),
        width - 1,
        0,
    );
    assert!(studio.score().contains("pan: 1,"), "{}", studio.score());
    assert!(
        studio.score().contains("cutoff: 800"),
        "the other rail stays where it was:\n{}",
        studio.score()
    );
}

/// The column follows the caret: the same file with another call moves the
/// rails to it, and back, without reopening anything. Its title names the
/// call whoever has the keys, and a positional number goes by the name the
/// reference gives it.
#[test]
fn the_column_follows_the_caret_between_calls() {
    let mut studio = studio_over(
        "$: note(\"c3\").s(\"sine\").set({ cutoff: 800 })\n$: note(\"e3\").s(\"sine\").hpf(300)",
    );
    open_knobs(&mut studio);
    row_containing(&studio.rows(), "knobs · hpf");
    assert!(
        !column_text(&studio.rows())
            .iter()
            .any(|row| row.contains("#0")),
        "the argument goes by its name:\n{}",
        column_text(&studio.rows()).join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    row_containing(&column_text(&studio.rows()), "knobs · hpf");
    studio.press(KeyCode::Up, KeyModifiers::NONE);
    row_containing(&column_text(&studio.rows()), "knobs · set");
    row_containing(&column_text(&studio.rows()), "800");
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    row_containing(&column_text(&studio.rows()), "300");
}

/// A call with no numbers shows a placeholder under its title.
#[test]
fn a_call_with_no_numbers_shows_a_placeholder() {
    let mut studio = studio_over("$: note(\"c3 e3\")");
    open_knobs(&mut studio);
    let column = column_text(&studio.rows());
    row_containing(&column, "knobs · note");
    row_containing(&column, "- empty -");
}

/// ^⇧K opens the column with the keyboard on it, and closes it again, on a
/// terminal that tells ^⇧K from ^K.
#[test]
fn ctrl_shift_k_opens_and_closes_the_column() {
    let mut studio =
        rustel_studio_e2e::hermetic_with(rustel_studio::editor::KeyboardCapabilities::enhanced());
    studio.settle();
    studio.chord("ctrl+shift+k");
    assert_eq!(studio.focus(), Some(PanelKind::Knobs));
    let column = |studio: &mut Hermetic| {
        studio
            .rows()
            .iter()
            .any(|row| row.contains("put the caret in a call"))
    };
    assert!(column(&mut studio), "the column is up");
    studio.chord("ctrl+shift+k");
    assert_eq!(studio.focus(), None, "the keyboard went back to the score");
    assert_eq!(studio.status(), "knobs hidden");
    assert!(!column(&mut studio), "the column closed");
}

/// The footer's second row names the call's own keys: on a plugin call,
/// `a` to add a knob and `r` to get the suggested list back.
#[test]
fn a_plugin_call_names_its_list_keys() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").vst(\"Some FX\", { mix: 0.5 })");
    open_knobs(&mut studio);
    let column = column_text(&studio.rows());
    assert!(
        column
            .iter()
            .any(|row| row.contains("a add · r reset list")),
        "the call's keys:\n{}",
        column.join("\n")
    );
}

/// a opens a plugin's list or a call's documented options: `set` has
/// neither, and the status says so rather than opening an empty sheet.
#[test]
fn add_needs_a_list() {
    let mut studio = studio_over("$: note(\"c3\").s(\"sine\").set({ cutoff: 800 })");
    open_knobs(&mut studio);

    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(
        studio.status().contains("vst"),
        "the status explains set has no list: {}",
        studio.status()
    );
}
