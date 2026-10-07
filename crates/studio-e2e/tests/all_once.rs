//! `all(fn)` does not repeat its transform within one source evaluation.

// The silent output requires the audio-callback allocation guard.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use rustel_studio_e2e::hermetic;

#[test]
fn all_transform_does_not_repeat_within_an_evaluation() {
    let mut studio = hermetic();

    studio.chord("ctrl+a");
    studio.type_text("let n = 0");
    studio.chord("enter");
    studio.type_text("all(p => { if (++n > 1) throw new Error(\"all ran twice\"); return p })");
    studio.chord("enter");
    studio.type_text("$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();

    // A repeated transform call in this evaluation must refuse the update.
    // The save ack and the install status race for the status line, so
    // accept either. A refused update never leaves a saved score sounding.
    let status = studio.status().to_owned();
    assert!(
        status == "playing from the top" || status.starts_with("saved "),
        "the update was refused or never installed: {status}"
    );
    assert!(studio.is_playing(), "the score is sounding");
}
