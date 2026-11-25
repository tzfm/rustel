//! Check error-navigation shortcuts from every TUI surface and asynchronous
//! status delivery. Unit tests cover caret movement. Legacy terminals use
//! Shift-F4 because Shift-F3 can be mistaken for a cursor-position report.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, PanelKind, hermetic};

/// The checker answers on a later frame: the jump arms a background
/// check and says so until the report replaces it. Bounded like every
/// asynchronous wait in the suite.
fn wait_for_report(studio: &mut Hermetic) {
    for _ in 0..300 {
        if !studio.status().contains("checking for errors") {
            return;
        }
        studio.pump();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the checker never answered: {}", studio.status());
}

/// Whether the status reads like a jump report: the cursor readout, a
/// located diagnostic (line:column first), or an engine failure with no
/// source position. Strict enough that an unrelated status - a refusal,
/// a receipt - is never mistaken for one.
fn is_report(status: &str) -> bool {
    status.starts_with("no errors · cursor:")
        || status.ends_with("source location unavailable")
        || status.split_once(':').is_some_and(|(head, _)| {
            !head.is_empty() && head.len() <= 4 && head.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The chord this fixture's terminal resolves for the jump, probed by
/// behaviour rather than guessed - the Keybinds row carries a default
/// column beside the resolved chord, so only a press tells the truth.
/// The probe's own jump is the first report, so it runs after the score
/// is set and before any surface is opened.
fn probe_first_error_key(studio: &mut Hermetic) -> KeyCode {
    for code in [KeyCode::F(3), KeyCode::F(4)] {
        studio.press(code, KeyModifiers::SHIFT);
        wait_for_report(studio);
        if is_report(studio.status()) {
            return code;
        }
    }
    panic!("neither ⇧F3 nor ⇧F4 reached the jump: {}", studio.status());
}

/// Press the resolved jump chord and wait for its report.
fn jump(studio: &mut Hermetic, key: KeyCode) {
    studio.press(key, KeyModifiers::SHIFT);
    wait_for_report(studio);
}

/// The jump outranks the surfaces that hold the keys: the piano's
/// letters and the settings sheet's chords both let it through, and
/// both come down - the landing is always the editor.
#[test]
fn the_jump_escapes_the_piano_and_the_settings_sheet() {
    let mut studio = hermetic();
    studio.set_score("// ok\n$: s(\"bd\"");
    studio.settle();
    let key = probe_first_error_key(&mut studio);
    assert!(
        studio.status().starts_with("2:"),
        "setup: a diagnostic to jump to: {}",
        studio.status()
    );

    // Under the piano the letters belong to the instrument - the jump
    // is no letter, and it takes the instrument down with it.
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains("PIANO")),
        "setup: the piano is open:\n{}",
        studio.rows().join("\n")
    );
    jump(&mut studio, key);
    assert!(
        !studio.rows().iter().any(|row| row.contains("PIANO")),
        "the jump closed the piano:\n{}",
        studio.rows().join("\n")
    );
    assert_eq!(studio.focus(), None, "and the keys are the editor's");
    assert!(
        studio.status().starts_with("2:"),
        "the report survived the escape: {}",
        studio.status()
    );

    // The sheet owns its chords - but not this one.
    studio.chord("ctrl+o");
    studio.settle();
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Settings),
        "setup: the sheet is open"
    );
    jump(&mut studio, key);
    assert_eq!(studio.focus(), None, "the jump closed the sheet");
    assert!(
        studio.status().starts_with("2:"),
        "and still reported: {}",
        studio.status()
    );
}

/// Static checking cannot see every failure: a current engine refusal
/// owns the navigation even when the lint is clean - it never says
/// `no errors`, opens the score at its start and says why. Editing the
/// score ends the refusal's ownership and the locator is clean again.
#[test]
fn an_engine_failure_owns_the_jump_until_the_score_is_edited() {
    let mut studio = hermetic();
    studio.set_score("// ok\nnull.x");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(
        studio.errors().as_deref(),
        Some("cannot read property 'x' of null"),
        "setup: the engine's refusal is on show"
    );

    // Park the caret on the second line so the jump has somewhere to
    // come back from - the failure carries no source position, so the
    // score opens at its start and the status says exactly that.
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.settle();
    let key = probe_first_error_key(&mut studio);
    assert_eq!(
        studio.status(),
        "cannot read property 'x' of null · source location unavailable"
    );

    // The edited revision no longer matches the failure: the locator
    // belongs to the clean score again.
    studio.set_score("$: s(\"bd\")");
    studio.settle();
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.settle();
    jump(&mut studio, key);
    assert_eq!(studio.status(), "no errors · cursor: 1:1");
    assert_eq!(
        studio.errors(),
        None,
        "and the slot cleared with the repair"
    );
}
