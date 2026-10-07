//! Check settings changes, limits, scrolling, About paging and persistence.
//! Prebakes, sources, keybinds and editor effects have separate suites.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, PanelKind, hermetic, hermetic_sized, reopen_over};

/// Walk the sheet's rows by label until the given one is selected - by
/// what is drawn, not by row arithmetic. Up/Down move the selection on
/// every list page (only Right/Left/Space change values), so the walk
/// itself touches nothing.
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

/// The drawn row that carries this label - the row itself, not the
/// explanation the sheet prints underneath the list.
fn row_with(studio: &mut Hermetic, label: &str) -> String {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains(label))
        .unwrap_or_else(|| panic!("no row reads {label:?}:\n{}", studio.rows().join("\n")))
}

/// The selected row, by its marker - inside the sheet, not the header's
/// scene chips, which wear the same `▸`.
fn selected_row(studio: &mut Hermetic) -> Option<String> {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains("▸") && row.contains("│"))
}

/// The row's VALUE, as its own segment: a row with room prints its
/// explanation inline after the value, and explanations quote other
/// rungs (`header counters · advanced adds…`), so `contains` on the
/// whole row reads the prose as the setting. Segments are what the
/// sheet's own two-space gaps divide the row into.
fn value_of(studio: &mut Hermetic, label: &str) -> String {
    let row = row_with(studio, label);
    let segments: Vec<&str> = row
        .split("  ")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    let at = segments
        .iter()
        .position(|segment| segment.contains(label))
        .unwrap_or_else(|| panic!("no segment carries {label:?}: {row}"));
    segments
        .get(at + 1)
        .map(|segment| segment.trim_matches('│').trim().to_owned())
        .unwrap_or_else(|| panic!("the row has no value segment: {row}"))
}

/// What a switch reads, past its glyph.
fn switch_on(studio: &mut Hermetic, label: &str) -> bool {
    value_of(studio, label)
        .split_whitespace()
        .any(|word| word == "on")
}

/// Open the sheet on its first page.
fn open_sheet(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    studio.settle();
    assert!(
        row_with(studio, "launch on").contains("▸"),
        "the sheet opens with its first row selected:\n{}",
        studio.rows().join("\n")
    );
}

/// Open the sheet on the Advanced page.
fn open_advanced(studio: &mut Hermetic) {
    open_sheet(studio);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
}

/// Open the sheet on the About page - the sixth of six tabs.
fn open_about(studio: &mut Hermetic) {
    open_sheet(studio);
    for _ in 0..5 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
}

/// The first whole number on a row: what a ladder reads, whatever
/// decoration (`(default)`, `· set on`) follows it.
fn leading_number(row: &str) -> usize {
    row.split_whitespace()
        .find_map(|word| word.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("no number on row: {row}"))
}

/// The row's percentage, as the opacity rows print it.
fn percent(row: &str) -> u32 {
    row.split_whitespace()
        .find_map(|word| {
            word.strip_suffix('%')
                .and_then(|digits| digits.parse().ok())
        })
        .unwrap_or_else(|| panic!("no percentage on row: {row}"))
}

/// A switch flips on every key that changes it: Right, Left and Space all
/// toggle it.
#[test]
fn switches_flip_on_every_changing_key() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ minimap");

    let reads_on = |studio: &mut Hermetic| switch_on(studio, "minimap");
    let start = reads_on(&mut studio);

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(reads_on(&mut studio), !start, "Right flipped the switch");

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(reads_on(&mut studio), start, "Space flipped it back");

    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        reads_on(&mut studio),
        !start,
        "and Left flips like the rest"
    );
}

