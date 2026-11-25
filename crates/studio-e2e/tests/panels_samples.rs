//! Check sample search, preview, insertion and copying with a generated local
//! WAV bank and the inline catalogue. No network access is needed.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic, hermetic_with_local_bank, row_containing};

/// Above the waveform and the volume, the tab says what the selected file
/// is, read from its header: its format, depth, rate and channels, then its
/// length and size. The list names the sound, so the details do not.
#[test]
fn the_selected_sounds_details_come_from_its_file() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("testkick");
    studio.settle();
    studio.pump_until(
        std::time::Duration::from_secs(5),
        "the details of the selected file",
        |studio| {
            studio
                .rows()
                .iter()
                .any(|row| row.contains("wav · 16-bit · 8 kHz · mono"))
        },
    );

    let rows = studio.rows();
    let row_of = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("no row holds {text:?}:\n{}", rows.join("\n")))
    };
    let what = row_of("wav · 16-bit · 8 kHz · mono");
    assert_eq!(row_of("1.00 s · 16 KB · "), what + 1);
    assert!(what < row_of("auto o"), "the details sit above the volume");
    assert!(!rows[what].contains("testkick") && !rows[what + 1].contains("testkick"));
}

/// A trim rewrites the selected file in place, under the same name: the
/// details read it again, and the length on show is the trimmed one.
#[test]
fn a_trim_shows_the_new_length_in_the_details() {
    let mut studio = hermetic();
    let imported = tempfile::tempdir().expect("import folder");
    let bank = imported.path().join("padded");
    std::fs::create_dir(&bank).expect("bank folder");
    // Half a second of silence each side of a second of a square wave.
    let samples: Vec<i16> = (0..16_000)
        .map(|frame| match frame {
            4_000..12_000 if frame % 16 < 8 => i16::MAX,
            4_000..12_000 => -i16::MAX,
            _ => 0,
        })
        .collect();
    std::fs::write(
        bank.join("hit.wav"),
        rustel_studio_e2e::pcm16_mono_wav(&samples),
    )
    .expect("sample");
    studio.paste(&imported.path().display().to_string());
    studio.settle();
    studio.pump_until(
        std::time::Duration::from_secs(5),
        "the local sample import settled",
        |studio| studio.status().contains("sound(s) imported"),
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("padded");
    studio.settle();
    let length_shown = |studio: &mut Hermetic, length: &str| {
        studio
            .rows()
            .iter()
            .any(|row| row.contains(&format!("{length} · ")))
    };
    studio.pump_until(
        std::time::Duration::from_secs(5),
        "the untrimmed length",
        |studio| length_shown(studio, "2.00 s"),
    );

    studio.chord("alt+t");
    studio.chord("alt+t");
    studio.pump_until(
        std::time::Duration::from_secs(10),
        "the trimmed length",
        // The second of sound, and the 10 ms the trim keeps on each side.
        |studio| !length_shown(studio, "2.00 s") && length_shown(studio, "1.02 s"),
    );
}

/// A search shows exactly the banks it matched - flat, no group rows to
/// walk - and Enter on a bank copies its name, the browser having been
/// opened to look things up.
#[test]
fn enter_copies_a_sound_when_the_column_opened_to_look_things_up() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("testkick");
    studio.settle();
    row_containing(&studio.rows(), "testkick");

    // Row zero is the bank itself; Enter copies and the panel stays open.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(studio.toast().as_deref(), Some("copied testkick"));
    assert_eq!(studio.reference_tab(), Some("samples"), "still browsing");
}

/// ^O is the settings sheet on the samples tab, as it is everywhere else.
/// It used to be taken by the browser to show a bank's folder, which hid
/// the settings chord behind a focused panel; that command is Alt+O now.
#[test]
fn ctrl_o_on_the_samples_tab_opens_the_settings_sheet() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    assert_eq!(studio.reference_tab(), Some("samples"));

    studio.chord("ctrl+o");
    let rows = studio.rows();
    assert!(
        rows.iter().any(|row| row.contains("launch on")),
        "the settings sheet opened:\n{}",
        rows.join("\n")
    );
    assert!(
        !studio.status().starts_with("opening") && !studio.status().starts_with("nowhere"),
        "nothing was revealed: {}",
        studio.status()
    );
}

/// The preview's own volume, on the keys the docs give it: `Alt++` up and
/// `Alt+-` down, three decibels a step, past unity on purpose - a browser
/// next to a playing set needs headroom. The unity detent means one step
/// down from +3 dB lands on 0.0 exactly.
#[test]
fn alt_plus_and_alt_minus_set_the_preview_volume() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();

    studio.chord("alt++");
    assert_eq!(
        studio.status(),
        "preview +3.0dB",
        "Alt++ raised the preview three decibels"
    );
    studio.chord("alt++");
    assert_eq!(studio.status(), "preview +6.0dB");
    studio.chord("alt+-");
    assert_eq!(studio.status(), "preview +3.0dB");
    studio.chord("alt+-");
    assert_eq!(
        studio.status(),
        "preview 0.0dB",
        "crossing zero lands exactly on the unity detent"
    );
    studio.chord("alt+-");
    assert_eq!(studio.status(), "preview -3.0dB");

    // The floor is the mute: down from -60 dB the gain is zero and the
    // label says so rather than a nonsense number.
    for _ in 0..20 {
        studio.chord("alt+-");
    }
    assert_eq!(
        studio.status(),
        "preview muted",
        "the floor is the mute, not -60.0dB"
    );
}
