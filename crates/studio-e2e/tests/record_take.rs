//! Check recording controls, clock and REC indicator. The hermetic output is
//! silent, so verify that silent recordings are discarded instead of saved.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::time::{Duration, Instant};

use rustel_studio_e2e::POLL;

/// How long the silent take in the discard test records for.
const SILENT_TAKE_LENGTH: Duration = Duration::from_secs(2);

/// How long the chip's clock gets to show a second going by.
const CLOCK_BUDGET: Duration = Duration::from_secs(15);

/// Ctrl+Shift+R starts a take: the status names the file, the header grows
/// the red `● REC` chip, and the writer is on disk holding the take's one
/// file open with the take still recording.
#[test]
fn ctrl_shift_r_starts_a_take_with_the_rec_chip() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.pump();
    let status = studio.status().to_owned();
    assert!(
        status.starts_with("● recording - ") && status.contains(".wav · s(\""),
        "the start names the take's file: {status}"
    );

    // The header's chip is the thing an audience sees; it says REC once
    // the engine has confirmed the take is open.
    studio.wait_for_rec_chip();

    // The chip is the engine's report that the take is open, and the writer
    // creates the take's file before that report. The file's length says
    // nothing about being finished: the writer streams chunks and refreshes
    // the header while it records.
    let wavs = studio.recording_wavs();
    assert_eq!(
        wavs.len(),
        1,
        "the open take is the one WAV on disk: {wavs:?}"
    );
    let file = wavs[0]
        .file_name()
        .expect("a take has a file name")
        .to_string_lossy();
    assert!(
        status.contains(file.as_ref()),
        "the WAV on disk is the take the start named: {file} in {status}"
    );
    studio.pump();
    assert!(
        studio.rows().iter().any(|row| row.contains("● REC")),
        "the take is still open:\n{}",
        studio.rows().join("\n")
    );

    // A take is not a score: closing the chip's studio now would be the
    // drop, so the suite leaves through the take's own door below.
    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.wait_for_take_close();
}

/// The second chord closes a silent take without keeping an empty WAV. A
/// third chord starts a fresh filename, so rejection cannot leave the writer
/// stuck on its previous path.
#[test]
fn closing_a_silent_take_discards_it_and_the_next_take_is_fresh() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.pump();
    let first = studio.status().to_owned();
    assert!(
        first.starts_with("● recording - ") && first.contains(".wav · s(\""),
        "the first take has a concrete path: {first}"
    );
    // A short silent take, the loop turning while it records.
    let recorded = Instant::now();
    while recorded.elapsed() < SILENT_TAKE_LENGTH {
        studio.pump();
        std::thread::sleep(POLL);
    }
    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.wait_for_take_close();
    assert_eq!(studio.status(), "take not saved - silence");
    assert!(
        studio.recording_wavs().is_empty(),
        "the rejected take leaves no WAV behind: {:?}",
        studio.recording_wavs()
    );

    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.pump();
    let second = studio.status().to_owned();
    assert!(
        second.starts_with("● recording - ") && second.contains(".wav · s(\""),
        "the next take has a concrete path: {second}"
    );
    assert_ne!(second, first, "a new take gets a fresh filename");
    studio.wait_for_rec_chip();
    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.wait_for_take_close();
    assert_eq!(studio.status(), "take not saved - silence");
    assert!(studio.recording_wavs().is_empty());
}

/// With nothing playing, the recording clock still advances, but closing the
/// take rejects the silent capture and leaves the recordings folder clean.
#[test]
fn a_take_over_silence_runs_the_clock_but_is_not_saved() {
    let mut studio = rustel_studio_e2e::hermetic();

    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.pump();
    // The chip's clock reads zero while nothing has passed, then grows:
    // the seconds cell appears once a second of the silent set has gone by.
    let mut saw_zero = false;
    let mut saw_growing = false;
    let watching = Instant::now();
    while watching.elapsed() < CLOCK_BUDGET {
        studio.pump();
        let rows = studio.rows();
        if let Some(row) = rows.iter().find(|row| row.contains("● REC")) {
            if row.contains("● REC 0:00") {
                saw_zero = true;
            } else {
                saw_growing = true;
            }
        }
        if saw_zero && saw_growing {
            break;
        }
        std::thread::sleep(POLL);
    }
    assert!(
        saw_growing,
        "the take's clock moves on the silent output; the REC row was there: {saw_zero}"
    );

    studio.press_until_taken(|studio| studio.chord("ctrl+shift+r"));
    studio.wait_for_take_close();
    assert_eq!(studio.status(), "take not saved - silence");
    assert!(
        studio.recording_wavs().is_empty(),
        "silence leaves no recording behind"
    );
}