/// A ladder steps forward, wraps to its start, and Left steps back into
/// the wrap from the other side - the metric detail's four rungs are the
/// ladder every other ladder is.
#[test]
fn ladders_step_wrap_both_ways() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ metric detail");

    const RUNGS: [&str; 4] = ["none", "basic", "advanced", "full"];
    let rung = |studio: &mut Hermetic| {
        let value = value_of(studio, "metric detail");
        RUNGS
            .iter()
            .find(|name| value.starts_with(**name))
            .copied()
            .unwrap_or_else(|| panic!("the row reads no rung of the ladder: {value}"))
    };
    let start = rung(&mut studio);

    let mut walked = Vec::new();
    for _ in 0..4 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        walked.push(rung(&mut studio));
    }
    assert_eq!(
        walked
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4,
        "four steps visited four different rungs: {walked:?}"
    );
    assert_eq!(
        *walked.last().unwrap(),
        start,
        "and the fourth wrapped back to the start"
    );

    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_ne!(
        rung(&mut studio),
        start,
        "Left from the start steps back into the wrap"
    );
}

/// The caret shape walks its six shapes in order - the row is the only
/// place the terminal's caret contract is visible from the keyboard.
#[test]
fn caret_shape_walks_its_six_shapes() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ caret shape");
    assert_eq!(
        value_of(&mut studio, "caret shape"),
        "steady bar",
        "the sheet starts on the shape the studio ships with"
    );

    for expected in [
        "blinking bar",
        "steady block",
        "blinking block",
        "steady underline",
        "blinking underline",
        "steady bar",
    ] {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        let value = value_of(&mut studio, "caret shape");
        assert_eq!(value, expected, "the ladder reached {expected}");
    }
}

/// The rendering ladder only offers what this terminal can draw: the
/// hermetic terminal claims no fine glyphs and no pixel grid, so the row
/// cycles Automatic and Cells and never advertises a tier that would
/// silently fall back.
#[test]
fn rendering_steps_only_through_what_the_terminal_can_draw() {
    let mut studio = hermetic();
    open_advanced(&mut studio);
    select_row(&mut studio, "▸ rendering");
    assert!(
        value_of(&mut studio, "rendering").starts_with("Automatic"),
        "the sheet starts on Automatic: {}",
        value_of(&mut studio, "rendering")
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    let value = value_of(&mut studio, "rendering");
    assert!(
        value.starts_with("Cells"),
        "the next step is the tier this terminal can draw: {value}"
    );

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    let value = value_of(&mut studio, "rendering");
    assert!(
        value.starts_with("Automatic"),
        "and the ladder wraps inside what is supported: {value}"
    );
    assert!(
        !value.contains("Fine") && !value.contains("Kitty"),
        "no unsupported tier was ever offered: {value}"
    );
}

/// The frame-rate ladder reads fast-first, so its arrows run the other
/// way round: Left speeds the screen up, Right slows it back down.
#[test]
fn frame_rate_reads_the_arrows_the_other_way_round() {
    let mut studio = hermetic();
    open_advanced(&mut studio);
    select_row(&mut studio, "▸ frame rate");

    let rate = |studio: &mut Hermetic| leading_number(&value_of(studio, "frame rate"));
    let start = rate(&mut studio);

    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    let left = rate(&mut studio);
    assert_ne!(left, start, "Left stepped the ladder");

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(rate(&mut studio), start, "and Right stepped it back");
}

/// Changing the polyphony names the voice count in the status line.
#[test]
fn max_polyphony_names_the_voice_count() {
    let mut studio = hermetic();
    open_advanced(&mut studio);
    select_row(&mut studio, "▸ max polyphony");

    let voices = |studio: &mut Hermetic| leading_number(&value_of(studio, "max polyphony"));
    let start = voices(&mut studio);

    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    let stepped = voices(&mut studio);
    assert_ne!(stepped, start, "the ladder stepped");
    assert_eq!(
        studio.status(),
        format!("polyphony: {stepped} voices"),
        "and the status names the count"
    );

    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(voices(&mut studio), start, "Left steps back");
    assert_eq!(studio.status(), format!("polyphony: {start} voices"));
}

/// The computer piano's two rows: the sound cycles its three oscillators
/// and the volume clamps at both ends of its 0-200% travel - a volume
/// that crept past 200 would be louder than the row ever said.
#[test]
fn piano_rows_cycle_and_clamp() {
    let mut studio = hermetic();
    open_advanced(&mut studio);

    select_row(&mut studio, "▸ piano sound");
    assert_eq!(value_of(&mut studio, "piano sound"), "triangle");
    let mut seen = Vec::new();
    for _ in 0..3 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        seen.push(value_of(&mut studio, "piano sound"));
    }
    assert!(
        seen.iter().any(|value| value == "sine")
            && seen.iter().any(|value| value == "triangle")
            && seen.iter().any(|value| value == "square"),
        "three steps walked the engine's own oscillators: {seen:?}"
    );
    assert_eq!(
        seen.last().unwrap(),
        "triangle",
        "and wrapped back to the default triangle: {seen:?}"
    );

    select_row(&mut studio, "▸ piano volume");
    let volume = |studio: &mut Hermetic| percent(&value_of(studio, "piano volume"));
    for _ in 0..25 {
        if volume(&mut studio) == 200 {
            break;
        }
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
    }
    assert_eq!(
        volume(&mut studio),
        200,
        "the ladder reaches its ceiling: {}",
        row_with(&mut studio, "piano volume")
    );
    assert_eq!(
        studio.status(),
        "settings kept - drawing with cells",
        "a changed setting says what the studio kept"
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        volume(&mut studio),
        200,
        "and clamps there: {}",
        row_with(&mut studio, "piano volume")
    );

    for _ in 0..30 {
        if volume(&mut studio) == 0 {
            break;
        }
        studio.press(KeyCode::Left, KeyModifiers::NONE);
        studio.settle();
    }
    assert_eq!(
        volume(&mut studio),
        0,
        "the floor is zero: {}",
        row_with(&mut studio, "piano volume")
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        volume(&mut studio),
        0,
        "and clamps there too: {}",
        row_with(&mut studio, "piano volume")
    );

    for _ in 0..13 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
    }
    studio.settle();
    assert_eq!(
        volume(&mut studio),
        130,
        "thirteen steps of ten land on the shipped default: {}",
        row_with(&mut studio, "piano volume")
    );
}

