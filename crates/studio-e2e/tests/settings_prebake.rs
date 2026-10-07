//! Open settings with Ctrl-O or Ctrl-Shift-P. Check persisted settings,
//! prebake validation and saved set setup.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, reopen_over, row_containing};

/// The biggest-sound ceiling is a setting now, not a constant a rebuild
/// changes: the advanced page's row names the rung and roughly how long
/// a take it holds, and ←/→ walk the ladder. What breaks if these fail:
/// a musician with a long take has no way to raise the ceiling, or the
/// row promises a size the player then refuses to honour.
#[test]
fn the_biggest_sound_is_a_ladder_a_setting_walks() {
    let mut studio = hermetic();
    studio.chord("ctrl+o");
    row_containing(&studio.rows(), "Tab pages");

    // Page over to Advanced, where the memory rows live. The walk reads
    // the sheet's own selection marker: Right acts on the row the sheet
    // says is selected, not on any row that happens to be visible.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ biggest sound"))
        {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ biggest sound")),
        "the walk selected the biggest-sound row:\n{}",
        studio.rows().join("\n")
    );

    // The row says the rung and its minutes - a musician counts in
    // seconds of take, not in mebibytes.
    let read = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio
            .rows()
            .iter()
            .find(|row| row.contains("biggest sound"))
            .expect("the row is on the advanced page")
            .clone()
    };
    let row = read(&mut studio);
    assert!(
        row.contains("256 MiB"),
        "the default rung is Roomy: {row:?}"
    );
    assert!(row.contains("min"), "the rung says what it holds: {row:?}");

    // → climbs the ladder, ↓ comes back down: the value follows the keys.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("512 MiB"),
        "→ raised the ceiling"
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("1 GiB"),
        "the ladder tops out at the player's own hard maximum"
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("512 MiB"),
        "← came back down a rung"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// Walk the sheet's rows by label until the given one is selected -
/// by what is drawn, not by row arithmetic, so the suite holds under
/// every feature set's row list.
fn select_row(studio: &mut rustel_studio_e2e::Hermetic, marker: &str) {
    for _ in 0..30 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// Ctrl+O opens the sheet, the sheet owns the keyboard, Esc closes it and
/// hands the keys back.
#[test]
fn ctrl_o_opens_the_sheet_and_esc_closes_it() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    // The sheet's status is only "settings". The terminal's answers are in
    // the log, written once at startup.
    assert_eq!(studio.status(), "settings");
    row_containing(&studio.rows(), "launch on");
    // The sheet is grouped and longer than it was: the prebake rows sit
    // below the fold, so walk to one rather than assume it is on screen.
    select_row(&mut studio, "global prebake");
    row_containing(&studio.rows(), "Enter opens it");

    // The closing chord the sheet was opened with works too.
    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), None);

    // And Esc, from the sheet itself.
    studio.chord("ctrl+o");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
}

/// A switch flips live: the row redraws, the status says it is kept, and
/// the editor takes its keys back the moment the sheet is gone.
#[test]
fn a_switch_flips_live_and_says_so() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "highlights");
    row_containing(&studio.rows(), "● on");

    // Left turns it off, right back on, both remembered at once.
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "settings kept - drawing with cells",
        "the change is live; no restart, no apply step"
    );
    assert!(row_containing(&studio.rows(), "highlights") > 0);

    // The toggle is on the drawn row: off, then on again.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    row_containing(&studio.rows(), "● on");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_score("$: s(\"bd\")");
    assert_eq!(
        studio.score(),
        "$: s(\"bd\")",
        "the editor owns the keys again"
    );
}

/// The clock rows are on the devices panel's MIDI tab, below the ports they
/// choose between. On a host with no MIDI ports they read "none", and a
/// step says to tick a port's box. The test names no real port, so it holds
/// on any host: it asserts the rows' place, their order and the no-ports
/// arithmetic.
#[test]
fn the_clock_rows_say_what_they_do_and_honour_no_ports() {
    let mut studio = hermetic();

    // Not on the settings sheet any more.
    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    assert!(
        !studio.rows().iter().any(|row| row.contains("clock out")),
        "the clock rows have left the sheet"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // On the devices panel's MIDI tab.
    studio.chord("ctrl+p");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    let rows = studio.rows();
    let input = row_containing(&rows, "clock in");
    let out = row_containing(&rows, "clock out");
    assert!(input < out, "clock in, then clock out");
    assert!(rows[input].contains("< none >"), "{}", rows[input]);
    assert!(rows[out].contains("< none >"), "{}", rows[out]);

    // A hermetic host lists no ports, and says so where the column names go.
    row_containing(&rows, "no MIDI ports found");

    // Stepping with nothing ticked keeps the choice at none, says where a
    // port would come from, and leaves the panel open.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    let rows = studio.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("clock in") && row.contains("< none >")),
        "no ports, no choice: the row stays none\n{}",
        rows.join("\n")
    );
    assert!(studio.status().contains("tick"), "{}", studio.status());
    assert_eq!(studio.focus(), Some(PanelKind::Devices));
}

