//! An edit that adds no sound must not change the loudness: typing a bare
//! label or a bare number under a playing set keeps the level it found.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic, loud_wav_bytes};

/// Walk the sheet's pages with Tab, then the page's rows, until the given
/// row is selected.
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

/// A playing studio over the probe's score, with auto evaluate on and the
/// typing pause at its shortest. The one bank it names holds an audible
/// sample.
fn playing_auto_studio(score: &str) -> Hermetic {
    let mut studio = hermetic();
    let bank = studio.set_directory().join("probe");
    std::fs::create_dir_all(&bank).expect("bank folder");
    std::fs::write(bank.join("hit.wav"), loud_wav_bytes()).expect("sample");
    studio.paste(&bank.display().to_string());
    studio.pump_until(Duration::from_secs(30), "the folder imported", |studio| {
        studio.status().contains("sound(s) imported")
    });
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    studio.set_score(score);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(studio.is_playing(), "the set plays");

    // The sheet opens on the page it last showed, so page with Tab until
    // the rows the walk names are up.
    studio.chord("ctrl+o");
    for _ in 0..10 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("auto evaluate"))
        {
            break;
        }
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
        studio.settle();
    }
    select_row(&mut studio, "▸ auto evaluate");
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    select_row(&mut studio, "▸ typing pause");
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio
}

/// The loudest master sample over a wall-clock window, the meter's own
/// reading. The pause of the apply lands inside the window: the loop pumps
/// past it.
fn loudest(studio: &mut Hermetic, seconds: u64) -> f32 {
    let started = Instant::now();
    let mut peak = 0.0_f32;
    while started.elapsed() < Duration::from_secs(seconds) {
        studio.pump();
        peak = peak.max(studio.master_peak());
        std::thread::sleep(Duration::from_millis(3));
    }
    peak
}

/// Typing a bare `$:` or a bare number under a playing set must not make
/// it louder. The lanes carry the reverb and the gains of the report, at
/// levels the meter reads under its ceiling.
#[test]
fn a_bare_label_or_number_keeps_the_loudness() {
    let score = concat!(
        "$: n(\"0 2 4 7\").scale(\"D4:minor\").s(\"probe\").fast(2).clip(.5).room(1)",
        ".gain(.06)\n\n",
        "$: s(\"probe ~ ~ probe ~ ~ probe ~, ~ ~ probe ~\").gain(.1)\n",
        "$: s(\"probe*8\").gain(.05)\n",
    );
    let mut studio = playing_auto_studio(score);
    let before = loudest(&mut studio, 4);
    eprintln!("probe: before {before}");

    for (name, typed) in [("label", "\n$:"), ("number", "\n4")] {
        studio.type_text(typed);
        let after = loudest(&mut studio, 4);
        eprintln!("probe: {name} {after}");
        assert!(before > 0.01, "the set must sound first: peak {before}");
        assert!(
            after <= before * 1.3,
            "a bare {name} raised the level: {before} -> {after}"
        );
    }
}