/// The syntax check's three modes each say what they still do - marks
/// going away could otherwise read as the score having been fixed.
#[test]
fn syntax_check_modes_each_say_what_they_still_do() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ syntax check");

    let expected = |value: &str| match value {
        "on update" => "syntax check on update - errors are marked where an update is refused",
        "full" => "syntax check full - errors are marked a pause after typing stops",
        _ => {
            "syntax check off - nothing is marked; the footer and the log still say why an update is refused"
        }
    };

    let start = value_of(&mut studio, "syntax check");
    for _ in 0..3 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        let value = value_of(&mut studio, "syntax check");
        assert_eq!(
            studio.status(),
            expected(&value),
            "the mode explains itself"
        );
    }
    assert_eq!(
        value_of(&mut studio, "syntax check"),
        start,
        "three rungs wrapped the ladder back to its start"
    );
}

/// The master limiter row is the default for new sets. A fresh set has made
/// no limiter choice of its own, so the row flips and the status is the
/// generic one. The mixer's suite covers the shadowed case.
#[test]
fn master_limiter_default_flips_for_a_fresh_set() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ master limiter");

    let was_on = switch_on(&mut studio, "master limiter");

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    let row = row_with(&mut studio, "master limiter");
    assert_eq!(
        switch_on(&mut studio, "master limiter"),
        !was_on,
        "the default flipped for sets to come: {row}"
    );
    assert_eq!(
        studio.status(),
        "settings kept - drawing with cells",
        "a set that has not answered leaves the row unshadowed: {}",
        studio.status()
    );
}