/// Following an outside clock is visible: the header carries the `⇄` chip
/// with the tempo heard. With no port ticked there is nothing to follow, so
/// no engine command is even sent and the chip stays dark. The engine's own
/// open failure is covered by the midi-clock unit tests on a host with a
/// virtual port.
#[test]
fn a_clock_choice_that_cannot_open_is_reported_and_the_chip_stays_dark() {
    let mut studio = hermetic();

    studio.chord("ctrl+p");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    // Clock in is the first row with no ports above it.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    let rows = studio.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("clock in") && row.contains("< none >")),
        "no ports: clock in stays none\n{}",
        rows.join("\n")
    );

    // No clock followed: the header has no ⇄ chip with a tempo.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    let rows = studio.rows();
    let header = rows
        .iter()
        .find(|row| row.contains("bpm"))
        .expect("the header paints");
    assert!(
        !header.contains("⇄"),
        "no clock in, no following chip: {header}"
    );
}

/// Enter on a prebake row opens it as a tab at the end of the strip: setup
/// before the scores, with its starter text and its own two chords.
#[test]
fn enter_opens_a_prebake_tab() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.focus(),
        None,
        "the tab hands the caret to the editor"
    );
    assert_eq!(
        studio.status(),
        "prebake (local) - setup before the scores; ^Enter applies it, ^W closes the tab"
    );
    assert_eq!(
        studio.scene_names().last(),
        Some(&"prebake (local)".to_owned()),
        "setup sits at the end of the strip, after every score"
    );
    assert!(
        studio.score().contains("Setup for every score in this set"),
        "an untouched prebake starts from its starter text"
    );
    assert!(!studio.is_dirty(), "opened clean, from the store");
}

/// Ctrl+S on a prebake tab checks it, keeps it, runs it - and the status
/// says so once the engine has answered.
#[test]
fn ctrl_s_applies_a_prebake() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("globalThis.riff = () => note(\"c e g\")");
    assert!(studio.is_dirty());

    studio.chord("ctrl+s");
    studio.settle();

    assert_eq!(studio.status(), "prebake (local) applied");
    assert!(!studio.is_dirty(), "applied text is the stored text");

    // Closing the tab keeps it: the settings sheet says what is stored.
    studio.chord("ctrl+w");
    assert_eq!(
        studio.status(),
        "closed prebake (local) - the settings sheet opens it again"
    );
    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    let row = row_containing(&studio.rows(), "▸ local prebake");
    assert!(
        studio.rows()[row].contains("applied"),
        "the sheet shows the verdict: {}",
        studio.rows()[row]
    );
}

/// A prebake the checker refuses is not applied, but is still kept - the
/// text stays in the tab, and the store is told only on the way out.
#[test]
fn a_refused_prebake_is_not_applied_but_still_kept() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    // Setup has rules: setcps belongs to a score, not to setup.
    studio.set_score("setcps(3)");
    studio.chord("ctrl+s");

    assert!(
        studio.status().starts_with("refused - "),
        "the refusal says why: {}",
        studio.status()
    );
    assert!(
        studio.status().contains("the setup was not applied"),
        "{}",
        studio.status()
    );
    assert!(studio.is_dirty(), "refused text is not kept as stored");

    // And the tab survives with the refused text in it.
    assert_eq!(
        studio.scene_names().last(),
        Some(&"prebake (local)".to_owned())
    );
    assert!(studio.score().contains("setcps(3)"));
}

/// A blank prebake applies as nothing: the store is emptied, and the
/// status says there was nothing to run.
#[test]
fn an_emptied_prebake_applies_as_nothing() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("");
    studio.chord("ctrl+s");
    studio.settle();

    assert_eq!(studio.status(), "prebake (local) is empty - nothing to run");
}

/// The local prebake is kept with the set: a studio reopened over the same
/// folder still has it, and the settings sheet's row says how many lines.
#[test]
fn the_local_prebake_survives_a_reopen() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("globalThis.riff = () => note(\"c e g\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(studio.status(), "prebake (local) applied");

    let mut reopened = reopen_over(studio);
    assert_eq!(
        reopened.status(),
        "prebake (local) applied",
        "a stored prebake runs again over the same set"
    );
    reopened.chord("ctrl+o");
    select_row(&mut reopened, "▸ local prebake");
    let row = row_containing(&reopened.rows(), "▸ local prebake");
    assert!(
        reopened.rows()[row].contains("1 line"),
        "the sheet counts the stored lines: {}",
        reopened.rows()[row]
    );
}

