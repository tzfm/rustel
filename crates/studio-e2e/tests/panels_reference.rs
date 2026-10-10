//! The reference column: Ctrl+D's lookup, the browser, and the way back out.
//!
//! What breaks if these fail: a musician cannot ask the score what its
//! words mean, or the column eats keys the score was owed. State and
//! status assertions carry these tests; the frame is asserted only where
//! the drawing *is* the contract (the search line, the tab titles).

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_sized, row_containing};

/// No terminal size hides a tab. The labels are measured from one table and
/// drawn from the geometry the hit-test reads. From the widest studio to
/// the narrowest panel, every tab is on screen and Tab reaches each tab.
#[test]
fn no_width_loses_a_tab_in_silence() {
    for width in [150u16, 120, 100, 80, 60, 40] {
        let mut studio = hermetic_sized(width, 40);
        studio.press(KeyCode::Home, KeyModifiers::NONE);
        studio.chord("ctrl+d");

        // Walk every tab with Tab itself: each must light up, whatever
        // the strip had to do to fit them.
        let labels = [
            ("reference", ["reference", "ref", "ref"]),
            ("samples", ["samples", "samp", "smp"]),
            ("chords", ["chords", "chord", "chd"]),
            ("scales", ["scales", "scale", "scl"]),
            ("generator", ["generator", "gen", "gen"]),
            ("examples", ["examples", "examp", "ex"]),
        ];
        for (tab, spellings) in labels {
            assert_eq!(studio.reference_tab(), Some(tab), "width {width}");
            let rows = studio.rows();
            assert!(
                rows.iter()
                    .any(|row| row.split_whitespace().any(|word| spellings.contains(&word))),
                "the selected {tab} tab is labelled at width {width}:\n{}",
                rows.join("\n")
            );
            studio.press(KeyCode::Tab, KeyModifiers::NONE);
        }
        assert_eq!(
            studio.reference_tab(),
            Some("reference"),
            "the six tabs wrap"
        );
        studio.press(KeyCode::Esc, KeyModifiers::NONE);
    }
}

/// A narrow strip seats the tab being shown. Below ~28 columns the panel
/// cannot seat every label at once, and it used to seat the leftmost and
/// stop: a reader who cycled onto a tab behind the edge was looking at
/// its contents with nothing marked anywhere on the row. Now the window
/// slides, `+N` counts what went behind the edge, and the shown tab's own
/// short label is always on the row.
#[test]
fn a_narrow_strip_keeps_the_shown_tab_seated() {
    // 26 columns: the narrowest terminal the layout still opens the
    // reference at, and too narrow for the tightest tier to seat all six
    // labels at once - the strip has to slide.
    let mut studio = hermetic_sized(26, 40);
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");

    // The tightest tier's labels are what a strip this narrow draws.
    let labels = [
        ("reference", "ref"),
        ("samples", "smp"),
        ("chords", "chd"),
        ("scales", "scl"),
        ("generator", "gen"),
        ("examples", "ex"),
    ];
    for (tab, label) in labels {
        let shown = studio
            .reference_tab()
            .expect("the reference column is open even at 26 columns");
        assert_eq!(shown, tab, "setup: the walk reached {tab}");
        let rows = studio.rows();
        let header = rows
            .iter()
            .find(|row| row.contains(label))
            .unwrap_or_else(|| {
                panic!(
                    "the shown tab {shown} has its {label} label seated:\n{}",
                    rows.join("\n")
                )
            });
        assert!(
            header.contains(label),
            "the strip seats the tab being shown: {shown} - {header:?}"
        );
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// Ctrl+D on a documented name opens that name's entry, with the column
/// focused - the whole point of the chord.
#[test]
fn ctrl_d_opens_the_entry_for_the_word_under_the_caret() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\")");

    // The caret just past `s` - the word under the caret is the function,
    // which the reference documents; `bd` is a sample name it does not.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    for _ in 0..6 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+d");

    assert_eq!(studio.focus(), Some(PanelKind::Reference));
    assert_eq!(studio.reference_tab(), Some("reference"));
    assert_eq!(studio.status(), "reference - s");
    row_containing(&studio.rows(), "reference");
    row_containing(&studio.rows(), "s(sound)");
}

