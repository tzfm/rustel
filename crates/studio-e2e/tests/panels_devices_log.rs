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

/// A warning nothing can resolve - no condition the studio will report
/// cleared - badges as news and then leaves the header on its own, without
/// the log ever being opened. The line stays in the log as history.
#[test]
fn an_unresolved_warning_leaves_the_badge_on_its_own() {
    let mut studio = hermetic();
    let note = studio.set_directory().join("note.txt");
    std::fs::write(&note, "not a sound").expect("note");

    studio.paste(&note.display().to_string());
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("⚠")),
        "the refusal badges: nobody has looked yet:\n{}",
        studio.rows().join("\n")
    );

    // Nobody opens the log. The keyless warning's stay passes and the
    // badge lets it go.
    studio.pump_until(
        std::time::Duration::from_secs(15),
        "the badge left the header on its own",
        |studio| !studio.rows().iter().any(|row| row.contains("⚠")),
    );

    // What left the header is still in the log.
    studio.chord("ctrl+shift+d");
    assert_eq!(studio.focus(), Some(PanelKind::Log));
    row_containing(
        &studio.rows(),
        "not audio, a folder of audio, a score, or a MIDI file",
    );
    let _ = std::fs::remove_file(note);
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
