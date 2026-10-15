//! Check device-picker and log-panel tabs, keys, empty states and focus.
//! Do not require particular host devices.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_sized, row_containing};

/// Ctrl+P opens the device picker; it owns the keyboard, says what its
/// keys do, and hands the keys back when it closes - by either chord.
#[test]
fn the_device_picker_opens_closes_and_hands_the_keys_back() {
    let mut studio = hermetic();

    studio.chord("ctrl+p");
    assert_eq!(studio.focus(), Some(PanelKind::Devices));
    assert_eq!(
        studio.status(),
        "devices - Enter picks, Tab switches list, Esc closes"
    );
    row_containing(&studio.rows(), "audio out");

    // Esc closes and the score owns the keyboard again.
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.status(), "devices closed");
    assert_eq!(studio.focus(), None);

    // Ctrl+P again, closed by its own chord this time.
    studio.chord("ctrl+p");
    assert_eq!(studio.focus(), Some(PanelKind::Devices));
    studio.chord("ctrl+p");
    assert_eq!(studio.focus(), None, "the picker's own chord closes it too");
}

/// Tab walks the picker's four families in order and wraps; BackTab walks
/// the other way. One MIDI tab serves both directions.
#[test]
fn tab_walks_the_device_families() {
    let mut studio = hermetic();

    studio.chord("ctrl+p");
    let tabs = ["audio out", "audio in", " midi ", "gamepads"];
    for expected in tabs {
        row_containing(&studio.rows(), expected);
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    // Wrapped back to the first family, still open.
    assert_eq!(studio.focus(), Some(PanelKind::Devices));
    row_containing(&studio.rows(), "audio out");
    // And there is no longer a tab each way.
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("midi out") || row.contains("midi in ")),
        "one MIDI tab, not one each way"
    );

    // BackTab walks the other way.
    studio.press(KeyCode::BackTab, KeyModifiers::NONE);
    row_containing(&studio.rows(), "gamepads");
}

/// Enter on a MIDI port copies its name to the clipboard and closes -
/// the thing a musician actually wants from the picker.
#[test]
fn enter_on_a_midi_port_copies_a_snippet() {
    let mut studio = hermetic();

    studio.chord("ctrl+p");
    // Onto a MIDI family. A hermetic host lists no ports; the honest
    // case is whatever is there - Enter either copies (a port exists)
    // or leaves the list as it was (nothing to pick). Both are the app
    // behaving; the panel closes only when something was picked.
    for _ in 0..2 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let focus = studio.focus();
    if focus.is_none() {
        // A port was there and was picked. Its name is the host's - an
        // IAC bus on macOS, nothing on a headless runner - so assert the
        // copied shape, not the name. (With no port at all the shape
        // still holds: `copied ""`.)
        let status = studio.status();
        assert!(
            status.starts_with("copied \"")
                && status.ends_with("\" - paste it into .midi() or midin()"),
            "a picked port copies its bare name: {status}"
        );
    } else {
        assert_eq!(
            focus,
            Some(PanelKind::Devices),
            "nothing to pick, nothing closed"
        );
    }
}

/// The log panel opens with Ctrl+Shift+D (and F9), says where it also
/// writes, and closes with Esc.
#[test]
fn the_log_panel_opens_and_closes() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+d");
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    assert!(
        studio.status().starts_with("log - also in "),
        "the fixture's log lives in the set's config folder: {}",
        studio.status()
    );
    row_containing(&studio.rows(), "opened -");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert_eq!(studio.errors(), None);

    // F9, the portable spelling, opens it again.
    studio.press(KeyCode::F(9), KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
}

/// Memory shares the log panel's border, with log entries on the left and
/// the breakdown on the right. The log path varies by host, so check the
/// visible columns and content instead of comparing a whole-screen golden.
#[test]
fn the_log_shares_its_panel_with_memory() {
    let mut studio = hermetic_sized(120, 40);
    studio.press(KeyCode::F(9), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.focus(), Some(PanelKind::Log));

    let rows = studio.rows();
    let title = &rows[row_containing(&rows, " mem ")];
    let memory_column = title[..title.find(" mem ").unwrap()].chars().count();
    assert!(title.contains(" log - "), "log title is present: {title}");
    assert!(
        memory_column > 60,
        "memory is to the right of the log: {title}"
    );

    for label in ["memory", "sounds", "script engine"] {
        let row = &rows[row_containing(&rows, label)];
        let column = row[..row.find(label).unwrap()].chars().count();
        assert!(
            column >= memory_column,
            "{label} is in the memory column: {row}"
        );
    }
    let entry = &rows[row_containing(&rows, "opened -")];
    let entry_column = entry[..entry.find("opened -").unwrap()].chars().count();
    assert!(
        entry_column < memory_column,
        "log entries stay on the left: {entry}"
    );
}