/// Ctrl+D on a name the reference has no entry for opens the browser on
/// the nearest real names, and choosing one replaces the word that opened
/// it - the anchor is the misspelt word, not the caret.
#[test]
fn ctrl_d_on_an_unknown_word_browses_and_enter_replaces_it() {
    let mut studio = hermetic();
    studio.set_score("$: zzzz");

    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    assert!(
        studio.status().starts_with("no entry for \"zzzz\""),
        "the miss is said, not hidden: {}",
        studio.status()
    );

    // `zzzz` matches nothing, so the list is empty - the box already holds
    // the misspelt word, and a musician answers the miss by clearing it and
    // typing the name they meant.
    for _ in 0..4 {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.type_text("gain");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(studio.score(), "$: gain()", "the chosen call replaced zzzz");
    assert_eq!(
        studio.reference_tab(),
        Some("reference"),
        "parameters stay visible"
    );
    row_containing(&studio.rows(), "gain(");
    studio.type_text("0.4");
    assert_eq!(
        studio.score(),
        "$: gain(0.4)",
        "the caret is inside the call"
    );
}

/// A searched function inserts a balanced call and keeps its parameters visible.
#[test]
fn browse_searches_and_enter_inserts_the_call_with_its_parameters() {
    let mut studio = hermetic();
    studio.set_score("$: ");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.type_text("chor");
    row_containing(&studio.rows(), "search: chor");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.score(), "$: chord()");
    assert_eq!(studio.reference_tab(), Some("reference"));
    row_containing(&studio.rows(), "chord(");
    studio.type_text("\"C\"");
    assert_eq!(studio.score(), "$: chord(\"C\")");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), None);
}

/// Esc from the bare browser closes the column outright.
#[test]
fn esc_from_browse_closes_the_column() {
    let mut studio = hermetic();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    assert_eq!(studio.reference_tab(), None);
    assert_eq!(studio.status(), "reference closed");
}

/// OSC off hides squiz from the browser and the suggestions. `tag:osc` and
/// Ctrl+D still reach the entry.
#[test]
fn the_reference_setting_hides_osc_entries() {
    let mut studio = hermetic();
    let close_column = |studio: &mut rustel_studio_e2e::Hermetic| {
        for _ in 0..3 {
            if studio.reference_tab().is_none() {
                break;
            }
            studio.press(KeyCode::Esc, KeyModifiers::NONE);
        }
        assert_eq!(studio.reference_tab(), None);
    };
    let lists_squiz = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio
            .rows()
            .iter()
            .any(|row| row.contains("squiz") && row.contains("SuperDirt (OSC)"))
    };
    let browse_for_squiz = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio.set_score("$: ");
        studio.press(KeyCode::End, KeyModifiers::NONE);
        studio.chord("ctrl+d");
        studio.type_text("squiz");
    };

    browse_for_squiz(&mut studio);
    assert!(lists_squiz(&mut studio), "listed out of the box");
    close_column(&mut studio);

    // Settings ▸ Reference is the sixth tab. Its first row is OSC.
    studio.chord("ctrl+o");
    for _ in 0..5 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    browse_for_squiz(&mut studio);
    assert!(
        !lists_squiz(&mut studio),
        "switched off, the browser leaves it out"
    );
    for _ in 0.."squiz".len() {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    row_containing(&studio.rows(), "hidden · type tag: to narrow");
    studio.type_text("tag:osc squiz");
    assert!(lists_squiz(&mut studio), "tag:osc lists squiz anyway");
    close_column(&mut studio);

    studio.set_score("$: s(\"bd\").squ");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.chord("ctrl+f");
    assert!(!lists_squiz(&mut studio), "no suggestion offers squiz");
    close_column(&mut studio);

    studio.set_score("$: squiz(2)");
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..5 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
    }
    studio.chord("ctrl+d");
    assert_eq!(
        studio.status(),
        "reference - squiz",
        "a hidden name still opens"
    );
}

/// Tab walks the tabs - reference, samples, chords, scales - and Esc from
/// any of them closes the column. (With the `hydra` feature Generator and Examples
/// follow scales; this walk only pins the four every build has.)
#[test]
fn tab_walks_the_tabs() {
    let mut studio = hermetic();

    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    assert_eq!(studio.reference_tab(), Some("reference"));

    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("chords"));
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("scales"));

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), None, "Esc closes from any tab");
}

/// An open entry has no search box: its letters belong to the score.
/// Reading an entry and typing what it teaches is the point.
#[test]
fn letters_fall_through_to_the_score_in_entry_mode() {
    let mut studio = hermetic();
    studio.set_score("$: s(\"bd\")");

    studio.press(KeyCode::End, KeyModifiers::NONE);
    for _ in 0..6 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+d");
    assert_eq!(studio.reference_tab(), Some("reference"));

    // End is not the column's key: it moves the caret in the score.
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.type_text("x");

    assert_eq!(
        studio.score(),
        "$: s(\"bd\")x",
        "the letter reached the score while the entry was open"
    );
    assert_eq!(studio.reference_tab(), Some("reference"), "still open");
}
