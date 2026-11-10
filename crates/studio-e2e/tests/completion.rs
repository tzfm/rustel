//! Contextual reference and completion know what an argument can hold.
//!
//! What breaks if these fail: a chosen name replaces the wrong span of
//! the score, a call's list never appears, or a refusal is silent. The
//! status line and the score text are the contracts here.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, hermetic_with_local_bank, row_containing};

/// With the caret inside a word, the chosen name replaces the whole token:
/// `testk|x` becomes `testkick`, not `testktestkick`. The answer is the
/// fixture's local `testkick` bank, so the test waits for no manifest fetch.
#[test]
fn a_half_typed_word_is_replaced_whole() {
    let mut studio = hermetic_with_local_bank();

    let text = "$: s(\"testkx\")";
    studio.set_score(text);
    studio.set_caret(text.find('x').expect("the extra letter"));
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.wait_for_catalogue();

    // The query is what is typed before the caret; `testkick` answers it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: s(\"testkick\")",
        "the whole token went, not just the half before the caret"
    );
}

/// A call whose argument has not been opened yet - `chord()` - still
/// offers its list, and the chosen name lands quoted.
#[test]
fn a_call_with_no_string_still_offers_its_list() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: chord()");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("chords"));

    // Open major onto its roots, walk onto `C^`, take it.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: chord(\"C^\")",
        "the name arrived quoted, where the argument would start"
    );
}

/// Ctrl+D inside a completable string opens that call's own list: the
/// sounds, ranked closest first.
#[test]
fn ctrl_d_inside_a_string_opens_the_sounds() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: s(\"bd\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    // The caret now sits after the `d`: the query is the whole word.
    studio.chord("ctrl+d");

    assert_eq!(
        studio.status(),
        "the sounds - closest first; Enter replaces the word"
    );
    assert_eq!(studio.reference_tab(), Some("samples"), "the list opened");
    studio.wait_for_catalogue();
    studio.settle();
    row_containing(&studio.rows(), "search: bd");
}

/// An unfinished compound argument needs the call's parameters, without
/// replacing any of the text the musician is still writing.
#[test]
fn ctrl_d_inside_an_unfinished_argument_opens_its_parameters() {
    let mut studio = hermetic();
    let text = "$: s(\"sine\").distort(\"0.1:0.3:";
    studio.set_score(text);
    studio.set_caret(text.len());
    studio.chord("ctrl+d");

    assert_eq!(studio.focus(), Some(PanelKind::Reference));
    assert_eq!(studio.reference_tab(), Some("reference"));
    assert_eq!(studio.status(), "reference - distort");
    row_containing(&studio.rows(), "distort(distortion, volume, type)");
    row_containing(&studio.rows(), "type (number | string | Pattern)");
    assert_eq!(studio.score(), text);

    studio.press(KeyCode::PageDown, KeyModifiers::NONE);
    studio.press(KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(
        studio.score(),
        text,
        "reading parameters leaves the score intact"
    );
    studio.chord("ctrl+d");
    assert_eq!(studio.reference_tab(), None);
    assert_eq!(studio.focus(), None);
    studio.type_text("fold\")");
    assert_eq!(studio.score(), format!("{text}fold\")"));
}

#[test]
fn ctrl_d_before_a_quote_opens_the_function_arguments() {
    for name in ["s", "chord", "scale"] {
        let mut studio = hermetic_with_local_bank();
        let text = format!("$: {name}(");
        studio.set_score(&text);
        studio.set_caret(text.len());
        studio.chord("ctrl+d");

        assert_eq!(studio.reference_tab(), Some("reference"), "{name}");
        assert_eq!(studio.status(), format!("reference - {name}"));
        assert_eq!(studio.score(), text, "opening documentation is read-only");
    }
}

#[test]
fn ctrl_d_after_the_scale_argument_opens_inspires_arguments() {
    let mut studio = hermetic_with_local_bank();
    let text = "$: s(\"piano\").inspire(\"C:major:pentatonic\", ";
    studio.set_score(text);
    studio.set_caret(text.len());
    studio.chord("ctrl+d");

    assert_eq!(studio.reference_tab(), Some("reference"));
    assert_eq!(studio.status(), "reference - inspire");
    assert_eq!(studio.score(), text);
}

#[test]
fn ctrl_d_inside_inspires_scale_argument_opens_scales() {
    let mut studio = hermetic_with_local_bank();
    let text = "$: s(\"piano\").inspire(\"C:ma\")";
    studio.set_score(text);
    studio.set_caret(text.len() - 2);
    studio.chord("ctrl+d");

    assert_eq!(studio.reference_tab(), Some("scales"));
    assert_eq!(studio.score(), text);
}

#[test]
fn backspace_from_an_empty_contextual_search_returns_to_function_arguments() {
    for (name, tab) in [("s", "samples"), ("chord", "chords"), ("scale", "scales")] {
        let mut studio = hermetic_with_local_bank();
        let text = format!("$: {name}(\"\")");
        studio.set_score(&text);
        studio.set_caret(text.len() - 2);
        studio.chord("ctrl+d");
        assert_eq!(studio.reference_tab(), Some(tab), "{name}");

        studio.press(KeyCode::Backspace, KeyModifiers::NONE);

        assert_eq!(studio.reference_tab(), Some("reference"), "{name}");
        assert_eq!(studio.status(), format!("reference - {name}"));
        assert_eq!(studio.score(), text, "Backspace only changes the panel");
    }
}

/// A string whose call takes no list - a number, a pattern - refuses the
/// completion, and says why.
#[test]
fn a_string_nothing_can_complete_says_so() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: gain(\"\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.chord("ctrl+space");

    assert_eq!(studio.status(), "nothing to complete inside this string");
    assert_eq!(studio.reference_tab(), None);
}

/// A string naming something the score registers is the score's own:
/// there is nothing to complete.
#[test]
fn a_registered_name_is_the_scores_own() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: register(\"myfunction\", () => {})");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    for _ in 0..14 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+space");

    assert_eq!(
        studio.status(),
        "this string names something of your own - nothing to complete"
    );
    assert_eq!(studio.reference_tab(), None);
}