/// The terminal's answers go to the log once, at startup, and not to the
/// status line each time the settings sheet opens.
#[test]
fn the_terminal_summary_is_a_startup_log_entry() {
    // Wide enough that the summary line is not clipped at the panel's
    // right edge - it grows by a phrase every time the studio learns to
    // ask the terminal something new, and the tier it picked is last.
    let mut studio = hermetic_sized(320, 40);

    studio.chord("ctrl+shift+d");
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    let at = row_containing(&studio.rows(), "rustel-e2e · truecolor");
    let row = studio.rows()[at].clone();
    assert!(
        row.contains("keyboard ✗") && row.contains("cell ?"),
        "the hermetic terminal's full summary is in the log: {row}"
    );
    assert!(
        row.contains("→ drawing "),
        "the summary says the tier it picked: {row}"
    );
}

/// The output stream's facts (host, device, rate, frames, cost) go to the
/// log once, when the stream opens. They do not replace the install status
/// and do not raise the warning badge. A built-in synth keeps the score
/// independent of sample downloads.
#[test]
fn the_stream_facts_are_a_log_entry_not_the_status_line() {
    let mut studio = hermetic_sized(160, 40);

    studio.set_score("$: note(36).s(\"sine\")");
    studio.chord("ctrl+s");
    assert!(
        matches!(
            studio.status(),
            "evaluating on the native engine…" | "playing from the top"
        ),
        "audio stream facts do not replace the transport status: {}",
        studio.status()
    );
    assert_eq!(studio.errors(), None);
    assert!(
        !studio.rows().iter().any(|row| row.contains("⚠")),
        "a clean start raises no warning badge:\n{}",
        studio.rows().join("\n")
    );

    studio.settle();
    studio.chord("ctrl+shift+d");
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    let at = row_containing(&studio.rows(), "info  audio");
    let row = studio.rows()[at].clone();
    assert!(
        row.contains("silent · silent") && row.contains("frames"),
        "the log names the host, the device and the buffer: {row}"
    );
}

/// A warning raises the unseen badge; opening the log clears it - that
/// is what the panel is for.
#[test]
fn a_warning_badges_and_the_log_clears_it() {
    let mut studio = hermetic();

    // An unknown sound is the engine's warning.
    studio.set_score("$: s(\"no-such-bank\")");
    studio.chord("ctrl+s");
    studio.settle();
    // An unknown name resolves only after the sample manifests settle.
    // Evaluation sometimes answers before the asynchronous warning arrives.
    studio.pump_until(
        std::time::Duration::from_secs(10),
        "the badge shows while the missing-sound warning is unseen",
        |studio| studio.rows().iter().any(|row| row.contains("⚠")),
    );

    // Opening the log marks seen; the badge goes away.
    studio.chord("ctrl+shift+d");
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    row_containing(&studio.rows(), "no-such-bank");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        !studio.rows().iter().any(|row| row.contains("⚠")),
        "seen warnings stop badging:\n{}",
        studio.rows().join("\n")
    );
}

/// The log walks: Up goes back into older lines, End returns to following
/// the newest - the way a musician finds the line that explains things.
#[test]
fn the_log_walks_back_and_follows_again() {
    let mut studio = hermetic();

    // Enough lines to scroll: several updates.
    for _ in 0..3 {
        studio.set_score("$: s(\"bd\")");
        studio.chord("ctrl+s");
        studio.settle();
    }

    studio.chord("ctrl+shift+d");
    assert_eq!(studio.focus(), Some(PanelKind::Log));

    // Up leaves the newest line: the panel is scrolled, still open.
    studio.press(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Log));

    // End follows the newest again.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Log));

    // Home goes to the log's first line - the studio's opening line.
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    row_containing(&studio.rows(), "opened -");
}