/// The opacity rows clamp at 100%, and touching one claims it: the
/// theme stops overwriting it, so what the sheet shows is what a reopen
/// shows - together with a plain ladder row that rides the same prefs
/// file.
#[test]
fn opacity_rows_clamp_and_are_claimed_across_a_reopen() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ ui opacity");
    let opacity = |studio: &mut Hermetic| percent(&value_of(studio, "ui opacity"));

    for _ in 0..40 {
        if opacity(&mut studio) == 100 {
            break;
        }
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
    }
    assert_eq!(
        opacity(&mut studio),
        100,
        "the ladder reaches full: {}",
        row_with(&mut studio, "ui opacity")
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        opacity(&mut studio),
        100,
        "and stops there: {}",
        row_with(&mut studio, "ui opacity")
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    let left_with = opacity(&mut studio);
    assert!(
        left_with < 100,
        "Left stepped back off the ceiling: {left_with}%"
    );

    select_row(&mut studio, "▸ caret shape");
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert!(
        row_with(&mut studio, "caret shape").contains("steady block"),
        "setup: a second row was changed"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // Prefs are written on a debounce: the wait is what makes the quit
    // honest.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);

    open_sheet(&mut studio);
    select_row(&mut studio, "▸ ui opacity");
    assert_eq!(
        percent(&value_of(&mut studio, "ui opacity")),
        left_with,
        "the claimed opacity survived the reopen: {}",
        row_with(&mut studio, "ui opacity")
    );
    select_row(&mut studio, "▸ caret shape");
    assert!(
        row_with(&mut studio, "caret shape").contains("steady block"),
        "and so did the caret: {}",
        row_with(&mut studio, "caret shape")
    );
}

/// `full paths` is a privacy switch with a visible job: messages about
/// files say the folder, not just the name - here the drop of an
/// already-open score, which names its path in the refusal.
#[test]
fn full_paths_puts_the_folder_in_the_message() {
    let mut studio = hermetic();
    let starter = studio.set_directory().join("first.strudel");
    let folder_name = studio
        .set_directory()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    open_sheet(&mut studio);
    select_row(&mut studio, "▸ full paths");
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert!(
        switch_on(&mut studio, "full paths"),
        "the switch flipped on: {}",
        row_with(&mut studio, "full paths")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    studio.paste(&starter.display().to_string());
    studio.settle();
    let status = studio.status().to_owned();
    assert!(
        status.contains("is already open"),
        "the drop of an open score still says so: {status}"
    );
    assert!(
        status.contains(&folder_name),
        "and with the switch on it names the folder, not just the file: {status}"
    );
}

/// The folder rows are doors, not ladders: Enter replaces the sheet with
/// the picker, and the picker's Esc comes back to the sheet - a settings
/// prompt remembers where it was opened from.
#[test]
fn folder_rows_open_their_picker_and_esc_returns_to_the_sheet() {
    let mut studio = hermetic();
    open_sheet(&mut studio);

    select_row(&mut studio, "▸ sets folder");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "sets folder - type a path, or Tab to browse and Enter on . · Esc back"
    );
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("keeps new sets there")),
        "the picker is up with its own promise:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "set prompt closed");
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Settings),
        "the Esc came back to the sheet that opened the picker"
    );
    assert!(
        row_with(&mut studio, "launch on").contains("launch on"),
        "and the sheet is still standing:\n{}",
        studio.rows().join("\n")
    );

    select_row(&mut studio, "▸ recordings folder");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "recordings folder - type a path, or Tab to browse and Enter on . · Esc back"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "set prompt closed");
}