/// The panel open on one string and the caret moved into another: the
/// chord re-targets rather than closing - one press answers the new
/// string.
#[test]
fn the_completion_retargets_to_the_string_the_caret_is_in() {
    let mut studio = hermetic_with_local_bank();

    studio.set_score("$: s(\"bd\")\n$: s(\"hh\")");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    // Into the second string's token, after the second `h`: the query is
    // the whole word.
    for _ in 0..2 {
        studio.press(KeyCode::Left, KeyModifiers::NONE);
    }
    studio.chord("ctrl+space");
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.wait_for_catalogue();
    studio.settle();

    // The query is the new string's word.
    row_containing(&studio.rows(), "search: hh");

    // The panel owns the arrows, so the caret moves the way it does on a
    // keyboard-and-mouse stage: a click into the editor, mid-token. Back
    // in the first string, the chord re-targets the panel rather than
    // closing it.
    // The first score line sits three rows down under the menu bar and
    // the scene tabs; column 12 is mid-token in `s("bd")`.
    studio.click(12, 3);
    studio.chord("ctrl+space");
    studio.settle();
    row_containing(&studio.rows(), "search: bd");
}

#[test]
fn ctrl_d_uses_kabelsalat_documentation_inside_graphs() {
    for (text, name, signature) in [
        (
            "$: note(\"c3\").K(() => sine(sFreq).out())",
            "sine",
            "sine(freq, sync, phase)",
        ),
        (
            "$: note(\"c3\").K(() => sine(sFreq).mul(sGate.adsr(.01, .2, .5, .1)).out())",
            "adsr",
            "adsr(gate, att, dec, sus, rel)",
        ),
        (
            "$: note(\"c3\").K(() => { Node.prototype.m = function(x) { return this.add(x) }; sine(sFreq).m(sGate.adsr(.01, .2, .5, .1)).out() })",
            "adsr",
            "adsr(gate, att, dec, sus, rel)",
        ),
    ] {
        let mut studio = hermetic();
        studio.set_score(text);
        studio.set_caret(text.find(name).expect("graph function") + name.len() - 1);
        studio.chord("ctrl+d");

        assert_eq!(studio.reference_tab(), Some("reference"));
        assert_eq!(studio.status(), format!("reference - {name}"));
        row_containing(&studio.rows(), signature);
        assert_eq!(studio.score(), text, "reading help leaves the graph intact");
    }
}