/// The audio-in tab's contract: its hint says what a picked input is for
/// (`s("in")`, and the channels a multi-channel port exposes), and the
/// cursor arrives on `none`, so Enter cannot capture from a real interface.
///
/// Not covered: that a real pick is remembered in the prefs. That needs a
/// step past `none` onto hardware, which this suite must not do.
#[test]
fn the_audio_in_tab_spells_its_contract_and_arrives_on_none() {
    let mut studio = hermetic();

    studio.chord("ctrl+p");
    // Onto the audio-in family: out → in.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    row_containing(&studio.rows(), "audio in");
    // The hint is clipped to the panel's width, so assert on the stable
    // prefix - what a picked input is *for* is the contract.
    row_containing(&studio.rows(), "Enter makes it s(\"in\")");

    // Enter takes the row the cursor arrived on. For audio inputs that is
    // `none`, which leads the list precisely so the arriving cursor lands on
    // it, and which `devices.rs` says is never a selector: the engine is told
    // there is no input with `None`, and that string must not reach
    // `open_input`. So nothing is captured, and the panel stays.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !studio.status().starts_with("audio in → "),
        "Enter on the arriving row must not pick a device: {}",
        studio.status()
    );
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Devices),
        "nothing to pick, nothing closed"
    );
}

/// The footer's audio-in chip: the hermetic studio opens no input, so the
/// chip is `♪ no output` alone, with no `♩` half. An open input is named
/// after the `♩`, but that needs a real device.
#[test]
fn the_footer_chip_shows_no_input_where_none_is_open() {
    let mut studio = hermetic();

    let footer = studio.rows().last().expect("the footer paints").clone();
    assert!(
        footer.contains("♪ no output") && !footer.contains("♩"),
        "no input, no ♩: {footer}"
    );
}

/// The picker says what the open stream costs: under the list, above the
/// hint, `out … ms · N frames` with the limiter's runway beside it. The
/// stream is the suite's silent output, which the fixture opens: the next
/// evaluation starts it, and the line appears once a snapshot arrives.
/// `LatencyReport` unit tests cover the arithmetic.
#[test]
fn the_picker_reports_what_the_open_stream_costs() {
    let mut studio = hermetic();

    // Before anything is open there is no cost to report: the audio-out
    // tab reserves its rows anyway, but they hold no numbers.
    studio.chord("ctrl+p");
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("out ") && row.contains("ms")),
        "no stream, no cost line yet"
    );

    // Make no choice in the picker. The fixture already opened the silent
    // output, and the next evaluation starts the stream. `silent` is the
    // last output in the list and the cursor starts at 0, so `Down` plus
    // `Enter` would open a real sound card. The test only reads the
    // picker.
    studio.chord("ctrl+p");

    // …and the next evaluation opens it. The chip names what is audible.
    studio.chord("ctrl+s");
    studio.settle();
    std::thread::sleep(std::time::Duration::from_millis(300));
    studio.settle();
    let footer = studio.rows().last().expect("the footer paints").clone();
    assert!(
        footer.contains("♪ silent"),
        "the stream the cost belongs to is open: {footer}"
    );

    // Reopen the picker: the cost sits under the list, above the hint.
    studio.chord("ctrl+p");
    studio.settle();
    let rows = studio.rows();
    let cost = rows
        .iter()
        .find(|row| row.contains("│") && row.contains("out ") && row.contains("ms · "))
        .unwrap_or_else(|| {
            panic!(
                "the stream cost is on the audio-out tab:\n{}",
                rows.join("\n")
            )
        });
    // The silent output settles at 256 frames; the row says so. The
    // limiter is off on a fresh studio - the runway it would add is
    // named only when it is on - so first the row carries no limiter
    // clause, then turning it on puts it there.
    assert!(
        cost.contains("256 frames"),
        "the row names the buffer it costs: {cost}"
    );
    assert!(
        !cost.contains("limiter"),
        "a limiter that is off costs no runway: {cost}"
    );

    // Turn the limiter on - Transport ▸ Add limiter to set gives the set
    // one, switched in - and reopen the picker: the runway it adds to the
    // latency is now part of the answer. The slot, not the settings: a
    // fresh set carries no limiter, so nothing else turns it on.
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('t'), KeyModifiers::NONE); // Transport, dropped
    row_containing(&studio.rows(), "Add limiter to set");
    studio.press(KeyCode::Char('m'), KeyModifiers::NONE); // the row's mnemonic
    assert!(
        studio.status().starts_with("limiter added \u{b7}"),
        "setup: the limiter is on: {}",
        studio.status()
    );
    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    studio.chord("ctrl+p");
    studio.settle();
    let rows = studio.rows();
    let cost = rows
        .iter()
        .find(|row| row.contains("│") && row.contains("out ") && row.contains("ms · "))
        .unwrap_or_else(|| {
            panic!(
                "the stream cost is on the audio-out tab with the limiter on:\n{}",
                rows.join("\n")
            )
        });
    assert!(
        cost.contains("limiter"),
        "the limiter's runway is part of the answer: {cost}"
    );
}