/// The About page reports on the current terminal: its name (`rustel-e2e`
/// in the hermetic fixture), the drawing tier, the shortcut profile, the
/// feature checklist and the capability report. It prints the paging keys
/// where the page can scroll.
#[test]
fn about_page_reads_the_terminal_it_is_running_in() {
    let mut studio = hermetic();
    open_about(&mut studio);

    let rows = studio.rows();
    let screen = rows.join("\n");
    for expected in [
        "Terminal: rustel-e2e",
        "Rendering: Automatic (Cells)",
        "Shortcut profile: automatic (rustel-e2e)",
        "Terminal features",
        "Full colour",
        "PgUp/PgDn scroll",
        "build",
    ] {
        assert!(
            screen.contains(expected),
            "the About page reads {expected:?}:\n{screen}"
        );
    }

    // Tab is the sheet's own key on every page: from the last tab it
    // wraps to the first.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
    assert!(
        row_with(&mut studio, "launch on").contains("▸"),
        "Tab wrapped the strip back to the settings page:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.focus(), None, "and Esc closed the sheet");
}

/// On a terminal too short for the whole page, the About content scrolls -
/// End takes the top out of view and down to the report, Home brings
/// the top back. A page that could not scroll would hide its own answer
/// about the terminal.
#[test]
fn about_page_scrolls_when_the_terminal_is_short() {
    let mut studio = hermetic_sized(120, 20);
    open_about(&mut studio);
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("Terminal: rustel-e2e")),
        "setup: the page opens at its top:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.settle();
    let rows = studio.rows();
    assert!(
        !rows.iter().any(|row| row.contains("Terminal:")),
        "End scrolled the top out of view:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().any(|row| row.contains("realtime priority")),
        "and brought the capability report's tail up:\n{}",
        rows.join("\n")
    );

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("Terminal: rustel-e2e")),
        "Home scrolled back to the top:\n{}",
        studio.rows().join("\n")
    );
}

/// The sheet on a short terminal: Home and End reach the first and last
/// rows, the selection wraps at both ends, and PgDn moves a page - the
/// row you wanted being the last one is exactly the case the scroll
/// exists for.
#[test]
fn the_sheet_reaches_its_last_rows_on_a_short_terminal() {
    let mut studio = hermetic_sized(120, 24);
    open_sheet(&mut studio);

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.settle();
    assert!(
        selected_row(&mut studio).is_some_and(|row| row.contains("full paths")),
        "End reached the last row: {:?}",
        selected_row(&mut studio)
    );

    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.settle();
    assert!(
        selected_row(&mut studio).is_some_and(|row| row.contains("launch on")),
        "Down from the last row wraps to the first: {:?}",
        selected_row(&mut studio)
    );

    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.settle();
    assert!(
        selected_row(&mut studio).is_some_and(|row| row.contains("master limiter")),
        "Down steps the rows in order: {:?}",
        selected_row(&mut studio)
    );

    studio.press(KeyCode::PageDown, KeyModifiers::NONE);
    studio.settle();
    let row = selected_row(&mut studio).expect("a row stays selected");
    assert!(
        !row.contains("master limiter"),
        "PgDn moved more than one row: {row}"
    );
}

#[test]
fn settings_and_advanced_keep_their_place_until_studio_restarts() {
    let mut studio = hermetic_sized(120, 24);
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ caret shape");
    let selection = |studio: &mut Hermetic| {
        studio
            .rows()
            .into_iter()
            .enumerate()
            .find(|(_, row)| row.contains('▸') && row.contains('│'))
            .expect("the selected settings row is visible")
    };
    let settings_position = selection(&mut studio);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.chord("ctrl+o");
    studio.settle();
    assert_eq!(selection(&mut studio), settings_position);

    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    select_row(&mut studio, "▸ frame rate");
    let advanced_position = selection(&mut studio);
    studio.chord("ctrl+o");
    studio.chord("ctrl+o");
    studio.settle();
    assert_eq!(selection(&mut studio), advanced_position);

    studio.press(KeyCode::BackTab, KeyModifiers::SHIFT);
    studio.settle();
    assert_eq!(selection(&mut studio), settings_position);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(selection(&mut studio), advanced_position);

    let mut studio = reopen_over(studio);
    open_sheet(&mut studio);
}

/// Tab walks the strip's six pages in order and wraps - each page
/// recognised by a row only it draws.
#[test]
fn tab_cycles_the_six_pages_and_wraps() {
    let mut studio = hermetic();
    open_sheet(&mut studio);

    for expected in [
        "max polyphony",        // advanced
        "not assigned",         // mapping: the slot boxes
        "automatic",            // keybinds: the terminal-profile row
        "fetch imports",        // sources
        "Terminal: rustel-e2e", // about
        "launch on",            // and back to settings
    ] {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
        studio.settle();
        let rows = studio.rows().join("\n");
        assert!(
            rows.contains(expected),
            "the next tab reads {expected:?}:\n{rows}"
        );
    }
}