#[test]
fn ctrl_d_keeps_pattern_documentation_outside_graphs_and_inside_s() {
    for (text, name, signature) in [
        ("$: sine.range(0, 12)", "sine", "sine()"),
        (
            "$: note(\"c3\").adsr(.01, .2, .5, .1)",
            "adsr",
            "adsr(time, time, gain, time)",
        ),
        (
            "$: note(\"c3\").K(() => sine(S(note(\"c3\").adsr(.01, .2, .5, .1))).out())",
            "adsr",
            "adsr(time, time, gain, time)",
        ),
        (
            "$: note(\"c3\").K(() => sine(pat[0]).out(), note(\"c3\").adsr(.01, .2, .5, .1))",
            "adsr",
            "adsr(time, time, gain, time)",
        ),
        (
            "$: note(\"c3\").worklet(\"sine(pat[0]).out()\", note(\"c3\").adsr(.01, .2, .5, .1))",
            "adsr",
            "adsr(time, time, gain, time)",
        ),
    ] {
        let mut studio = hermetic();
        studio.set_score(text);
        studio.set_caret(text.find(name).expect("pattern function") + name.len() - 1);
        studio.chord("ctrl+d");

        assert_eq!(studio.reference_tab(), Some("reference"));
        assert_eq!(studio.status(), format!("reference - {name}"));
        row_containing(&studio.rows(), signature);
        assert_eq!(
            studio.score(),
            text,
            "reading help leaves the pattern intact"
        );
    }
}

