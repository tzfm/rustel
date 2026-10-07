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
use rustel_studio_e2e::{hermetic, hermetic_with_local_bank, row_containing};

/// The catalogue the library owns reaches the samples tab: the local bank
/// is listed by name once the tab has read the library in.
#[test]
fn the_samples_tab_lists_the_librarys_banks() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();

    // Unfiltered, the tree starts folded - nine categories rather than a
    // thousand banks - so the search is how a name is shown.
    studio.type_text("testkick");
    studio.settle();

    let rows = studio.rows();
    assert_eq!(studio.reference_tab(), Some("samples"));
    row_containing(&rows, "testkick");
}

/// The search box filters the catalogue live, and a miss lists nothing.
#[test]
fn the_search_box_filters_the_catalogue() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();

    studio.type_text("test");
    studio.settle();
    let rows = studio.rows();
    row_containing(&rows, "search: test");
    row_containing(&rows, "testkick");
    // The filter is said in numbers against the whole catalogue - the
    // same total the unfiltered tab showed, whatever the catalogue has
    // grown to - and the banks this score brought are tagged as their
    // own: `testkick`, and the `bd` the fixture hands every studio.
    let total = |rows: &[String]| -> String {
        let row = rows
            .iter()
            .find(|row| row.contains(" sounds · "))
            .expect("the catalogue says its size")
            .clone();
        let start = row.find(" of ").expect("matched against the whole") + 4;
        row[start..]
            .split(" sounds")
            .next()
            .expect("the whole catalogue's size")
            .to_owned()
    };
    let whole = total(&studio.rows());
    row_containing(&rows, &format!("of {whole} sounds"));
    row_containing(&rows, "2 from this score");

    // Widening the query narrows nothing else away - the same bank, still
    // the only answer, whatever the query's tail adds. (Whether a query
    // *can* have no answer is the fuzzy matcher's contract, unit-tested in
    // `fuzzy.rs`; the tab shows whatever the matcher returns.)
    studio.type_text("kick");
    studio.settle();
    row_containing(&studio.rows(), "testkick");
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

/// Opened from inside `s("")` - Ctrl+Space - Enter puts the name in the
/// score where the caret stands.
#[test]
fn enter_inserts_a_sound_where_the_caret_stands() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: s(\"\")");
    // The caret goes between the quotes: two Lefts from the line's end.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.wait_for_catalogue();

    studio.type_text("testkick");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: s(\"testkick\")",
        "the chosen sound landed in the string"
    );
    assert_eq!(studio.reference_tab(), None, "the column closed behind it");
}

/// Space plays the sound under the cursor and says so - a bank's first
/// variant, written the way a score writes it - and a second Space on the
/// same row inside the stop window silences it.
#[test]
fn space_previews_and_space_again_stops() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("testkick");
    studio.settle();

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview testkick:0");

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview stopped");
}

/// Esc leaves the samples browser outright, however deep the reader is -
/// and closing silences a preview that is still sounding: one press, not
/// one to stop the sound and another to leave.
#[test]
fn esc_first_silences_a_live_preview() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("testkick");
    studio.settle();
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(studio.status(), "preview testkick:0");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), None, "Esc leaves the browser");
    assert_eq!(studio.status(), "reference closed");
}

/// The browser's own keys: Alt+A walks auto-play, and plain letters stay
/// in the search box - typing "cla" must never toggle auto-play mid-word.
#[test]
fn letters_type_and_alt_keys_command_the_browser() {
    let mut studio = hermetic_with_local_bank();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();

    studio.type_text("a");
    studio.settle();
    row_containing(&studio.rows(), "search: a");

    studio.chord("alt+a");
    assert_eq!(studio.status(), "samples - auto-play on: moving previews");
    studio.chord("alt+a");
    assert_eq!(studio.status(), "samples - auto-play off");

    // The query was untouched by the chords: the box still reads `a`.
    row_containing(&studio.rows(), "search: a");
}

#[test]
fn alt_d_deletes_an_imported_sample_only_after_enter() {
    let mut studio = hermetic();
    let imported = tempfile::tempdir().expect("import folder");
    let bank = imported.path().join("deletable");
    std::fs::create_dir(&bank).expect("bank folder");
    let path = bank.join("hit.wav");
    std::fs::write(&path, rustel_studio_e2e::wav_bytes()).expect("sample");
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
    studio.type_text("deletable");
    studio.settle();
    row_containing(&studio.rows(), "deletable");
    let score = studio.score();

    studio.chord("alt+d");
    assert!(path.exists(), "requesting deletion keeps the file");
    assert!(studio.status().contains("Enter"), "{}", studio.status());
    row_containing(&studio.rows(), "Delete permanently?");
    row_containing(&studio.rows(), "hit.wav");
    row_containing(&studio.rows(), "Enter deletes");
    row_containing(&studio.rows(), "Esc cancels");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("samples"));
    assert!(path.exists(), "Esc cancels deletion");

    studio.chord("alt+d");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.pump_until(
        std::time::Duration::from_secs(5),
        "the deleted sample disappeared from the browser",
        |studio| {
            studio
                .rows()
                .iter()
                .any(|row| row.contains("no sample banks named \"deletable\""))
        },
    );
    assert!(!path.exists(), "Enter deletes the selected local file");
    assert!(
        studio
            .samples_catalogue_names()
            .iter()
            .all(|name| name != "deletable")
    );
    row_containing(&studio.rows(), "no sample banks named \"deletable\"");
    assert_eq!(studio.reference_tab(), Some("samples"));
    assert_eq!(
        studio.score(),
        score,
        "confirmation does not insert a sound"
    );
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
        rows.iter().any(|row| row.contains("bracket match")),
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

/// A studio with no local bank still lists the inline default catalogue -
/// and no row it cannot play. The search stays live either way.
#[test]
fn without_a_local_bank_no_invented_row_appears() {
    let mut studio = hermetic();
    assert!(
        studio
            .samples_catalogue_names()
            .iter()
            .all(|name| name != "testkick")
    );

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("kick");
    studio.settle();

    let rows = studio.rows();
    row_containing(&rows, "search: kick");
    assert!(
        !rows.iter().any(|row| row.contains("testkick")),
        "nothing was invented:\n{}",
        rows.join("\n")
    );
}