/// The audio-out latency row is on the Advanced page, in the "Audio out"
/// group. ←/→ step the buffer ladder, the readout names the step, Esc keeps
/// the choice, and a new sheet reads it back. Engine unit tests cover the
/// ladder's arithmetic.
#[test]
fn the_output_latency_row_walks_and_is_kept() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    // Page over to Advanced, where the machine rows live.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    select_row(&mut studio, "▸ audio out latency");

    let read = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio
            .rows()
            .iter()
            .find(|row| row.contains("audio out latency"))
            .expect("the row stays on the page")
            .clone()
    };
    assert!(
        read(&mut studio).contains("automatic"),
        "the default keeps the engine's own policy: {:?}",
        read(&mut studio)
    );

    // → walks up the ladder, ← comes back down: the readout follows.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("32 frames"),
        "→ took the first rung: {:?}",
        read(&mut studio)
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("64 frames"),
        "the ladder doubles: {:?}",
        read(&mut studio)
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("32 frames"),
        "← came back down a rung"
    );

    // Esc keeps the choice and the Advanced page's selected row.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.chord("ctrl+o");
    row_containing(&studio.rows(), "▸ audio out latency");
    assert!(
        read(&mut studio).contains("32 frames"),
        "the kept rung survives the sheet closing: {:?}",
        read(&mut studio)
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// The frame-rate row on Advanced steps the whole ladder, including the two
/// steps above sixty. The terminal can limit those rates, so the row shows
/// the requested rate without a clamp. → is faster, ← is slower, and the
/// ladder wraps both ways.
#[test]
fn the_frame_rate_row_climbs_past_sixty() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    select_row(&mut studio, "▸ frame rate");

    let read = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio
            .rows()
            .iter()
            .find(|row| row.contains("frame rate"))
            .expect("the row stays on the page")
            .clone()
    };
    assert!(
        read(&mut studio).contains("60 fps"),
        "the default is the loop's own sixty: {:?}",
        read(&mut studio)
    );

    // → climbs: past sixty to the asked-for rungs, then round the top to
    // the slowest.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("120 fps"),
        "→ climbs past sixty: {:?}",
        read(&mut studio)
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("240 fps"),
        "the top of the ladder: {:?}",
        read(&mut studio)
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("8 fps"),
        "the ladder wraps past the top: {:?}",
        read(&mut studio)
    );

    // ← walks back down the same rungs.
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("240 fps"),
        "← wraps back to the top: {:?}",
        read(&mut studio)
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("60 fps"),
        "two rungs down is the default: {:?}",
        read(&mut studio)
    );

    // Esc keeps the choice; the sheet reads it back and → climbs again -
    // a ladder runs both ways on the same keys.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.chord("ctrl+o");
    row_containing(&studio.rows(), "▸ frame rate");
    assert!(
        read(&mut studio).contains("120 fps"),
        "the kept rung survives the sheet closing: {:?}",
        read(&mut studio)
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("60 fps"),
        "← comes back down: {:?}",
        read(&mut studio)
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// The Sources page lists the shipped packs after the cache controls and
/// the imports, below a rule, each with its address and marked apart from
/// the imports. `c` and `C` start real downloads, so the test does not
/// press them. The list's editing keys change nothing on a shipped pack.
#[test]
fn the_shipped_packs_are_listed_under_the_imports() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    for _ in 0..4 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    let rows = studio.rows();
    let at = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no row reads {needle:?}:\n{}", rows.join("\n")))
    };
    let rule = at("default samples");
    let piano = at("◇ piano");
    let fonts = at("◇ gm soundfonts");
    assert!(rule < piano && piano < fonts, "{}", rows.join("\n"));
    assert!(
        rows.iter().any(|row| row.contains("cache all")),
        "the cache controls lead the page:\n{}",
        rows.join("\n")
    );
    // The Advanced page no longer carries the row: it moved here.
    assert!(
        !rows.iter().any(|row| row.contains("cache whole library")),
        "{}",
        rows.join("\n")
    );

    // Walk onto a pack: it shows its address, and the editing keys have
    // nothing to do on it - the page still has every pack afterwards.
    select_row(&mut studio, "▸ ◇ piano");
    let rows = studio.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("▸ ◇ piano")
                && row.contains("https://strudel.b-cdn.net/piano.json")),
        "the selected pack shows where its list lives:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().any(|row| row.contains("c caches this pack")),
        "the hint on a shipped pack says how to cache it:\n{}",
        rows.join("\n")
    );
    assert!(
        !rows.iter().any(|row| row.contains("C caches all")),
        "cache-all is a top control, not a pack shortcut:\n{}",
        rows.join("\n")
    );
    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.press(KeyCode::Char(']'), KeyModifiers::NONE);
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("▸ ◇ piano")),
        "a shipped pack cannot be removed, turned off or moved:\n{}",
        rows.join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}