#[test]
fn kabelsalat_completion_inserts_values_without_call_parentheses() {
    for (typed, completed) in [("sFre", "sFreq"), ("sGat", "sGate")] {
        let mut studio = hermetic();
        let text = format!("$: note(\"c3\").K(() => sine({typed}).out())");
        studio.set_score(&text);
        studio.set_caret(text.find(typed).expect("graph value") + typed.len());
        studio.chord("ctrl+space");

        assert_eq!(studio.reference_tab(), Some("reference"));
        studio.press(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(studio.score(), text.replace(typed, completed));
    }
}

#[test]
fn kabelsalat_completion_distinguishes_oscillator_calls_from_pattern_signals() {
    for (text, occurrence, expected) in [
        (
            "$: note(\"c3\").K(() => sine)",
            0,
            "$: note(\"c3\").K(() => sine())",
        ),
        (
            "$: note(\"c3\").K(() => n(sFreq).sine)",
            0,
            "$: note(\"c3\").K(() => n(sFreq).sine())",
        ),
        ("$: n(sine)", 0, "$: n(sine)"),
        (
            "$: note(\"c3\").K(() => sine(S(sine)).out())",
            1,
            "$: note(\"c3\").K(() => sine(S(sine)).out())",
        ),
        (
            "$: note(\"c3\").K(() => sine(pat[0]).out(), sine.range(220, 440))",
            1,
            "$: note(\"c3\").K(() => sine(pat[0]).out(), sine.range(220, 440))",
        ),
        (
            "$: note(\"c3\").worklet(\"sine(pat[0]).out()\", sine.range(220, 440))",
            1,
            "$: note(\"c3\").worklet(\"sine(pat[0]).out()\", sine.range(220, 440))",
        ),
    ] {
        let mut studio = hermetic();
        studio.set_score(text);
        let start = text
            .match_indices("sine")
            .nth(occurrence)
            .expect("oscillator or pattern signal")
            .0;
        studio.set_caret(start + "sine".len());
        studio.chord("ctrl+space");

        assert_eq!(studio.reference_tab(), Some("reference"));
        studio.press(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(studio.score(), expected, "completion in {text}");
    }
}

#[test]
fn kabelsalat_completion_preserves_qualified_helpers_and_node_properties() {
    for (expression, typed, completed) in [
        ("Node.parseI", "Node.parseI", "Node.parseInput()"),
        ("Node.prototy", "Node.prototy", "Node.prototype"),
        ("nodeRegistry.ge", "nodeRegistry.ge", "nodeRegistry.get()"),
        ("sine(sFreq).clon", "clon", "clone()"),
        ("sine(sFreq).inputO", "inputO", "inputOf"),
    ] {
        let mut studio = hermetic();
        let text = format!("$: note(\"c3\").K(() => {expression})");
        studio.set_score(&text);
        studio.set_caret(text.find(typed).expect("graph helper") + typed.len());
        studio.chord("ctrl+space");

        assert_eq!(studio.reference_tab(), Some("reference"));
        studio.press(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(studio.score(), text.replace(typed, completed));
    }
}

#[test]
fn kabelsalat_node_properties_join_completed_calls_without_parentheses() {
    let mut studio = hermetic();
    let text = "$: note(\"c3\").K(() => sine(sFreq))";
    studio.set_score(text);
    studio.set_caret(text.rfind(')').expect("graph boundary"));
    studio.chord("ctrl+space");
    studio.type_text("inputOf");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        studio.score(),
        "$: note(\"c3\").K(() => sine(sFreq).inputOf)"
    );
}

#[test]
fn kabelsalat_completion_keeps_globals_and_node_methods_in_their_own_positions() {
    for (expression, typed, invalid) in [
        ("n(sFreq).sFre", "sFre", "sFreq"),
        ("n(sFreq).registerN", "registerN", "registerNode()"),
        ("clon", "clon", "clone()"),
    ] {
        let mut studio = hermetic();
        let text = format!("$: note(\"c3\").K(() => {expression})");
        studio.set_score(&text);
        studio.set_caret(text.rfind(typed).expect("graph name") + typed.len());
        studio.chord("ctrl+space");
        studio.press(KeyCode::Enter, KeyModifiers::NONE);

        let start = text.rfind(typed).expect("graph name");
        let mut invalid_score = text.clone();
        invalid_score.replace_range(start..start + typed.len(), invalid);
        assert_ne!(
            studio.score(),
            invalid_score,
            "{invalid} is unavailable at this graph position"
        );
    }
}

/// Hydra names and pattern names share spellings: `osc` and `shape` are
/// shader sources in a Hydra statement and controls in a pattern. Ctrl+D
/// reads each one in the language of its own statement.
#[test]
fn ctrl_d_reads_hydra_names_in_hydra_code_and_pattern_names_elsewhere() {
    let text = "await initHydra()\nosc(10).kaleid(4).out()\n$: s(\"bd\").shape(0.5)";
    for (name, signature) in [
        ("osc", "osc(frequency, sync, offset)"),
        ("kaleid", "kaleid(nSides)"),
        ("shape", "shape("),
    ] {
        let mut studio = hermetic();
        studio.set_score(text);
        studio.set_caret(text.find(name).expect("a name") + name.len() - 1);
        studio.chord("ctrl+d");

        assert_eq!(studio.reference_tab(), Some("reference"));
        assert_eq!(studio.status(), format!("reference - {name}"));
        let rows = studio.rows();
        row_containing(&rows, signature);
        assert!(
            !rows.iter().any(|row| row.contains("shape(sides")),
            "{name}: the pattern shape is not the Hydra shape"
        );
        assert_eq!(studio.score(), text);
    }
}

/// Completion after a dot in a Hydra chain offers Hydra methods and
/// inserts the chosen one as a call.
#[test]
fn hydra_completion_inserts_hydra_methods() {
    let mut studio = hermetic();
    let text = "await initHydra()\nosc(10).kalei";
    studio.set_score(text);
    studio.set_caret(text.len());
    studio.chord("ctrl+space");

    assert_eq!(studio.reference_tab(), Some("reference"));
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.score(), "await initHydra()\nosc(10).kaleid()");
}

/// The searchable list follows the language at the caret: Hydra names in
/// Hydra code, and never in pattern code.
#[test]
fn ctrl_f_lists_only_the_language_at_the_caret() {
    for (text, hydra) in [
        ("await initHydra()\nosc(10).out()\n", true),
        ("await initHydra()\nosc(10).out()\n$: s(\"bd\")\n", false),
    ] {
        let mut studio = hermetic();
        studio.set_score(text);
        studio.set_caret(text.len());
        studio.chord("ctrl+f");
        studio.type_text("kaleid");

        let rows = studio.rows();
        row_containing(&rows, "search: kaleid");
        assert_eq!(
            rows.iter()
                .any(|row| row.contains("kaleid") && !row.contains("search:")),
            hydra,
            "{text:?}: {rows:#?}"
        );
        assert_eq!(studio.score(), text, "searching leaves the score alone");
    }
}
