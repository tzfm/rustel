//! Check sample-recording refusal and its interaction with an active take.
//! The hermetic harness disables default input, so these tests never open a
//! microphone. Recorded WAVs, clocks and silent-input discard need live input
//! and are outside this suite; record_take covers recording from silent output.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic};

/// How long the engine may take to answer a sample request.
const SAMPLE_ANSWER_BUDGET: Duration = Duration::from_secs(4);

/// Turn the loop until the engine has answered a sample request: the press
/// says `● recording a sample…` at once, and only the engine's answer -
/// started or refused - moves the status on from it.
fn wait_for_sample_answer(studio: &mut Hermetic) {
    studio.pump_until(
        SAMPLE_ANSWER_BUDGET,
        "the engine answered the sample",
        |studio| studio.status() != "● recording a sample…",
    );
}

/// While a take is running, ^H is refused and the refusal names the chord
/// that finishes the take. The score keeps playing; the take keeps writing.
#[test]
fn a_running_take_refuses_a_sample_and_names_the_chord() {
    let mut studio = hermetic();

    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.pump();
    assert!(
        studio.status().starts_with("● recording - "),
        "the take started: {}",
        studio.status()
    );
    studio.wait_for_rec_chip();

    studio.press(KeyCode::Char('h'), KeyModifiers::CONTROL);
    let error = studio.errors().unwrap_or_default();
    assert!(
        error.contains("a take is recording - finish it"),
        "the refusal says what to finish: {error:?}"
    );
    assert!(
        error.contains("^⇧R") || error.contains("Ctrl+Shift+R"),
        "the refusal names the take's chord in this terminal's spelling: {error:?}"
    );

    // The take is still open: closing it is what the refusal asked for.
    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.wait_for_take_close();
}

/// ^H with no input to record from is refused, and the refusal leaves
/// nothing behind: the error line says why, the status stops claiming a
/// sample, the header shows no sample clock, the next ^H is a fresh start
/// rather than the finish of a phantom one, and no WAV is left in the
/// recordings folder.
#[test]
fn ctrl_h_with_no_input_is_refused_and_leaves_nothing_open() {
    let mut studio = hermetic();

    for attempt in 1..=2 {
        studio.press_until_taken(|studio| studio.press(KeyCode::Char('h'), KeyModifiers::CONTROL));
        // Said at once, before the engine has answered - and on the second
        // press too, because the refused first one left no take to finish.
        assert_eq!(
            studio.status(),
            "● recording a sample…",
            "press {attempt} asked for a fresh sample"
        );
        wait_for_sample_answer(&mut studio);
        assert_eq!(
            studio.status(),
            "no sample is recording",
            "press {attempt}: the status stopped claiming a sample"
        );
        assert_eq!(
            studio.errors().as_deref(),
            Some(
                "could not record a sample: no audio input to record from: \
                 this studio opens no default input"
            ),
            "press {attempt}: the error line says why"
        );
        assert!(
            !studio.rows().iter().any(|row| row.contains("REC SAMPLE")),
            "press {attempt}: no sample clock in the header"
        );
    }

    let wavs = studio.recording_wavs();
    assert!(
        wavs.is_empty(),
        "a refused sample leaves no WAV behind: {wavs:?}"
    );
}
